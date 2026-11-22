//! Countdown cards (Sir chose them on 2026-11-22): "count down to my exam on 12 March", "count
//! down to Christmas", "zähle die Tage bis zu meiner Prüfung am 12. März". Each is kept in the
//! kit's store; one card on the board lists them with the days left (`show`), "how many days
//! until my exam?" is answered from it (`calc::answer_dates`), and the morning briefing tells the
//! ones a week or less away (`briefing_line`).

use crate::kit::say;
use chrono::NaiveDate;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

const KIND: &str = "countdowns";
const CARD: &str = "countdowns";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a countdown phrase")
}

static SET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:start|set(?:\s+up)?|make|add|create)\s+a\s+)?count\s*down\s+(?:to|until|till|for)\s+(?P<what>.+?)(?:\s+(?:on|which\s+is\s+on|is\s+on|at)\s+(?P<date>.+))?$|^(?:zähle?|zähl)\s+(?:die\s+tage\s+)?(?:runter\s+)?bis\s+(?:zu\s+|zum\s+|zur\s+)?(?P<what2>.+?)(?:\s+am\s+(?P<date2>.+))?$|^countdown\s+(?:bis|für)\s+(?:zu\s+|zum\s+|zur\s+)?(?P<what3>.+?)(?:\s+am\s+(?P<date3>.+))?$")
});
static SHOW: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:show|list|open)\s+(?:me\s+)?(?:my|the|all)\s+countdowns?|my\s+countdowns|zeig(?:e)?\s+(?:mir\s+)?(?:meine\s+|die\s+)?countdowns)$"));
static DROP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:delete|remove|cancel|stop|drop)\s+(?:the\s+|my\s+)?(?:countdown\s+(?:to|for|until)\s+(?P<what>.+)|(?P<what2>.+?)\s+countdown)$|^(?:lösche|entferne|stoppe?)\s+(?:den\s+)?countdown\s+(?:bis|für|zu|zum|zur)\s+(?P<what3>.+)$")
});

/// How a name is kept and compared: no "my", "the", "meine".
fn key_of(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().filter(|w| !matches!(*w, "my" | "the" | "our" | "meine" | "meiner" | "meinem" | "meinen" | "mein" | "die" | "der" | "dem" | "den")).collect();
    words.join(" ")
}

/// A countdown order: the tool's arguments (None: not one, or no day he can count to).
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    if SHOW.is_match(s) {
        return Some(json!({"action": "show"}));
    }
    if let Some(c) = DROP.captures(s) {
        let what = ["what", "what2", "what3"].iter().find_map(|g| c.name(g))?.as_str();
        return Some(json!({"action": "delete", "name": key_of(what)}));
    }
    let c = SET.captures(s)?;
    let what = ["what", "what2", "what3"].iter().find_map(|g| c.name(g))?.as_str().trim();
    let today = chrono::Local::now().date_naive();
    // ("Count down to Christmas": the name is the day.)
    let date = match ["date", "date2", "date3"].iter().find_map(|g| c.name(g)) {
        Some(day) => crate::calc::parse_date(day.as_str(), today, Some(today))?.0,
        None => crate::calc::parse_date(what, today, Some(today))?.0,
    };
    let name = key_of(what);
    (!name.is_empty()).then(|| json!({"action": "set", "name": name, "mine": what.split_whitespace().next().is_some_and(|w| matches!(w, "my" | "meine" | "meiner" | "meinem" | "meinen" | "mein")), "date": date.format("%Y-%m-%d").to_string()}))
}

/// How he says a countdown's name: "your exam", "Christmas".
fn spoken(name: &str, mine: bool) -> String {
    if mine {
        return format!("your {name}");
    }
    let mut c = name.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// Every countdown still ahead (or today): (key, how he says it, the day), soonest first.
fn ahead(today: NaiveDate) -> Vec<(String, String, NaiveDate)> {
    let mut all: Vec<(String, String, NaiveDate)> = crate::kit::list(KIND)
        .into_iter()
        .filter_map(|(key, v, _)| Some((key.clone(), spoken(&key, v["mine"].as_bool().unwrap_or(false)), NaiveDate::parse_from_str(v["date"].as_str()?, "%Y-%m-%d").ok()?)))
        .filter(|(_, _, date)| *date >= today)
        .collect();
    all.sort_by_key(|(_, _, date)| *date);
    all
}

fn days(n: i64) -> String {
    match n {
        0 => "today".to_string(),
        1 => "1 day".to_string(),
        n => format!("{n} days"),
    }
}

/// The card's text: one row each.
fn card_text(today: NaiveDate) -> String {
    ahead(today).iter().map(|(_, name, date)| format!("{}  ·  {}  ·  {}", name.to_uppercase(), date.format("%d %b %Y").to_string().to_uppercase(), days((*date - today).num_days()).to_uppercase())).collect::<Vec<_>>().join("\n")
}

fn show_card(today: NaiveDate) {
    let rows = ahead(today).len();
    crate::kit::show_text(CARD, "COUNTDOWNS", &card_text(today), [380.0, 60.0 + 22.0 * rows.clamp(1, 12) as f32]);
}

/// The card, kept right as the days pass (called once a minute: `kit_notes::start`).
pub fn refresh() {
    if crate::kit::text_is_up(CARD) {
        show_card(chrono::Local::now().date_naive());
    }
}

/// "How many days until my exam?": his countdown of that name, and its day.
pub fn kept(phrase: &str) -> Option<(String, NaiveDate)> {
    let asked = key_of(&crate::ear::words(phrase).join(" "));
    ahead(chrono::Local::now().date_naive()).into_iter().find(|(key, _, _)| *key == asked).map(|(_, name, date)| (name, date))
}

/// The morning briefing's line: the countdowns a week or less away.
pub fn briefing_line(today: NaiveDate) -> Option<String> {
    let near: Vec<String> = ahead(today)
        .into_iter()
        .filter(|(_, _, date)| (*date - today).num_days() <= 7)
        .map(|(_, name, date)| match (date - today).num_days() {
            0 => format!("{name} is today"),
            n => format!("{} until {name}", days(n)),
        })
        .collect();
    (!near.is_empty()).then(|| format!("{}.", spoken(&crate::briefing::join_and(&near), false)))
}

pub fn run(args: &Value) -> Result<String, String> {
    let today = chrono::Local::now().date_naive();
    match args["action"].as_str().unwrap_or("show") {
        "set" => {
            let name = args["name"].as_str().filter(|n| !n.is_empty()).ok_or("What shall I count down to, Sir?")?;
            let date = args["date"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).ok_or("Which day is it, Sir?")?;
            crate::kit::put(KIND, name, &json!({"date": date.format("%Y-%m-%d").to_string(), "mine": args["mine"].as_bool().unwrap_or(false)}))?;
            show_card(today);
            let said = spoken(name, args["mine"].as_bool().unwrap_or(false));
            let line = match (date - today).num_days() {
                0 => format!("{} is today, Sir.", spoken(&said, false)),
                n => format!("{} until {said}, Sir. It's on the board.", spoken(&days(n), false)),
            };
            Ok(say(&line, &line))
        }
        "delete" => {
            let name = args["name"].as_str().unwrap_or("");
            if !crate::kit::remove(KIND, name) {
                return Err(format!("I have no countdown to {name}, Sir."));
            }
            if ahead(today).is_empty() {
                crate::kit::close_text(CARD);
            } else {
                show_card(today);
            }
            Ok(say(format!("The countdown to {name} is gone, Sir."), "Done, Sir."))
        }
        _ => {
            if ahead(today).is_empty() {
                return Ok(say("You have no countdowns, Sir. Say \"count down to my exam on 12 March\" to start one.", "You have no countdowns, Sir."));
            }
            show_card(today);
            Ok(say("Your countdowns are on the board, Sir.", "Your countdowns are on the board, Sir."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sir chose them on 2026-11-22: "how many days until Christmas?", "count down to my exam
    /// on 12 March". Kept, counted, listed, asked after and dropped again.
    #[test]
    fn a_countdown_is_set_counted_and_dropped() {
        let _kit = crate::kit::test_lock();
        let today = chrono::Local::now().date_naive();
        let set = command("count down to my exam on 12 March").expect("a countdown");
        assert_eq!((set["name"].as_str(), set["mine"].as_bool()), (Some("exam"), Some(true)));
        let date = NaiveDate::parse_from_str(set["date"].as_str().unwrap(), "%Y-%m-%d").unwrap();
        assert!(date >= today && date.format("%m-%d").to_string() == "03-12", "the next 12 March: {date}");
        assert_eq!(command("count down to christmas").map(|a| a["name"].clone()), Some(json!("christmas")));
        assert_eq!(command("zähle die tage bis zu meiner prüfung am 12. märz").map(|a| a["name"].clone()), Some(json!("prüfung")));
        for not in ["count down from ten", "count down to midnight", "countdown to launch", "count the days"] {
            assert_eq!(command(not), None, "{not}");
        }
        assert_eq!(command("show my countdowns"), Some(json!({"action": "show"})));
        assert_eq!(command("delete the countdown to my exam"), Some(json!({"action": "delete", "name": "exam"})));
        assert_eq!(command("cancel the exam countdown"), Some(json!({"action": "delete", "name": "exam"})));

        let in_five = (today + chrono::Duration::days(5)).format("%Y-%m-%d").to_string();
        run(&json!({"action": "set", "name": "exam", "mine": true, "date": in_five})).unwrap();
        assert!(card_text(today).starts_with("YOUR EXAM  ·  "), "{}", card_text(today));
        assert!(card_text(today).ends_with("5 DAYS"));
        assert_eq!(kept("my exam").map(|(name, _)| name), Some("your exam".to_string()));
        assert_eq!(briefing_line(today), Some("5 days until your exam.".to_string()));
        assert_eq!(crate::calc::answer_dates("how many days until my exam", today).as_deref().map(|a| a.starts_with("5 days until your exam, Sir")), Some(true));
        run(&json!({"action": "delete", "name": "exam"})).unwrap();
        assert!(ahead(today).is_empty() && briefing_line(today).is_none());
    }
}
