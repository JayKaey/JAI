//! The kit: the features Sir chose on 2026-11-13, behind one door. Each is asked for in his
//! own words on the fast path (`command`), carried out by name (`run_tool`) and answers for
//! itself in one short line (`features::spoken`); none of them is in the language model's
//! tool list, whose room is spoken for (`brain::the_whole_prompt_fits_the_context_window`).
//!
//! What they keep (notes, snippets, rules, desk layouts) lives in one small table of the
//! memory database (`kit_store`), read and written through `put` / `get` / `list` / `remove`.
//! A scripted session and the tests keep it in memory only: nothing of Sir's is read or
//! touched there. What they show as plain text (the notebook, a report) is a block of the
//! board like any other (`show_text`), drawn each frame by `show`.

use crate::tools::ToolContext;
use serde_json::Value;
use std::sync::Mutex;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The tools of the kit, by name.
pub const TOOLS: &[&str] = &[
    "take_dictation",
    "type_snippet",
    "press_keys",
    "notebook",
    "sleep_timer",
    "whenever_rule",
    "monitor_brightness",
    "desk_layout",
    "portrait_dashboard",
    "read_aloud",
    "pronunciation_coach",
    "say_like",
    "hearing_log",
    "known_guest",
    "evening_recap",
    "internet_doctor",
    "privacy_board",
    "show_changes",
    "memory_backup",
    "subtitle_place",
    "block_places",
    "notification_history",
    "scheduled_order",
    "games",
    "next_lesson",
    "grades",
    "school_day",
    "pc_helper",
    "code_helper",
    "spell_word",
    "put_away",
    "equation",
    "what_to_wear",
    "birthdays",
    "accounts",
    "other_pc",
    "admin",
    crate::news::TOOL,
];

/// "What's my next lesson?", "wann ist meine nächste Stunde?": WebUntis, without the model.
static NEXT_LESSON: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(?:whats|what s|what is|when is|when s|whens|where is|wheres|where s|was ist|wann ist|wo ist|was hab ich|was habe ich)\s+(?:my\s+|the\s+|meine\s+|die\s+)?(?:next|nächste|naechste)\s+(?:lesson|class|period|stunde|unterrichtsstunde|schulstunde)(?:\s+(?:today|tomorrow|heute|morgen))?$|^(?:what|which)\s+(?:lesson|class)\s+(?:do i have|have i got|is)\s+next$")
        .expect("next lesson pattern")
});

async fn next_lesson(ctx: &ToolContext) -> Result<String, String> {
    let (subject, at, room) = crate::briefing::next_lesson(ctx).await.ok_or("No lesson ahead in the next three days, or WebUntis isn't set up.")?;
    let when = if at.date() == chrono::Local::now().date_naive() { format!("at {}", at.format("%H:%M")) } else { format!("on {} at {}", at.format("%A"), at.format("%H:%M")) };
    let room = if room.is_empty() { String::new() } else { format!(" in {room}") };
    Ok(say(format!("Next lesson: {subject} {when}{room}"), format!("Your next lesson is {subject} {when}{room}, Sir.")))
}

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// An order for one of the kit's features in Sir's words: the tool and its arguments. `s`: the
/// fast path's normalised phrase; `original`: the words as they were said or typed (what is
/// to be kept, a note's text, keeps its capitals and its commas).
pub fn command(s: &str, original: &str) -> Option<(&'static str, Value)> {
    let s = s.trim();
    // (An order for another PC first: "open the earth on Max's PC" is Max's PC's to carry out.
    // Then games: "whenever I start GTA, …" is a game's routine, not a whenever-rule.)
    crate::fleet::command(s).map(|a| ("other_pc", a)).or_else(|| crate::kit_games::command(s).map(|a| ("games", a)))
        .or_else(|| NEXT_LESSON.is_match(s.trim_end_matches(['?', '.', '!'])).then(|| ("next_lesson", serde_json::json!({}))))
        .or_else(|| crate::news::command(s).map(|a| (crate::news::TOOL, a)))
        .or_else(|| crate::calc::equation_command(original).map(|a| ("equation", a)))
        .or_else(|| crate::commute_advisor::wear_command(s).map(|a| ("what_to_wear", a)))
        .or_else(|| crate::kit_school::grades_command(s).map(|a| ("grades", a)))
        .or_else(|| crate::kit_school::school_command(s).map(|a| ("school_day", a)))
        .or_else(|| crate::kit_pc::command(s).map(|a| ("pc_helper", a)))
        .or_else(|| crate::kit_code::command(s).map(|a| ("code_helper", a)))
        .or_else(|| crate::kit_typing::dictation_command(s).map(|a| ("take_dictation", a)))
        .or_else(|| crate::kit_typing::snippet_command(s, original).map(|a| ("type_snippet", a)))
        .or_else(|| crate::kit_typing::keys_command(s).map(|a| ("press_keys", a)))
        .or_else(|| crate::kit_notes::notebook_command(s, original).map(|a| ("notebook", a)))
        .or_else(|| crate::kit_notes::place_command(s).map(|a| ("put_away", a)))
        .or_else(|| crate::kit_notes::birthday_command(original).map(|a| ("birthdays", a)))
        .or_else(|| crate::kit_voice::spell_command(s, original).map(|a| ("spell_word", a)))
        .or_else(|| crate::kit_notes::sleep_command(s).map(|a| ("sleep_timer", a)))
        .or_else(|| crate::kit_notes::rule_command(s).map(|a| ("whenever_rule", a)))
        .or_else(|| crate::kit_screen::brightness_command(s).map(|a| ("monitor_brightness", a)))
        .or_else(|| crate::kit_screen::layout_command(s).map(|a| ("desk_layout", a)))
        .or_else(|| crate::kit_screen::dashboard_command(s).map(|a| ("portrait_dashboard", a)))
        .or_else(|| crate::kit_voice::read_command(s).map(|a| ("read_aloud", a)))
        .or_else(|| crate::kit_voice::coach_command(s).map(|a| ("pronunciation_coach", a)))
        .or_else(|| crate::kit_voice::say_like_command(s).map(|a| ("say_like", a)))
        .or_else(|| crate::kit_voice::hearing_command(s).map(|a| ("hearing_log", a)))
        .or_else(|| crate::kit_voice::guest_command(s).map(|a| ("known_guest", a)))
        .or_else(|| crate::kit_insight::recap_command(s).map(|a| ("evening_recap", a)))
        .or_else(|| crate::kit_insight::doctor_command(s).map(|a| ("internet_doctor", a)))
        .or_else(|| crate::kit_insight::privacy_command(s).map(|a| ("privacy_board", a)))
        .or_else(|| crate::admin::command(s).map(|a| ("admin", a)))
        .or_else(|| crate::accounts::command(s).map(|a| ("accounts", a)))
        .or_else(|| crate::changes::command(s).map(|a| ("show_changes", a)))
        .or_else(|| crate::backups::command(s).map(|a| ("memory_backup", a)))
        .or_else(|| crate::live_subtitles::place_command(s).map(|a| ("subtitle_place", a)))
        .or_else(|| crate::block_places::command(s).map(|a| ("block_places", a)))
        .or_else(|| crate::hud_notify::command(s).map(|a| ("notification_history", a)))
        // (Putting an order off is not asked here: `later::order` reads the whole sentence,
        // before every other rule. These are the list and the orders that drop one.)
        .or_else(|| crate::later::command(s).map(|a| (crate::later::TOOL, a)))
}

/// Carries out a kit tool.
pub async fn run_tool(ctx: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "take_dictation" => crate::kit_typing::dictation(args),
        "type_snippet" => crate::kit_typing::snippet(ctx, args).await,
        "press_keys" => crate::kit_typing::keys(ctx, args).await,
        "notebook" => crate::kit_notes::notebook(args),
        "sleep_timer" => crate::kit_notes::sleep_timer(args),
        "whenever_rule" => crate::kit_notes::rule(args),
        "monitor_brightness" => crate::kit_screen::brightness(args).await,
        "desk_layout" => crate::kit_screen::layout(ctx, args).await,
        "portrait_dashboard" => crate::kit_screen::dashboard(ctx, args),
        "read_aloud" => crate::kit_voice::read_aloud(ctx, args).await,
        "pronunciation_coach" => crate::kit_voice::coach(args),
        "say_like" => crate::kit_voice::say_like(args),
        "hearing_log" => crate::kit_voice::hearing(args),
        "known_guest" => crate::kit_voice::guest(ctx, args),
        "evening_recap" => crate::kit_insight::recap(ctx).await,
        "internet_doctor" => crate::kit_insight::doctor().await,
        "privacy_board" => crate::kit_insight::privacy(ctx, args),
        "show_changes" => crate::changes::run(args),
        "memory_backup" => crate::backups::run(args).await,
        "subtitle_place" => crate::live_subtitles::place_tool(args),
        "block_places" => crate::block_places::run(args),
        "notification_history" => crate::hud_notify::run(args),
        "scheduled_order" => crate::later::run(args),
        "games" => crate::kit_games::run(args).await,
        "next_lesson" => next_lesson(ctx).await,
        "grades" => crate::kit_school::grades(args),
        "school_day" => crate::kit_school::school(ctx, args).await,
        "pc_helper" => crate::kit_pc::run(args).await,
        "code_helper" => crate::kit_code::run(ctx, args).await,
        "spell_word" => crate::kit_voice::spell(args),
        "put_away" => crate::kit_notes::place(args),
        "birthdays" => crate::kit_notes::birthday(args),
        "equation" => crate::calc::equation(args),
        "what_to_wear" => crate::commute_advisor::wear(ctx, args).await,
        "accounts" => crate::accounts::run(ctx, args).await,
        "other_pc" => crate::fleet::run(ctx, args).await,
        "admin" => crate::admin::run(ctx, args),
        crate::news::TOOL => crate::news::run(ctx, args).await,
        other => Err(format!("The kit has no tool called '{other}'.")),
    }
}

/// Whether a call only looks (a question may ask for it) and stays inside J.A.I. (a silent
/// scripted session may carry it out): the reading actions, and what only opens a block of
/// his own or keeps a line in his own store.
pub fn only_looks(name: &str, args: &Value) -> bool {
    let action = args["action"].as_str().unwrap_or("");
    is_tool(name)
        && (matches!(action, "show" | "list" | "status" | "check" | "report")
            || matches!(name, "evening_recap" | "internet_doctor" | "show_changes" | "hearing_log" | "next_lesson" | "spell_word" | "equation" | "what_to_wear" | crate::news::TOOL)
            || (matches!(name, "put_away" | "birthdays") && matches!(action, "where" | "when"))
            || (name == "games" && matches!(action, "playtime" | "news" | "free" | "vram" | "download" | "routines" | "unplayed" | "wishlist"))
            || (name == "school_day" && crate::kit_school::school_only_looks(args))
            || (name == "pc_helper" && crate::kit_pc::only_looks(args))
            || (name == "code_helper" && crate::kit_code::only_looks(args)))
}

pub fn stays_inside(name: &str, args: &Value) -> bool {
    let action = args["action"].as_str().unwrap_or("");
    // (An order put off is a line in his own store; what it does when it falls due goes
    // through this same door, call by call.)
    only_looks(name, args)
        || (name == "games" && matches!(action, "after" | "routine" | "forget_routine" | "bedtime" | "watch_download"))
        || matches!(name, "notebook" | "privacy_board" | "pronunciation_coach" | "say_like" | "known_guest" | "subtitle_place" | "block_places" | "notification_history" | "scheduled_order" | "grades" | "put_away" | "birthdays")
        || (name == "accounts" && action == "close")
        || name == "admin"
        || (name == "school_day" && matches!(action, "teach_pack" | "homework_done")) || (matches!(name, "type_snippet" | "whenever_rule" | "desk_layout") && matches!(action, "save" | "add" | "delete" | "remove"))
}

/// Tools that press keys or type in the app in front, or answer aloud about who is in the
/// room: without his name they are carried out for no one (`ears::classify_addressee`).
pub fn acts_for_whoever_speaks(name: &str) -> bool {
    matches!(name, "press_keys" | "type_snippet" | "take_dictation" | "read_aloud" | "known_guest")
}

/// A tool's answer: what stands in the chat, and the one line he says.
pub fn say(display: impl AsRef<str>, spoken: impl AsRef<str>) -> String {
    crate::features::spoken(display, spoken)
}

// ------------------------------------------------------------------------- the store ----

/// The memory database (None: a scripted session or a test, kept in memory only).
static DB: Mutex<Option<String>> = Mutex::new(None);
/// What is kept without a database: (kind, key, value, when).
static MEMORY: Mutex<Vec<(String, String, Value, String)>> = Mutex::new(Vec::new());

/// Where the kit keeps things. Not called in a scripted session.
pub fn set_db(path: &str) {
    *lock(&DB) = Some(path.to_string());
}

fn open(db: &str) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS kit_store (kind TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, at TEXT NOT NULL, PRIMARY KEY (kind, key));")?;
    Ok(conn)
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// Keeps `value` under `kind` / `key` (replacing what was there).
pub fn put(kind: &str, key: &str, value: &Value) -> Result<(), String> {
    match lock(&DB).clone() {
        Some(db) => open(&db)
            .and_then(|conn| {
                conn.execute(
                    "INSERT INTO kit_store (kind, key, value, at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(kind, key) DO UPDATE SET value = ?3, at = ?4",
                    rusqlite::params![kind, key, value.to_string(), now()],
                )
            })
            .map(|_| ())
            .map_err(|e| format!("It could not be written down: {e}")),
        None => {
            let mut memory = lock(&MEMORY);
            memory.retain(|(k, id, _, _)| !(k == kind && id == key));
            memory.push((kind.to_string(), key.to_string(), value.clone(), now()));
            Ok(())
        }
    }
}

/// Everything kept under `kind`, oldest first: (key, value, when it was kept).
pub fn list(kind: &str) -> Vec<(String, Value, String)> {
    match lock(&DB).clone() {
        Some(db) => {
            // (Read only: looking makes no table.)
            let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Vec::new() };
            let Ok(mut rows) = conn.prepare("SELECT key, value, at FROM kit_store WHERE kind = ?1 ORDER BY at") else { return Vec::new() };
            let read = rows.query_map([kind], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)));
            read.map(|rows| rows.flatten().filter_map(|(key, value, at)| Some((key, serde_json::from_str(&value).ok()?, at))).collect()).unwrap_or_default()
        }
        None => lock(&MEMORY).iter().filter(|(k, _, _, _)| k == kind).map(|(_, key, value, at)| (key.clone(), value.clone(), at.clone())).collect(),
    }
}

pub fn get(kind: &str, key: &str) -> Option<Value> {
    list(kind).into_iter().find(|(k, _, _)| k == key).map(|(_, value, _)| value)
}

/// Removes one entry; whether it was there.
pub fn remove(kind: &str, key: &str) -> bool {
    match lock(&DB).clone() {
        Some(db) => open(&db).and_then(|conn| conn.execute("DELETE FROM kit_store WHERE kind = ?1 AND key = ?2", rusqlite::params![kind, key])).is_ok_and(|n| n > 0),
        None => {
            let mut memory = lock(&MEMORY);
            let before = memory.len();
            memory.retain(|(k, id, _, _)| !(k == kind && id == key));
            memory.len() != before
        }
    }
}

/// Removes everything kept under `kind`; how much it was.
pub fn clear(kind: &str) -> usize {
    match lock(&DB).clone() {
        Some(db) => open(&db).and_then(|conn| conn.execute("DELETE FROM kit_store WHERE kind = ?1", [kind])).unwrap_or(0),
        None => {
            let mut memory = lock(&MEMORY);
            let before = memory.len();
            memory.retain(|(k, _, _, _)| k != kind);
            before - memory.len()
        }
    }
}

/// A key that sorts by time and is never used twice.
pub fn stamp() -> String {
    static LAST: Mutex<i64> = Mutex::new(0);
    let mut last = lock(&LAST);
    *last = chrono::Local::now().timestamp_millis().max(*last + 1);
    last.to_string()
}

#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    let guard = lock(&ONE);
    *lock(&DB) = None;
    lock(&MEMORY).clear();
    guard
}

// ------------------------------------------------------------------- blocks of plain text ----

struct TextBlock {
    id: String,
    title: String,
    text: String,
    size: [f32; 2],
}

static BLOCKS: Mutex<Vec<TextBlock>> = Mutex::new(Vec::new());

/// Opens a block of plain text on the board (or gives the one that is up its new text).
pub fn show_text(id: &str, title: &str, text: &str, size: [f32; 2]) {
    let mut blocks = lock(&BLOCKS);
    match blocks.iter_mut().find(|b| b.id == id) {
        Some(block) => (block.title, block.text) = (title.to_string(), text.to_string()),
        None => blocks.push(TextBlock { id: id.to_string(), title: title.to_string(), text: text.to_string(), size }),
    }
    drop(blocks);
    crate::hud_dock::cancel_close(id);
    crate::gui::refresh();
}

/// Whether one of the kit's text blocks is up.
pub fn text_is_up(id: &str) -> bool {
    lock(&BLOCKS).iter().any(|b| b.id == id)
}

pub fn close_text(id: &str) {
    lock(&BLOCKS).retain(|b| b.id != id);
    crate::hud_dock::forget(id);
}

/// Draws the kit's blocks (each HUD frame).
pub fn show(ctx: &eframe::egui::Context) {
    let open: Vec<(String, String, String, [f32; 2])> = lock(&BLOCKS).iter().map(|b| (b.id.clone(), b.title.clone(), b.text.clone(), b.size)).collect();
    for (id, title, text, size) in open {
        if crate::gui::text_block(ctx, &id, &title, size, &text) {
            close_text(&id);
        }
    }
    crate::kit_voice::show(ctx);
    crate::kit_insight::show(ctx);
    crate::changes::show(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_store_keeps_replaces_lists_and_forgets() {
        let _one = test_lock();
        assert!(list("note").is_empty());
        put("note", "1", &json!({"text": "milk"})).unwrap();
        put("note", "2", &json!({"text": "bread"})).unwrap();
        put("snippet", "address", &json!({"text": "12 Example Road"})).unwrap();
        assert_eq!(list("note").iter().map(|(k, v, _)| (k.as_str(), v["text"].as_str().unwrap_or(""))).collect::<Vec<_>>(), [("1", "milk"), ("2", "bread")]);
        put("note", "1", &json!({"text": "oat milk"})).unwrap();
        assert_eq!(get("note", "1"), Some(json!({"text": "oat milk"})));
        assert_eq!(list("note").len(), 2, "replaced, not added");
        assert!(remove("note", "2") && !remove("note", "2"));
        assert_eq!(clear("note"), 1);
        assert_eq!(list("snippet").len(), 1, "another kind is left alone");
        let (a, b) = (stamp(), stamp());
        assert!(a < b, "stamps sort by time and are never the same");
    }

    #[test]
    fn the_store_survives_the_database() {
        let _one = test_lock();
        let dir = std::env::temp_dir().join(format!("jai-kit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("kit.db").to_string_lossy().to_string();
        set_db(&db);
        assert!(list("rule").is_empty(), "a database without the table reads as nothing kept");
        put("rule", "a", &json!({"when": "minecraft starts"})).unwrap();
        put("rule", "b", &json!({"when": "discord closes"})).unwrap();
        assert_eq!(list("rule").len(), 2);
        assert!(remove("rule", "a"));
        assert_eq!(list("rule").iter().map(|(k, _, _)| k.clone()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(clear("rule"), 1);
        *lock(&DB) = None;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_tool_of_the_kit_has_a_name_and_only_reading_ones_answer_questions() {
        assert!(TOOLS.iter().all(|t| is_tool(t)) && !is_tool("open_application"));
        assert!(only_looks("notebook", &json!({"action": "show"})) && !only_looks("notebook", &json!({"action": "add", "text": "x"})));
        assert!(only_looks("sleep_timer", &json!({"action": "status"})) && !only_looks("sleep_timer", &json!({"action": "set", "minutes": 30})));
        assert!(stays_inside("notebook", &json!({"action": "add"})) && !stays_inside("press_keys", &json!({"keys": "next_tab"})) && !stays_inside("monitor_brightness", &json!({"level": 40})));
    }
}
