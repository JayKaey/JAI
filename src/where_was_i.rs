//! "Where was I?" (Sir chose it on 2026-11-22): every five minutes J.A.I. keeps what is open
//! (Sir's programs and where they stand, J.A.I.'s own windows, the playlist) and what Sir last
//! asked. "Where was I?" brings it back and tells him what he was doing: after a restart what
//! was open when J.A.I. last ran, later in the day what is kept from this run.
//!
//! The same scan a protocol's capture makes (`protocols::scan_workspace` / `workspace_steps`),
//! kept in the kit's store under one key; a test or a scripted session keeps and opens nothing.

use crate::kit::say;
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const KIND: &str = "where_was_i";
/// What this run keeps, and what the run before kept (moved there as J.A.I. starts).
const LAST: &str = "last";
const BEFORE: &str = "before";
/// How often the desk is kept.
const EVERY: Duration = Duration::from_secs(300);
/// Until this run is this old, "where was I" means the run before.
const YOUNG: Duration = Duration::from_secs(30 * 60);

static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);
static ASKED: Mutex<String> = Mutex::new(String::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

static ORDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:where\s+was\s+i|where\s+did\s+i\s+(?:leave\s+off|stop)|what\s+was\s+i\s+(?:doing|working\s+on)|pick\s+up\s+where\s+i\s+left\s+off|(?:bring\s+back|restore|reopen)\s+(?:my\s+)?(?:last\s+session|what\s+i\s+had\s+open)|wo\s+war\s+ich(?:\s+stehen\s+geblieben)?|was\s+habe?\s+ich\s+(?:zuletzt\s+)?(?:gemacht|getan)|mach\s+weiter\s+wo\s+ich\s+aufgehört\s+habe)$")
        .expect("where was I")
});

/// "Where was I?", "pick up where I left off", "wo war ich?".
pub fn command(s: &str) -> Option<Value> {
    ORDER.is_match(s.trim().trim_end_matches(['?', '.', '!']).trim()).then(|| json!({}))
}

/// What Sir asked last (kept with the desk): an order or a question of some words, not this one.
pub fn note_asked(typed: &str) {
    if worth_keeping(typed) {
        *lock(&ASKED) = typed.trim().chars().take(160).collect();
    }
}

fn worth_keeping(typed: &str) -> bool {
    let said = crate::ear::words(typed).join(" ");
    said.split(' ').count() >= 3 && command(&said).is_none()
}

/// Keeps the desk every five minutes; what the run before kept is "before" from now on.
pub fn start(tools: ToolContext, stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    LazyLock::force(&STARTED);
    if let Some(last) = crate::kit::get(KIND, LAST) {
        let _ = crate::kit::put(KIND, BEFORE, &last);
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + EVERY, EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            keep(&tools).await;
        }
    });
}

async fn keep(tools: &ToolContext) {
    let apps = tokio::task::spawn_blocking(crate::protocols::scan_workspace).await.unwrap_or_default();
    // (A desk with nothing open on it keeps the one before: he stepped away, or is in a game.)
    if apps.is_empty() || crate::live_self_tester::dry() {
        return;
    }
    let music = tools.music.last_played();
    let steps = crate::protocols::workspace_steps(&apps, &tools.widgets.windows(), music.as_ref());
    let names: Vec<String> = apps.iter().map(|a| crate::apps::display_name(&a.process)).collect();
    let desk = json!({
        "at": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        "apps": names,
        "music": music.map(|m| m.name),
        "asked": lock(&ASKED).clone(),
        "steps": steps,
    });
    let _ = crate::kit::put(KIND, LAST, &desk);
}

/// When the desk was kept, as he would say it ("this morning at 9:40", "yesterday at 18:42").
fn when(at: &str) -> String {
    let Ok(at) = chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M") else { return String::new() };
    let today = chrono::Local::now().date_naive();
    let time = at.format("%H:%M");
    match (today - at.date()).num_days() {
        0 => format!("today at {time}"),
        1 => format!("yesterday at {time}"),
        _ => format!("on {} at {time}", at.format("%A")),
    }
}

/// What he says of a kept desk: the line on the board and the one he says.
fn told(desk: &Value) -> (String, String) {
    let names: Vec<String> = desk["apps"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    let mut line = format!("{} you had {} open", sentence_start(&when(desk["at"].as_str().unwrap_or(""))), crate::briefing::join_and(&names));
    if let Some(music) = desk["music"].as_str().filter(|m| !m.is_empty()) {
        line.push_str(&format!(" and {music} playing"));
    }
    line.push('.');
    let spoken = format!("{line} Bringing it back, Sir.");
    if let Some(asked) = desk["asked"].as_str().filter(|a| !a.is_empty()) {
        line.push_str(&format!(" The last thing you asked me was \"{asked}\"."));
    }
    (format!("{line} Bringing it back, Sir."), spoken)
}

fn sentence_start(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

pub async fn run(ctx: &ToolContext, _args: &Value) -> Result<String, String> {
    let young = STARTED.elapsed() < YOUNG;
    let Some(desk) = young.then(|| crate::kit::get(KIND, BEFORE)).flatten().or_else(|| crate::kit::get(KIND, LAST)) else {
        return Err("I have nothing kept yet, Sir: I keep what you have open every five minutes.".to_string());
    };
    let steps: Vec<crate::protocols::Step> = serde_json::from_value(desk["steps"].clone()).unwrap_or_default();
    let (line, spoken) = told(&desk);
    if crate::platform::hands_off() || crate::live_self_tester::dry() {
        return Ok(say(line, spoken));
    }
    // (Each program opens and takes its place one after the other, as a protocol's steps do.)
    for step in steps {
        let _ = Box::pin(ctx.execute(&step.tool, &step.args)).await;
    }
    Ok(say(line, spoken))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sir chose it on 2026-11-22: "where was I?" tells what was open and what he asked, and
    /// a test opens nothing.
    #[test]
    fn where_he_was_is_told_in_his_words() {
        for said in ["where was I?", "Where did I leave off", "what was I doing", "pick up where I left off", "reopen what I had open", "wo war ich", "Wo war ich stehen geblieben?"] {
            assert!(command(&crate::ear::words(said).join(" ")).is_some(), "{said}");
        }
        for said in ["where was i born", "where is my phone", "what am i doing tomorrow"] {
            assert_eq!(command(said), None, "{said}");
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let desk = json!({"at": format!("{today} 09:40"), "apps": ["Firefox", "Visual Studio Code", "Discord"], "music": "Lo-fi Beats", "asked": "summarise this", "steps": []});
        let (line, spoken) = told(&desk);
        assert_eq!(line, "Today at 09:40 you had Firefox, Visual Studio Code and Discord open and Lo-fi Beats playing. The last thing you asked me was \"summarise this\". Bringing it back, Sir.");
        assert_eq!(spoken, "Today at 09:40 you had Firefox, Visual Studio Code and Discord open and Lo-fi Beats playing. Bringing it back, Sir.");
        assert!(!worth_keeping("where was I?") && !worth_keeping("ok"), "neither this order nor a word or two is what he asked");
        assert!(worth_keeping("Open the earth on my second screen"));
        // Nothing kept yet: said so, nothing opened.
        let _kit = crate::kit::test_lock();
        assert!(crate::kit::get(KIND, LAST).is_none());
    }
}
