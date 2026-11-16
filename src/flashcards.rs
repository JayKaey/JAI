//! Flashcards with spaced repetition (a Leitner box) for exam prep: "add a flashcard: mitosis
//! means cell division into two identical cells", "quiz me on my flashcards", "the answer is
//! cell division", "how many flashcards are due".
//!
//! A card lives in one of five boxes. A right answer moves it one box up and it comes back
//! after 1, 2, 4, 8 or 16 days; a wrong answer sends it back to box one and it returns in ten
//! minutes. The quiz asks what is due, oldest first. Everything is local (SQLite, next to the
//! memory database); the answer check is word overlap, not a language model, so it is instant
//! and Sir can always overrule it ("I got it right").

use crate::features::{spoken, str_arg};
use crate::tools::ToolContext;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Days until a card in box 1-5 is due again after a right answer.
pub const INTERVAL_DAYS: [i64; 5] = [1, 2, 4, 8, 16];
/// Seconds until a card answered wrongly comes back.
pub const RETRY_SECS: i64 = 600;
/// The share of the answer's key words Sir must say for it to count as right.
pub const PASS_SHARE: f32 = 0.6;

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: i64,
    pub deck: String,
    pub front: String,
    pub back: String,
    /// 1-5.
    pub level: i64,
    /// Unix seconds.
    pub due: i64,
}

/// What the flashcard tool answers: the chat block, the spoken line and, when the quiz shows
/// something, the Markdown of the flashcard block on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub display: String,
    pub line: String,
    pub board: Option<String>,
}

impl Reply {
    fn say(display: impl Into<String>, line: impl Into<String>) -> Self {
        Self { display: display.into(), line: line.into(), board: None }
    }

    fn with_board(mut self, markdown: String) -> Self {
        self.board = Some(markdown);
        self
    }
}

/// The cards and the one being asked right now.
#[derive(Clone)]
pub struct Deck {
    conn: Arc<Mutex<Connection>>,
    asking: Arc<Mutex<Option<i64>>>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The words of an answer that carry it (lower case, no articles or fillers).
fn key_words(text: &str) -> Vec<String> {
    const SMALL: &[&str] = &["the", "a", "an", "of", "to", "in", "is", "are", "it", "its", "and", "or", "that", "this", "into", "with", "for", "on", "by", "as", "der", "die", "das", "ein", "eine", "und", "oder", "ist", "sind", "in", "von", "zu", "mit", "für", "auf", "den", "dem", "des", "es", "sich"];
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !SMALL.contains(w)).map(String::from).collect()
}

/// Whether `answer` says what the card's back says: enough of its key words, in any order
/// (a word counts when one begins the other: "cells" for "cell", "Zellteilung" for "Zellteilungen").
pub fn matches(answer: &str, back: &str) -> bool {
    let wanted = key_words(back);
    let said = key_words(answer);
    if wanted.is_empty() || said.is_empty() {
        return false;
    }
    let hit = wanted.iter().filter(|w| said.iter().any(|s| s == *w || (s.len() >= 4 && w.len() >= 4 && (s.starts_with(w.as_str()) || w.starts_with(s.as_str()))))).count();
    hit as f32 / wanted.len() as f32 >= PASS_SHARE
}

impl Deck {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("in-memory SQLite")).expect("flashcard schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS flashcards (id INTEGER PRIMARY KEY, deck TEXT NOT NULL COLLATE NOCASE, front TEXT NOT NULL, back TEXT NOT NULL, level INTEGER NOT NULL DEFAULT 1, due INTEGER NOT NULL, seen INTEGER NOT NULL DEFAULT 0, correct INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS flashcards_due ON flashcards(due);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)), asking: Arc::new(Mutex::new(None)) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new card in box one, due at once. The same front in the same deck replaces its back.
    pub fn add(&self, deck: &str, front: &str, back: &str, at: i64) -> Result<i64, String> {
        let (deck, front, back) = (deck.trim(), front.trim(), back.trim());
        if front.is_empty() || back.is_empty() {
            return Err("A card needs a front and a back, Sir.".to_string());
        }
        let conn = self.conn();
        let known: Option<i64> = conn.query_row("SELECT id FROM flashcards WHERE deck = ?1 AND front = ?2 COLLATE NOCASE", params![deck, front], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        match known {
            Some(id) => {
                conn.execute("UPDATE flashcards SET back = ?1, level = 1, due = ?2 WHERE id = ?3", params![back, at, id]).map_err(|e| e.to_string())?;
                Ok(id)
            }
            None => {
                conn.execute("INSERT INTO flashcards (deck, front, back, level, due) VALUES (?1, ?2, ?3, 1, ?4)", params![deck, front, back, at]).map_err(|e| e.to_string())?;
                Ok(conn.last_insert_rowid())
            }
        }
    }

    fn card(row: &rusqlite::Row<'_>) -> rusqlite::Result<Card> {
        Ok(Card { id: row.get(0)?, deck: row.get(1)?, front: row.get(2)?, back: row.get(3)?, level: row.get(4)?, due: row.get(5)? })
    }

    pub fn get(&self, id: i64) -> Option<Card> {
        self.conn().query_row("SELECT id, deck, front, back, level, due FROM flashcards WHERE id = ?1", params![id], Self::card).optional().ok().flatten()
    }

    /// How many cards are due at `at` (in `deck`, or in all of them), and how many there are.
    pub fn counts(&self, deck: Option<&str>, at: i64) -> (usize, usize) {
        let conn = self.conn();
        let count = |sql: &str, due: bool| -> usize {
            let n: rusqlite::Result<i64> = match (deck, due) {
                (Some(d), true) => conn.query_row(&format!("{sql} WHERE deck = ?1 AND due <= ?2"), params![d, at], |r| r.get(0)),
                (Some(d), false) => conn.query_row(&format!("{sql} WHERE deck = ?1"), params![d], |r| r.get(0)),
                (None, true) => conn.query_row(&format!("{sql} WHERE due <= ?1"), params![at], |r| r.get(0)),
                (None, false) => conn.query_row(sql, [], |r| r.get(0)),
            };
            n.unwrap_or(0).max(0) as usize
        };
        (count("SELECT COUNT(*) FROM flashcards", true), count("SELECT COUNT(*) FROM flashcards", false))
    }

    /// The next card due at `at`: the longest overdue first.
    pub fn next_due(&self, deck: Option<&str>, at: i64) -> Option<Card> {
        let conn = self.conn();
        match deck {
            Some(d) => conn.query_row("SELECT id, deck, front, back, level, due FROM flashcards WHERE deck = ?1 AND due <= ?2 ORDER BY due, id LIMIT 1", params![d, at], Self::card),
            None => conn.query_row("SELECT id, deck, front, back, level, due FROM flashcards WHERE due <= ?1 ORDER BY due, id LIMIT 1", params![at], Self::card),
        }
        .optional()
        .ok()
        .flatten()
    }

    /// Moves a card after an answer at `at`: right → one box up, due in that box's interval;
    /// wrong → box one, due in ten minutes. The card as it is now.
    pub fn grade(&self, id: i64, right: bool, at: i64) -> Option<Card> {
        let card = self.get(id)?;
        let level = if right { (card.level + 1).min(5) } else { 1 };
        let due = if right { at + INTERVAL_DAYS[(level - 1) as usize] * 86_400 } else { at + RETRY_SECS };
        self.conn().execute("UPDATE flashcards SET level = ?1, due = ?2, seen = seen + 1, correct = correct + ?3 WHERE id = ?4", params![level, due, i64::from(right), id]).ok()?;
        self.get(id)
    }

    /// Every deck with (cards, due at `at`).
    pub fn decks(&self, at: i64) -> Vec<(String, usize, usize)> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare("SELECT deck, COUNT(*), SUM(CASE WHEN due <= ?1 THEN 1 ELSE 0 END) FROM flashcards GROUP BY deck ORDER BY deck") else { return Vec::new() };
        stmt.query_map(params![at], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize, r.get::<_, i64>(2)? as usize))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn asking(&self) -> Option<i64> {
        *self.asking.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_asking(&self, id: Option<i64>) {
        *self.asking.lock().unwrap_or_else(|e| e.into_inner()) = id;
        ASKING.store(id.is_some(), std::sync::atomic::Ordering::Release);
    }

    /// Asks the next due card: the board shows its front. None when nothing is due.
    fn ask_next(&self, deck: Option<&str>, at: i64, lead: &str) -> Option<Reply> {
        let card = self.next_due(deck, at)?;
        self.set_asking(Some(card.id));
        let (due, _) = self.counts(deck, at);
        let board = format!("**{}** — box {} of 5, {due} due\n\n## {}\n\n_Say the answer, \"show the answer\", or \"stop the quiz\"._", card.deck, card.level, card.front);
        Some(Reply::say(format!("Card ({} due, box {}): {}", due, card.level, card.front), format!("{lead}{}", card.front)).with_board(board))
    }

    /// The `manage_flashcards` tool: the reply, and the flashcard block on the board when the
    /// quiz shows something.
    pub fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let reply = self.run(args, now())?;
        if let Some(board) = &reply.board {
            crate::features::show_window(tools, "flashcards", "Flashcards", board, 520.0, 300.0);
        }
        Ok(spoken(reply.display, reply.line))
    }

    /// What the tool does at time `at` (Unix seconds).
    pub fn run(&self, args: &Value, at: i64) -> Result<Reply, String> {
        let deck = str_arg(args, &["deck", "subject"]).map(str::trim).filter(|d| !d.is_empty());
        let action = str_arg(args, &["action"]).unwrap_or(if str_arg(args, &["front"]).is_some() { "add" } else if str_arg(args, &["answer"]).is_some() { "answer" } else { "quiz" }).to_lowercase();
        match action.as_str() {
            "add" | "create" | "new" => {
                let front = str_arg(args, &["front", "question", "term"]).ok_or("What goes on the front of the card, Sir?")?;
                let back = str_arg(args, &["back", "answer", "definition"]).ok_or("And what is the answer on the back, Sir?")?;
                self.add(deck.unwrap_or("General"), front, back, at)?;
                let (_, total) = self.counts(None, at);
                Ok(Reply::say(format!("Card added to {}: {front} → {back} ({total} cards)", deck.unwrap_or("General")), format!("Card added, Sir. That makes {total}.")))
            }
            "import" => {
                let found = pairs(str_arg(args, &["text"]).unwrap_or(""));
                if found.is_empty() {
                    return Err("I found no word and meaning there, Sir. Each line needs a dash, an equals sign or a colon between them.".to_string());
                }
                let deck = deck.unwrap_or("General");
                for (front, back) in &found {
                    self.add(deck, front, back, at)?;
                }
                let preview = found.iter().take(12).map(|(f, b)| format!("- {f} → {b}")).collect::<Vec<_>>().join("\n");
                let more = if found.len() > 12 { format!("\n- … and {} more", found.len() - 12) } else { String::new() };
                let n = found.len();
                Ok(Reply::say(format!("{n} cards added to {deck}."), format!("{n} card{} added to {deck}, Sir.", if n == 1 { "" } else { "s" })).with_board(format!("### {deck}: {n} new\n{preview}{more}")))
            }
            "quiz" | "start" | "study" | "next" | "skip" => {
                if matches!(action.as_str(), "next" | "skip") {
                    // Skipped: it stays due and goes to the back of the queue.
                    if let Some(id) = self.asking() {
                        let _ = self.conn().execute("UPDATE flashcards SET due = ?1 WHERE id = ?2", params![at, id]);
                    }
                }
                match self.ask_next(deck, at, "") {
                    Some(question) => Ok(question),
                    None => {
                        self.set_asking(None);
                        let (_, total) = self.counts(deck, at);
                        Ok(Reply::say(format!("Nothing due ({total} cards)."), if total == 0 { "You have no flashcards yet, Sir. Say \"add a flashcard\" and I'll start a deck.".to_string() } else { "Nothing is due right now, Sir. They come back when it's time.".to_string() }))
                    }
                }
            }
            "answer" | "grade" | "check" => {
                let id = self.asking().ok_or("No card is being asked right now, Sir. Say \"quiz me on my flashcards\".")?;
                let card = self.get(id).ok_or("That card is gone, Sir.")?;
                let right = match (crate::tools::bool_arg(args, "correct"), str_arg(args, &["answer", "text"])) {
                    (Some(verdict), _) => verdict,
                    (None, Some(answer)) => matches(answer, &card.back),
                    (None, None) => return Err("What is your answer, Sir?".to_string()),
                };
                let moved = self.grade(id, right, at).ok_or("I couldn't move that card, Sir.")?;
                let verdict = if right { format!("Correct. Box {} now. ", moved.level) } else { format!("Not quite: {}. It comes back in ten minutes. ", card.back) };
                match self.ask_next(deck, at, &format!("{verdict}Next: ")) {
                    Some(question) => Ok(question),
                    None => {
                        self.set_asking(None);
                        let board = format!("**{}**\n\n## Done for now\n\n{}", card.deck, if right { "Every due card answered." } else { "The missed card returns in ten minutes." });
                        Ok(Reply::say(format!("{verdict}Nothing else due."), format!("{verdict}That was the last one due, Sir.")).with_board(board))
                    }
                }
            }
            "reveal" | "show" | "flip" => {
                let id = self.asking().ok_or("No card is being asked right now, Sir.")?;
                let card = self.get(id).ok_or("That card is gone, Sir.")?;
                let board = format!("**{}** — box {} of 5\n\n## {}\n\n{}\n\n_Say \"I got it right\" or \"I got it wrong\"._", card.deck, card.level, card.front, card.back);
                Ok(Reply::say(format!("Answer: {}", card.back), format!("{}. Did you have it, Sir?", card.back)).with_board(board))
            }
            "stop" | "end" | "finish" => {
                self.set_asking(None);
                let (due, total) = self.counts(None, at);
                Ok(Reply::say(format!("Quiz ended ({due} of {total} still due)."), "Quiz ended, Sir."))
            }
            "due" | "stats" | "status" | "list" => {
                let decks = self.decks(at);
                let (due, total) = self.counts(None, at);
                let lines: Vec<String> = decks.iter().map(|(name, cards, due)| format!("{name}: {cards} cards, {due} due")).collect();
                let line = match (total, due) {
                    (0, _) => "You have no flashcards yet, Sir.".to_string(),
                    (_, 0) => format!("Nothing is due, Sir. {total} cards are resting."),
                    (_, 1) => format!("One of {total} cards is due, Sir."),
                    (_, n) => format!("{n} of {total} cards are due, Sir."),
                };
                Ok(Reply::say(if lines.is_empty() { "No flashcards yet.".to_string() } else { lines.join("\n") }, line))
            }
            other => Err(format!("Unknown flashcard action '{other}'.")),
        }
    }
}

/// A quiz is running (the voice routes read it: only then is "the answer is …" an answer).
static ASKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn quiz_running() -> bool {
    ASKING.load(std::sync::atomic::Ordering::Acquire)
}

/// Word and meaning on each line of a vocabulary list read by OCR: "house - das Haus",
/// "der Hund = the dog", "Mitose: Zellteilung". A line without a divider is skipped.
fn pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        // The leftmost divider: "Mitose: Zellteilung - cell division" is Mitose.
        .filter_map(|line| [" - ", " – ", " — ", "=", ":", "\t"].iter().filter_map(|sep| line.find(sep).map(|at| (at, at + sep.len()))).min().map(|(at, past)| (&line[..at], &line[past..])))
        .map(|(front, back)| (front.trim().to_string(), back.trim().to_string()))
        .filter(|(front, back)| !front.is_empty() && !back.is_empty() && front.chars().count() <= 80 && back.chars().count() <= 160)
        .collect()
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "manage_flashcards",
        "description": "Flashcards with spaced repetition (five boxes; a right answer returns after 1, 2, 4, 8, 16 days, a wrong one after ten minutes). action add (front, back, deck), quiz (asks the next due card; deck optional), answer (answer: Sir's words, or correct: true/false when he judges himself), reveal (shows the back), next (skips), stop, due (what is due per deck), from_screenshot (reads a vocabulary list in his latest screenshot, or the picture at path, and adds a card per line: 'make flashcards from my screenshot').",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["add", "quiz", "answer", "reveal", "next", "stop", "due", "from_screenshot"]},
            "path": {"type": "string", "description": "from_screenshot: a picture instead of the latest screenshot."},
            "front": {"type": "string", "description": "add: the question or term."},
            "back": {"type": "string", "description": "add: the answer."},
            "deck": {"type": "string", "description": "The deck or subject (default General)."},
            "answer": {"type": "string", "description": "answer: what Sir said."},
            "correct": {"type": "boolean", "description": "answer: Sir's own verdict."}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn a_right_answer_climbs_the_boxes_and_a_wrong_one_starts_over() {
        let deck = Deck::in_memory();
        let t0 = 1_800_000_000;
        let id = deck.add("Biology", "Mitosis", "cell division into two identical cells", t0).unwrap();
        assert_eq!(deck.counts(None, t0), (1, 1));
        assert_eq!(deck.next_due(None, t0).map(|c| c.id), Some(id));
        // Right five times: boxes 2, 3, 4, 5, 5 with 2, 4, 8, 16, 16 days between.
        let mut at = t0;
        for (level, days) in [(2, 2), (3, 4), (4, 8), (5, 16), (5, 16)] {
            let card = deck.grade(id, true, at).unwrap();
            assert_eq!((card.level, card.due), (level, at + days * DAY));
            assert_eq!(deck.next_due(None, card.due - 1), None, "not due a second early");
            assert_eq!(deck.next_due(None, card.due).map(|c| c.id), Some(id));
            at = card.due;
        }
        // Wrong: box one, back in ten minutes.
        let card = deck.grade(id, false, at).unwrap();
        assert_eq!((card.level, card.due), (1, at + RETRY_SECS));
        // Box one's own interval is one day (a first right answer on a new card goes to box 2).
        assert_eq!(INTERVAL_DAYS[0], 1);
        assert_eq!(deck.grade(999, true, at), None);
    }

    #[test]
    fn the_longest_overdue_card_comes_first_and_decks_keep_apart() {
        let deck = Deck::in_memory();
        let t0 = 1_800_000_000;
        let a = deck.add("Physics", "Ohm's law", "U = R * I", t0).unwrap();
        let b = deck.add("Chemistry", "pH", "negative log of the oxonium concentration", t0 + 5).unwrap();
        let c = deck.add("Physics", "Momentum", "mass times velocity", t0 + 9).unwrap();
        assert_eq!(deck.next_due(None, t0 + 10).map(|x| x.id), Some(a));
        assert_eq!(deck.next_due(Some("chemistry"), t0 + 10).map(|x| x.id), Some(b), "deck names ignore case");
        deck.grade(a, true, t0 + 10);
        assert_eq!(deck.next_due(Some("Physics"), t0 + 10).map(|x| x.id), Some(c));
        assert_eq!(deck.decks(t0 + 10), vec![("Chemistry".to_string(), 1, 1), ("Physics".to_string(), 2, 1)]);
        assert_eq!(deck.counts(Some("Physics"), t0 + 10), (1, 2));
        // The same front again replaces the back and starts the card over.
        assert_eq!(deck.add("Physics", "ohm's law", "voltage is resistance times current", t0 + 20).unwrap(), a);
        assert_eq!(deck.get(a).map(|x| (x.back, x.level)), Some(("voltage is resistance times current".to_string(), 1)));
        assert!(deck.add("Physics", "  ", "x", t0).is_err() && deck.add("Physics", "x", "", t0).is_err());
    }

    #[test]
    fn an_answer_counts_by_its_key_words_not_its_wording() {
        let back = "cell division into two identical cells";
        assert!(matches("it's cell division, two identical cells", back));
        assert!(matches("division of a cell into identical cells, two of them", back));
        assert!(matches("Cells divide... division into two identical ones", back));
        assert!(!matches("photosynthesis", back));
        assert!(!matches("cell", back), "one word of four is not the answer");
        assert!(!matches("", back) && !matches("anything", ""));
        // German: inflected forms count.
        assert!(matches("die Zellteilung in zwei identische Zellen", "Zellteilung in zwei identische Zellen"));
        assert!(matches("Spannung gleich Widerstand mal Stromstärke", "Spannung ist Widerstand mal Stromstärke"));
    }

    #[test]
    fn a_vocabulary_list_read_from_a_picture_becomes_cards() {
        let deck = Deck::in_memory();
        let t = 1_800_000_000;
        let ocr = "Unit 4 vocabulary\nhouse - das Haus\nder Hund = the dog\nMitose: Zellteilung - cell division\nto run\tlaufen\nPage 12\n - \n";
        let reply = deck.run(&json!({"action": "import", "text": ocr, "deck": "English"}), t).unwrap();
        assert_eq!(reply.line, "4 cards added to English, Sir.");
        assert!(reply.board.as_deref().is_some_and(|b| b.contains("- Mitose → Zellteilung - cell division")), "{:?}", reply.board);
        assert_eq!(deck.counts(Some("English"), t), (4, 4));
        assert!(deck.run(&json!({"action": "import", "text": "Page 12\nno dividers here"}), t).is_err());
    }

    #[test]
    fn the_quiz_asks_checks_and_moves_on() {
        let deck = Deck::in_memory();
        let t = 1_800_000_000;
        let run = |args: Value| deck.run(&args, t);
        assert_eq!(run(json!({"action": "due"})).unwrap().line, "You have no flashcards yet, Sir.");
        run(json!({"action": "add", "front": "Mitosis", "back": "cell division into two identical cells", "deck": "Biology"})).unwrap();
        run(json!({"action": "add", "front": "Osmosis", "back": "diffusion of water through a membrane"})).unwrap();
        assert!(!quiz_running());
        let asked = run(json!({"action": "quiz"})).unwrap();
        assert_eq!(asked.line, "Mitosis");
        assert!(asked.board.as_deref().is_some_and(|b| b.contains("## Mitosis") && b.contains("box 1 of 5, 2 due")), "{asked:?}");
        assert!(quiz_running());
        // The back, on request.
        let shown = run(json!({"action": "reveal"})).unwrap();
        assert!(shown.display.contains("cell division into two identical cells") && shown.board.is_some(), "{shown:?}");
        // A right answer: the next card follows in the same breath.
        let next = run(json!({"action": "answer", "answer": "cell division into two identical cells"})).unwrap();
        assert_eq!(next.line, "Correct. Box 2 now. Next: Osmosis");
        // A wrong answer: the right one is said; the card returns in ten minutes, so nothing else is due.
        let done = run(json!({"action": "answer", "answer": "no idea"})).unwrap();
        assert!(done.line.contains("Not quite: diffusion of water through a membrane") && done.line.contains("last one due"), "{done:?}");
        assert!(!quiz_running());
        assert!(run(json!({"action": "answer", "answer": "x"})).is_err(), "no card is being asked");
        // Ten minutes later the missed card is back.
        assert_eq!(deck.run(&json!({"action": "quiz"}), t + RETRY_SECS).unwrap().line, "Osmosis");
        deck.run(&json!({"action": "stop"}), t + RETRY_SECS).unwrap();
        // Sir's own verdict overrules the word check.
        run(json!({"action": "add", "front": "Ion", "back": "a charged atom"})).unwrap();
        run(json!({"action": "quiz"})).unwrap();
        let ruled = run(json!({"action": "answer", "correct": true})).unwrap();
        assert!(ruled.line.starts_with("Correct. Box 2 now."), "{ruled:?}");
        let said = run(json!({"action": "due"})).unwrap();
        assert!(said.display.contains("General: 2 cards") && said.display.contains("Biology: 1 cards"), "{said:?}");
        assert!(run(json!({"action": "shuffle"})).is_err());
    }
}
