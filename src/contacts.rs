//! Sir's contacts (`contacts` in `jai_memory.db`): name, nickname, e-mail, phone number and
//! WhatsApp display name, so "send that to Max", "reply to Herr Müller" and "text +49 151 …"
//! all reach the right person. Older databases are upgraded in place (missing columns are added).
//! People who write to Sir on WhatsApp are added automatically (without a number until Sir gives
//! one); `priority` contacts always get through the focus auto-responder.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Contact {
    pub id: i64,
    pub name: String,
    pub nickname: String,
    pub email: String,
    /// International format ("+49151…"), empty when unknown.
    pub phone_number: String,
    /// The name WhatsApp shows for this person (notifications use it).
    pub whatsapp_name: String,
    pub notes: String,
    /// Urgent by definition (family …): the auto-responder never holds them back.
    pub priority: bool,
}

impl Contact {
    /// "Max (+49151…, WhatsApp "Maxi")".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.phone_number.is_empty() {
            parts.push(self.phone_number.clone());
        }
        if !self.email.is_empty() {
            parts.push(self.email.clone());
        }
        if !self.whatsapp_name.is_empty() && !self.whatsapp_name.eq_ignore_ascii_case(&self.name) {
            parts.push(format!("WhatsApp \"{}\"", self.whatsapp_name));
        }
        if !self.nickname.is_empty() {
            parts.push(format!("aka {}", self.nickname));
        }
        if self.priority {
            parts.push("priority".into());
        }
        if parts.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, parts.join(", "))
        }
    }
}

/// "0151 234 567" → "+49151234567" (with the default country code), "0049…" → "+49…".
pub fn normalize_phone(raw: &str, default_cc: &str) -> String {
    let trimmed = raw.trim();
    let plus = trimmed.starts_with('+');
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return String::new();
    }
    if plus {
        return format!("+{digits}");
    }
    if let Some(rest) = digits.strip_prefix("00") {
        return format!("+{rest}");
    }
    let cc: String = default_cc.chars().filter(char::is_ascii_digit).collect();
    match digits.strip_prefix('0') {
        Some(rest) if !cc.is_empty() => format!("+{cc}{rest}"),
        _ if digits.len() >= 11 => format!("+{digits}"),
        _ => digits,
    }
}

/// Whether a reference looks like a phone number rather than a name.
pub fn looks_like_phone(s: &str) -> bool {
    let digits = s.chars().filter(char::is_ascii_digit).count();
    digits >= 6 && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '/' | '.'))
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .replace('ß', "ss")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !matches!(*w, "herr" | "frau" | "mr" | "mrs" | "ms" | "dr" | "my" | "mein" | "meine" | "meinem" | "meiner"))
        .collect::<Vec<_>>()
        .join(" ")
}

const COLUMNS: &str = "id, name, nickname, email, phone_number, whatsapp_name, notes, priority";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Contact> {
    Ok(Contact {
        id: r.get(0)?,
        name: r.get(1)?,
        nickname: r.get(2)?,
        email: r.get(3)?,
        phone_number: r.get(4)?,
        whatsapp_name: r.get(5)?,
        notes: r.get(6)?,
        priority: r.get::<_, i64>(7)? != 0,
    })
}

#[derive(Clone)]
pub struct ContactBook {
    conn: Arc<Mutex<Connection>>,
}

impl ContactBook {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let conn = Connection::open(path.as_ref()).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        Self::init(conn).expect("in-memory contacts schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS contacts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL,
                 nickname TEXT NOT NULL DEFAULT '',
                 email TEXT NOT NULL DEFAULT '',
                 updated_at TEXT NOT NULL DEFAULT ''
             );",
        )
        .map_err(|e| e.to_string())?;
        // Upgrade older tables: phone number, WhatsApp name, notes and priority.
        let existing: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(contacts)").map_err(|e| e.to_string())?;
            let names = stmt.query_map([], |r| r.get::<_, String>(1)).map_err(|e| e.to_string())?;
            names.filter_map(Result::ok).collect()
        };
        for (column, ddl) in [
            ("phone_number", "ALTER TABLE contacts ADD COLUMN phone_number TEXT NOT NULL DEFAULT ''"),
            ("whatsapp_name", "ALTER TABLE contacts ADD COLUMN whatsapp_name TEXT NOT NULL DEFAULT ''"),
            ("notes", "ALTER TABLE contacts ADD COLUMN notes TEXT NOT NULL DEFAULT ''"),
            ("priority", "ALTER TABLE contacts ADD COLUMN priority INTEGER NOT NULL DEFAULT 0"),
            ("nickname", "ALTER TABLE contacts ADD COLUMN nickname TEXT NOT NULL DEFAULT ''"),
            ("email", "ALTER TABLE contacts ADD COLUMN email TEXT NOT NULL DEFAULT ''"),
            ("updated_at", "ALTER TABLE contacts ADD COLUMN updated_at TEXT NOT NULL DEFAULT ''"),
        ] {
            if !existing.iter().any(|c| c == column) {
                conn.execute_batch(ddl).map_err(|e| e.to_string())?;
                info!("[CONTACTS] Upgraded the contacts table: added '{column}'.");
            }
        }
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<Contact> {
        let db = self.db();
        let Ok(mut stmt) = db.prepare(&format!("SELECT {COLUMNS} FROM contacts ORDER BY priority DESC, name COLLATE NOCASE")) else { return Vec::new() };
        stmt.query_map([], row).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
    }

    pub fn get(&self, id: i64) -> Option<Contact> {
        self.db().query_row(&format!("SELECT {COLUMNS} FROM contacts WHERE id = ?1"), params![id], row).optional().ok().flatten()
    }

    /// Adds or updates (by name, WhatsApp name or number); empty fields keep their value.
    pub fn upsert(&self, c: &Contact, default_cc: &str) -> Result<Contact, String> {
        let name = c.name.trim();
        if name.is_empty() && c.phone_number.trim().is_empty() {
            return Err("A contact needs a name or a phone number.".into());
        }
        let phone = if c.phone_number.trim().is_empty() { String::new() } else { normalize_phone(&c.phone_number, default_cc) };
        let existing = self
            .find(if name.is_empty() { &phone } else { name })
            .into_iter()
            .find(|e| fold(&e.name) == fold(name) || (!phone.is_empty() && e.phone_number == phone) || (!c.whatsapp_name.is_empty() && e.whatsapp_name.eq_ignore_ascii_case(&c.whatsapp_name)));
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let db = self.db();
        let id = match existing {
            Some(e) => {
                let pick = |new: &str, old: &str| if new.trim().is_empty() { old.to_string() } else { new.trim().to_string() };
                db.execute(
                    "UPDATE contacts SET name = ?1, nickname = ?2, email = ?3, phone_number = ?4, whatsapp_name = ?5, notes = ?6, priority = ?7, updated_at = ?8 WHERE id = ?9",
                    params![
                        pick(name, &e.name),
                        pick(&c.nickname, &e.nickname),
                        pick(&c.email, &e.email),
                        pick(&phone, &e.phone_number),
                        pick(&c.whatsapp_name, &e.whatsapp_name),
                        pick(&c.notes, &e.notes),
                        (c.priority || e.priority) as i64,
                        now,
                        e.id
                    ],
                )
                .map_err(|e| e.to_string())?;
                e.id
            }
            None => {
                db.execute(
                    "INSERT INTO contacts (name, nickname, email, phone_number, whatsapp_name, notes, priority, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        if name.is_empty() { phone.clone() } else { name.to_string() },
                        c.nickname.trim(),
                        c.email.trim(),
                        phone,
                        c.whatsapp_name.trim(),
                        c.notes.trim(),
                        c.priority as i64,
                        now
                    ],
                )
                .map_err(|e| e.to_string())?;
                db.last_insert_rowid()
            }
        };
        drop(db);
        self.get(id).ok_or_else(|| "The contact could not be read back.".to_string())
    }

    /// Someone wrote on WhatsApp: known by that name from now on.
    pub fn learn_whatsapp_sender(&self, whatsapp_name: &str, phone: Option<&str>, default_cc: &str) {
        let n = whatsapp_name.trim();
        if n.is_empty() || n.eq_ignore_ascii_case("you") || n.eq_ignore_ascii_case("du") {
            return;
        }
        let known = self.find(n).into_iter().any(|c| c.whatsapp_name.eq_ignore_ascii_case(n) || fold(&c.name) == fold(n));
        if known && phone.is_none() {
            return;
        }
        let contact = Contact { name: n.to_string(), whatsapp_name: n.to_string(), phone_number: phone.unwrap_or("").to_string(), ..Contact::default() };
        if let Err(e) = self.upsert(&contact, default_cc) {
            warn!("[CONTACTS] Could not learn WhatsApp sender '{n}': {e}");
        }
    }

    pub fn delete(&self, name: &str) -> Result<String, String> {
        let target = self.resolve(name)?;
        self.db().execute("DELETE FROM contacts WHERE id = ?1", params![target.id]).map_err(|e| e.to_string())?;
        Ok(format!("Deleted the contact {}.", target.name))
    }

    /// Contacts matching a name, nickname, WhatsApp name, e-mail or number, best first.
    pub fn find(&self, query: &str) -> Vec<Contact> {
        let q = fold(query);
        let digits: String = query.chars().filter(char::is_ascii_digit).collect();
        if q.is_empty() && digits.len() < 6 {
            return Vec::new();
        }
        let mut scored: Vec<(f64, Contact)> = self
            .list()
            .into_iter()
            .filter_map(|c| {
                let phone_digits: String = c.phone_number.chars().filter(char::is_ascii_digit).collect();
                if digits.len() >= 6 && !phone_digits.is_empty() && (phone_digits.ends_with(&digits) || digits.ends_with(&phone_digits[phone_digits.len().saturating_sub(9)..])) {
                    return Some((1.0, c));
                }
                let fields = [fold(&c.name), fold(&c.nickname), fold(&c.whatsapp_name), c.email.to_lowercase()];
                let score = fields
                    .iter()
                    .filter(|f| !f.is_empty())
                    .map(|f| {
                        if *f == q {
                            1.0
                        } else if f.split_whitespace().any(|w| w == q) || (q.len() >= 3 && f.starts_with(&q)) {
                            0.95
                        } else {
                            strsim::jaro_winkler(f, &q)
                        }
                    })
                    .fold(0.0, f64::max);
                (score >= 0.86).then_some((score, c))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    /// The one contact a reference means.
    pub fn resolve(&self, query: &str) -> Result<Contact, String> {
        let found = self.find(query);
        match found.as_slice() {
            [] => Err(format!("I have no contact called '{}'.", query.trim())),
            [one] => Ok(one.clone()),
            [first, second, ..] => {
                let q = fold(query);
                if fold(&first.name) == q || first.whatsapp_name.eq_ignore_ascii_case(query.trim()) || fold(&second.name) != q {
                    Ok(first.clone())
                } else {
                    Err(format!("Several contacts match '{}': {} and {}.", query.trim(), first.name, second.name))
                }
            }
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "manage_contacts",
        "description": "Sir's contacts (name, nickname, e-mail, phone number, WhatsApp name, priority = always gets through the auto-responder): add or update one (\"save Max's number +49 151 …\"), find, list or delete.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["add", "update", "find", "list", "delete"]},
            "name": {"type": "string"},
            "nickname": {"type": "string"},
            "email": {"type": "string"},
            "phone_number": {"type": "string"},
            "whatsapp_name": {"type": "string"},
            "priority": {"type": "boolean"},
            "notes": {"type": "string"}
        }, "required": ["action"]}
    }})
}

pub fn tool(book: &ContactBook, args: &Value, default_cc: &str) -> Result<String, String> {
    let s = |k: &str| crate::tools::str_arg(args, &[k]).unwrap_or("").to_string();
    match s("action").to_lowercase().as_str() {
        "add" | "update" | "save" | "edit" => {
            let c = Contact {
                name: s("name"),
                nickname: s("nickname"),
                email: s("email"),
                phone_number: s("phone_number"),
                whatsapp_name: s("whatsapp_name"),
                notes: s("notes"),
                priority: crate::tools::bool_arg(args, "priority").unwrap_or(false),
                ..Contact::default()
            };
            book.upsert(&c, default_cc).map(|c| format!("Saved contact: {}.", c.describe()))
        }
        "find" | "search" | "get" => {
            let q = s("name");
            let q = if q.is_empty() { s("phone_number") } else { q };
            let found = book.find(&q);
            if found.is_empty() {
                Err(format!("No contact matches '{q}'."))
            } else {
                Ok(found.iter().take(5).map(|c| format!("- {}", c.describe())).collect::<Vec<_>>().join("\n"))
            }
        }
        "delete" | "remove" => book.delete(&s("name")),
        _ => {
            let all = book.list();
            if all.is_empty() {
                Ok("No contacts saved yet.".into())
            } else {
                Ok(format!("{} contacts:\n{}", all.len(), all.iter().take(40).map(|c| format!("- {}", c.describe())).collect::<Vec<_>>().join("\n")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_numbers_normalise() {
        assert_eq!(normalize_phone("0151 234 5678", "+49"), "+491512345678");
        assert_eq!(normalize_phone("0049 151 2345678", "+49"), "+491512345678");
        assert_eq!(normalize_phone("+44 (20) 7946-0958", "+49"), "+442079460958");
        assert!(looks_like_phone("+49 151 2345678") && !looks_like_phone("Max"));
    }

    #[test]
    fn contacts_resolve_by_name_nickname_whatsapp_and_number() {
        let book = ContactBook::in_memory();
        book.upsert(&Contact { name: "Max Mustermann".into(), nickname: "Maxi".into(), phone_number: "0151 1234567".into(), ..Contact::default() }, "+49").unwrap();
        book.upsert(&Contact { name: "Herr Müller".into(), email: "mueller@school.de".into(), ..Contact::default() }, "+49").unwrap();
        assert_eq!(book.resolve("Max").unwrap().name, "Max Mustermann");
        assert_eq!(book.resolve("maxi").unwrap().phone_number, "+491511234567");
        assert_eq!(book.resolve("Müller").unwrap().email, "mueller@school.de");
        assert_eq!(book.resolve("151 1234567").unwrap().name, "Max Mustermann");
        assert!(book.resolve("Anna").is_err());
        // Updating keeps fields that were not given.
        book.upsert(&Contact { name: "Max Mustermann".into(), whatsapp_name: "Max \u{1F3AE}".into(), ..Contact::default() }, "+49").unwrap();
        let max = book.resolve("Max").unwrap();
        assert_eq!(max.phone_number, "+491511234567");
        book.learn_whatsapp_sender("Lena", None, "+49");
        assert_eq!(book.resolve("Lena").unwrap().whatsapp_name, "Lena");
    }
}
