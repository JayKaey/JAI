//! The Finance Tracker: "I spent 12 euros on pizza", "put 50 towards the new GPU", "how much have I
//! spent this month?" Income, expenses and savings goals in SQLite, with a progress card window.

use crate::features::{bar, num_arg, show_window, spoken, str_arg};
use crate::tools::ToolContext;
use chrono::Datelike;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    pub name: String,
    pub target: f64,
    pub saved: f64,
    pub deadline: Option<String>,
}

#[derive(Clone)]
pub struct Finance {
    conn: Arc<Mutex<Connection>>,
}

/// A category from free text ("pizza" → food).
pub fn categorise(raw: &str) -> String {
    let t = raw.to_lowercase();
    const MAP: &[(&str, &[&str])] = &[
        ("food", &["pizza", "food", "lunch", "dinner", "breakfast", "döner", "doner", "burger", "mcdonald", "restaurant", "snack", "groceries", "essen", "supermarkt", "rewe", "edeka", "lidl", "aldi", "kebab", "coffee", "café", "cafe"]),
        ("games", &["game", "steam", "playstation", "xbox", "nintendo", "minecraft", "skin", "v-bucks", "battle pass", "spiel"]),
        ("tech", &["gpu", "pc", "ram", "ssd", "keyboard", "mouse", "headset", "monitor", "cable", "phone", "laptop", "rtx", "cpu", "tech"]),
        ("transport", &["bus", "train", "ticket", "fuel", "petrol", "benzin", "tanken", "motorcycle", "moped", "bahn", "deutschlandticket", "uber", "taxi"]),
        ("subscriptions", &["spotify", "netflix", "subscription", "abo", "disney", "youtube premium", "chatgpt", "claude", "icloud"]),
        ("clothes", &["clothes", "shoes", "jacket", "shirt", "hoodie", "kleidung", "schuhe"]),
        ("school", &["book", "school", "pen", "calculator", "schule", "heft", "stationery"]),
        ("going out", &["cinema", "kino", "party", "club", "concert", "bowling"]),
    ];
    MAP.iter().find(|(_, words)| words.iter().any(|w| t.contains(w))).map(|(c, _)| c.to_string()).unwrap_or_else(|| {
        let w = t.split_whitespace().next().unwrap_or("other").trim_matches(|c: char| !c.is_alphanumeric());
        if w.is_empty() {
            "other".into()
        } else {
            w.to_string()
        }
    })
}

fn cents(eur: f64) -> i64 {
    (eur * 100.0).round() as i64
}

fn eur(cents: i64) -> f64 {
    cents as f64 / 100.0
}

impl Finance {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("in-memory SQLite")).expect("finance schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS finance_entries (id INTEGER PRIMARY KEY, ts TEXT NOT NULL, cents INTEGER NOT NULL, category TEXT NOT NULL, description TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS finance_goals (name TEXT PRIMARY KEY COLLATE NOCASE, target_cents INTEGER NOT NULL, saved_cents INTEGER NOT NULL DEFAULT 0, deadline TEXT, created TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Positive = income, negative = expense.
    pub fn add(&self, amount: f64, category: &str, description: &str) -> Result<(), String> {
        self.db()
            .execute("INSERT INTO finance_entries (ts, cents, category, description) VALUES (?1, ?2, ?3, ?4)", params![chrono::Local::now().to_rfc3339(), cents(amount), category, description])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// (income, expenses by category) since the first of this month.
    pub fn month(&self) -> (f64, Vec<(String, f64)>) {
        let now = chrono::Local::now();
        let from = format!("{:04}-{:02}-01", now.year(), now.month());
        let db = self.db();
        let income: i64 = db.query_row("SELECT COALESCE(SUM(cents), 0) FROM finance_entries WHERE cents > 0 AND ts >= ?1", [&from], |r| r.get(0)).unwrap_or(0);
        let mut by: Vec<(String, f64)> = db
            .prepare("SELECT category, -SUM(cents) FROM finance_entries WHERE cents < 0 AND ts >= ?1 GROUP BY category ORDER BY SUM(cents)")
            .and_then(|mut st| st.query_map([&from], |r| Ok((r.get::<_, String>(0)?, eur(r.get::<_, i64>(1)?)))).map(|rows| rows.flatten().collect()))
            .unwrap_or_default();
        by.sort_by(|a, b| b.1.total_cmp(&a.1));
        (eur(income), by)
    }

    pub fn goals(&self) -> Vec<Goal> {
        let db = self.db();
        db.prepare("SELECT name, target_cents, saved_cents, deadline FROM finance_goals ORDER BY created")
            .and_then(|mut st| st.query_map([], |r| Ok(Goal { name: r.get(0)?, target: eur(r.get(1)?), saved: eur(r.get(2)?), deadline: r.get(3)? })).map(|rows| rows.flatten().collect()))
            .unwrap_or_default()
    }

    fn set_goal(&self, name: &str, target: f64, deadline: Option<&str>) -> Result<(), String> {
        self.db()
            .execute(
                "INSERT INTO finance_goals (name, target_cents, deadline, created) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(name) DO UPDATE SET target_cents = excluded.target_cents, deadline = COALESCE(excluded.deadline, deadline)",
                params![name, cents(target), deadline, chrono::Local::now().to_rfc3339()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Adds to the goal that best matches `name` (the only goal when there is one).
    fn add_to_goal(&self, name: Option<&str>, amount: f64) -> Result<Goal, String> {
        let goals = self.goals();
        let goal = match name {
            Some(n) => goals.iter().max_by(|a, b| strsim::jaro_winkler(&a.name.to_lowercase(), &n.to_lowercase()).total_cmp(&strsim::jaro_winkler(&b.name.to_lowercase(), &n.to_lowercase()))).filter(|g| strsim::jaro_winkler(&g.name.to_lowercase(), &n.to_lowercase()) > 0.6 || goals.len() == 1),
            None if goals.len() == 1 => goals.first(),
            None => None,
        }
        .cloned()
        .ok_or_else(|| if goals.is_empty() { "There is no savings goal yet; say \"I'm saving for … with a goal of … euros\".".to_string() } else { format!("Which goal, Sir: {}?", goals.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", ")) })?;
        let db = self.db();
        db.execute("UPDATE finance_goals SET saved_cents = saved_cents + ?1 WHERE name = ?2", params![cents(amount), goal.name]).map_err(|e| e.to_string())?;
        let saved: i64 = db.query_row("SELECT saved_cents FROM finance_goals WHERE name = ?1", [&goal.name], |r| r.get(0)).optional().map_err(|e| e.to_string())?.unwrap_or(0);
        Ok(Goal { saved: eur(saved), ..goal })
    }

    fn delete_last(&self) -> Result<String, String> {
        let db = self.db();
        let last: Option<(i64, i64, String)> = db.query_row("SELECT id, cents, category FROM finance_entries ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(|e| e.to_string())?;
        let (id, c, cat) = last.ok_or("There is nothing to remove.")?;
        db.execute("DELETE FROM finance_entries WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
        Ok(format!("{:.2} € ({cat})", eur(c.abs())))
    }

    fn card(&self, tools: &ToolContext) -> (String, String) {
        let (income, spent) = self.month();
        let total: f64 = spent.iter().map(|(_, v)| v).sum();
        let month = chrono::Local::now().format("%B %Y").to_string();
        let mut md = format!("## Finances — {month}\n\n**Income** {income:.2} €  ·  **Spent** {total:.2} €  ·  **Balance** {:+.2} €\n\n", income - total);
        if !spent.is_empty() {
            md.push_str("| Category | Spent | Share |\n|---|---|---|\n");
            for (cat, v) in &spent {
                md.push_str(&format!("| {cat} | {v:.2} € | {} |\n", bar(v / total.max(0.01), 10)));
            }
        }
        let goals = self.goals();
        if !goals.is_empty() {
            md.push_str("\n### Savings goals\n");
            for g in &goals {
                md.push_str(&format!("**{}** — {:.2} / {:.2} €{}\n\n{}\n\n", g.name, g.saved, g.target, g.deadline.as_ref().map(|d| format!(" (by {d})")).unwrap_or_default(), bar(g.saved / g.target.max(0.01), 16)));
            }
        }
        show_window(tools, "finance_tracker", "Finance Tracker", &md, 560.0, 560.0);
        let top = spent.first().map(|(c, v)| format!(" Most went on {c}: {v:.0} euros.")).unwrap_or_default();
        let goal = goals.first().map(|g| format!(" {} is at {:.0} percent.", g.name, g.saved / g.target.max(0.01) * 100.0)).unwrap_or_default();
        (md, format!("This month you've spent {total:.2} euros and earned {income:.2}.{top}{goal}"))
    }

    /// `manage_finances_and_goals`.
    pub fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or("summary").to_lowercase();
        let amount = num_arg(args, &["amount_eur", "amount"]).map(f64::abs);
        let description = str_arg(args, &["description", "note"]).unwrap_or("").to_string();
        let category = str_arg(args, &["category"]).map(String::from).unwrap_or_else(|| categorise(&description));
        match action.as_str() {
            "add_expense" | "expense" | "spent" => {
                let a = amount.ok_or("How much, Sir?")?;
                self.add(-a, &categorise(&category), &description)?;
                let (_, spent) = self.month();
                let cat_total = spent.iter().find(|(c, _)| *c == categorise(&category)).map(|(_, v)| *v).unwrap_or(a);
                let (md, _) = self.card(tools);
                Ok(spoken(format!("Logged −{a:.2} € ({}).\n\n{md}", categorise(&category)), format!("Noted, Sir: {a:.2} euros on {}. That's {cat_total:.2} on {} this month.", categorise(&category), categorise(&category))))
            }
            "add_income" | "income" | "earned" => {
                let a = amount.ok_or("How much, Sir?")?;
                self.add(a, &category, &description)?;
                let (md, _) = self.card(tools);
                Ok(spoken(format!("Logged +{a:.2} € ({category}).\n\n{md}"), format!("Noted: {a:.2} euros in, Sir.")))
            }
            "set_goal" | "goal" | "create_goal" => {
                let name = str_arg(args, &["goal_name", "name"]).ok_or("What are you saving for, Sir?")?;
                let target = amount.ok_or("How much do you need for it?")?;
                self.set_goal(name, target, str_arg(args, &["deadline"]))?;
                let (md, _) = self.card(tools);
                Ok(spoken(format!("Savings goal \"{name}\": {target:.2} €.\n\n{md}"), format!("Goal set, Sir: {name}, {target:.0} euros.")))
            }
            "add_to_goal" | "save" | "deposit" => {
                let a = amount.ok_or("How much shall I put aside?")?;
                let g = self.add_to_goal(str_arg(args, &["goal_name", "name"]), a)?;
                let (md, _) = self.card(tools);
                let left = (g.target - g.saved).max(0.0);
                Ok(spoken(
                    format!("+{a:.2} € → {}: {:.2} / {:.2} €.\n\n{md}", g.name, g.saved, g.target),
                    if left <= 0.0 { format!("That completes {}, Sir. Well done.", g.name) } else { format!("{} is at {:.0} percent; {left:.0} euros to go.", g.name, g.saved / g.target.max(0.01) * 100.0) },
                ))
            }
            "delete_last" | "undo" | "remove_last" => {
                let what = self.delete_last()?;
                Ok(spoken(format!("Removed the last entry: {what}."), format!("Removed {what}, Sir.")))
            }
            _ => {
                let (md, said) = self.card(tools);
                Ok(spoken(md, said))
            }
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "manage_finances_and_goals",
        "description": "Finance Tracker: log expenses and income, savings goals with progress bars, and the month's summary by category. action add_expense / add_income (amount_eur, category, description), set_goal (goal_name, amount_eur), add_to_goal, summary, delete_last.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["add_expense", "add_income", "set_goal", "add_to_goal", "summary", "delete_last"]},
            "amount_eur": {"type": "number"},
            "category": {"type": "string"},
            "description": {"type": "string"},
            "goal_name": {"type": "string"}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_and_goals() {
        assert_eq!(categorise("pizza with friends"), "food");
        assert_eq!(categorise("Steam sale"), "games");
        let f = Finance::in_memory();
        f.add(-12.5, "food", "pizza").unwrap();
        f.add(-7.5, "food", "döner").unwrap();
        f.add(-30.0, "games", "skin").unwrap();
        f.add(100.0, "income", "pocket money").unwrap();
        let (income, spent) = f.month();
        assert_eq!(income, 100.0);
        assert_eq!(spent, vec![("games".to_string(), 30.0), ("food".to_string(), 20.0)]);
        f.set_goal("RTX 5070", 600.0, None).unwrap();
        let g = f.add_to_goal(Some("rtx"), 150.0).unwrap();
        assert_eq!((g.name.as_str(), g.saved), ("RTX 5070", 150.0));
        assert!(f.delete_last().unwrap().starts_with("100.00"));
    }
}
