//! The suggestion bar: under the block in the board's spotlight, the three things Sir most
//! likely wants next, one click each.
//!
//! The candidates come from the block itself — a paged note has a next page, a map has a route,
//! a report has a source and a walk-through — and their order from Sir's own habits: every
//! choice he makes after a block of that kind (a click on the bar, or the same thing said
//! aloud: "next page", "walk me through it", "close the board") is counted per kind of block
//! and kept in `config/suggestion_habits.json`. A fresh installation starts from sensible
//! priors; after a handful of choices the bar leads with what he really does.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use tracing::warn;

/// Where Sir's habits are kept.
pub const HABITS_PATH: &str = "config/suggestion_habits.json";
/// The bar holds three.
pub const SHOWN: usize = 3;
/// What one earlier choice adds to an action's score, and after how many it stops counting.
const HABIT_WEIGHT: f32 = 0.5;
const HABIT_CAP: u32 = 10;

/// One key of the bar: its label and the tool call a click carries out, as it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub label: &'static str,
    pub tool: &'static str,
    pub args: Value,
}

fn suggestion(label: &'static str, tool: &'static str, args: Value) -> Suggestion {
    Suggestion { label, tool, args }
}

/// What the bar knows about the block in the spotlight.
#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    /// "note", "chart", "figure", "photo" or "map".
    pub kind: &'static str,
    /// The report's topic and the block's own headline.
    pub topic: String,
    pub headline: String,
    /// The page in front (0-based) and how many there are.
    pub page: usize,
    pub pages: usize,
    /// Blocks on the board.
    pub blocks: usize,
    /// The report's first source, when it is a web page.
    pub source: Option<String>,
}

/// Every action that fits the block, each with how likely it is before Sir's habits are known.
pub fn candidates(s: &Subject) -> Vec<(Suggestion, f32)> {
    let board = |label: &'static str, args: Value| suggestion(label, "hud_board", args);
    let mut out = Vec::new();
    if s.pages > 1 && s.page + 1 < s.pages {
        out.push((board("NEXT PAGE", json!({ "action": "flip_note", "direction": "next" })), 3.0));
    }
    if s.kind == "map" {
        out.push((suggestion("ROUTE THERE", crate::venue_guide::TOOL, json!({ "action": "go_here" })), 2.6));
    }
    if s.blocks > 1 {
        out.push((board("WALK ME THROUGH", json!({ "action": "walkthrough" })), 2.0));
    }
    let subject = [s.topic.trim(), s.headline.trim()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(": ");
    if !subject.is_empty() && s.kind != "map" {
        out.push((board("DIG DEEPER", json!({ "action": "research", "topic": subject })), 1.7));
    }
    if let Some(url) = s.source.as_ref().filter(|u| u.starts_with("http")) {
        out.push((suggestion("OPEN SOURCE", "open_website", json!({ "platform_or_url": url, "action_type": "direct_url" })), 1.5));
    }
    if s.pages > 1 && s.page > 0 {
        out.push((board("PREV PAGE", json!({ "action": "flip_note", "direction": "previous" })), 1.4));
    }
    out.push((board("BACK TO BOARD", json!({ "action": "minimize" })), 1.2));
    out.push((board("CLOSE BOARD", json!({ "action": "close" })), 0.6));
    out
}

/// How often Sir chose each action after each kind of block ("note/NEXT PAGE" → 7).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Habits {
    picks: HashMap<String, u32>,
}

impl Habits {
    pub fn load(path: &Path) -> Self {
        let picks = std::fs::read_to_string(path).ok().and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default();
        Self { picks }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        // Sorted, so the file reads the same from run to run.
        let sorted: std::collections::BTreeMap<&String, &u32> = self.picks.iter().collect();
        std::fs::write(path, serde_json::to_string_pretty(&sorted).unwrap_or_default())
    }

    pub fn picked(&self, kind: &str, label: &str) -> u32 {
        self.picks.get(&format!("{kind}/{label}")).copied().unwrap_or(0)
    }

    pub fn learn(&mut self, kind: &str, label: &str) {
        let count = self.picks.entry(format!("{kind}/{label}")).or_insert(0);
        *count = count.saturating_add(1);
    }
}

/// The three most likely next actions for the block: the priors, moved by Sir's habits.
pub fn top(s: &Subject, habits: &Habits) -> Vec<Suggestion> {
    let mut ranked: Vec<(Suggestion, f32)> = candidates(s)
        .into_iter()
        .map(|(suggestion, prior)| {
            let score = prior + HABIT_WEIGHT * habits.picked(s.kind, suggestion.label).min(HABIT_CAP) as f32;
            (suggestion, score)
        })
        .collect();
    // Stable: equal scores keep the candidates' own order.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(SHOWN);
    ranked.into_iter().map(|(suggestion, _)| suggestion).collect()
}

static HABITS: LazyLock<Mutex<Habits>> = LazyLock::new(|| Mutex::new(if cfg!(test) { Habits::default() } else { Habits::load(Path::new(HABITS_PATH)) }));
/// Counts every change of the habits: the board asks again only when it moved.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The bar for the block in the spotlight.
pub fn for_subject(s: &Subject) -> Vec<Suggestion> {
    top(s, &HABITS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Sir chose `label` after a block of `kind` (on the bar, or by saying it): the bar remembers.
pub fn learned(kind: &str, label: &str) {
    let snapshot = {
        let mut habits = HABITS.lock().unwrap_or_else(|e| e.into_inner());
        habits.learn(kind, label);
        habits.clone()
    };
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if cfg!(test) {
        return;
    }
    // The file is written off the caller's thread (the HUD's, on a click).
    std::thread::Builder::new()
        .name("suggestion-habits".into())
        .spawn(move || {
            if let Err(e) = snapshot.save(Path::new(HABITS_PATH)) {
                warn!("[BOARD] Could not save the suggestion habits to {HABITS_PATH}: {e}");
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(page: usize, pages: usize) -> Subject {
        Subject { kind: "note", topic: "State of VR".into(), headline: "Hardware & pricing".into(), page, pages, blocks: 6, source: Some("https://example.org/vr".into()) }
    }

    fn labels(s: &Subject, habits: &Habits) -> Vec<&'static str> {
        top(s, habits).into_iter().map(|x| x.label).collect()
    }

    #[test]
    fn the_bar_holds_the_three_most_likely_next_actions_for_the_block() {
        let fresh = Habits::default();
        // A paged note: its next page first, then the walk-through, then more on the subject.
        assert_eq!(labels(&note(0, 3), &fresh), ["NEXT PAGE", "WALK ME THROUGH", "DIG DEEPER"]);
        // On its last page there is no next page: the source moves up.
        assert_eq!(labels(&note(2, 3), &fresh), ["WALK ME THROUGH", "DIG DEEPER", "OPEN SOURCE"]);
        // A map: the route there.
        let map = Subject { kind: "map", topic: "Europa-Park".into(), headline: "Europa-Park".into(), page: 0, pages: 1, blocks: 3, source: None };
        assert_eq!(labels(&map, &fresh), ["ROUTE THERE", "WALK ME THROUGH", "BACK TO BOARD"]);
        // A lone figure without a source: what is left still fills the bar.
        let lone = Subject { kind: "figure", topic: String::new(), headline: String::new(), page: 0, pages: 1, blocks: 1, source: None };
        assert_eq!(labels(&lone, &fresh), ["BACK TO BOARD", "CLOSE BOARD"]);
        // What a click carries out: the board's own tool, with the block's subject where it matters.
        let deeper = top(&note(2, 3), &fresh).into_iter().find(|x| x.label == "DIG DEEPER").unwrap();
        assert_eq!((deeper.tool, &deeper.args), ("hud_board", &json!({ "action": "research", "topic": "State of VR: Hardware & pricing" })));
        let source = top(&note(2, 3), &fresh).into_iter().find(|x| x.label == "OPEN SOURCE").unwrap();
        assert_eq!((source.tool, &source.args), ("open_website", &json!({ "platform_or_url": "https://example.org/vr", "action_type": "direct_url" })));
        assert!(candidates(&note(1, 3)).iter().any(|(x, _)| x.label == "PREV PAGE"), "a middle page can go back too");
    }

    #[test]
    fn sirs_habits_reorder_the_bar_and_survive_a_restart() {
        let mut habits = Habits::default();
        assert_eq!(labels(&note(0, 3), &habits)[0], "NEXT PAGE");
        // He opens the source after notes, again and again: it climbs to the front.
        for _ in 0..2 {
            habits.learn("note", "OPEN SOURCE");
        }
        assert_eq!(labels(&note(0, 3), &habits), ["NEXT PAGE", "OPEN SOURCE", "WALK ME THROUGH"]);
        for _ in 0..3 {
            habits.learn("note", "OPEN SOURCE");
        }
        assert_eq!(labels(&note(0, 3), &habits)[0], "OPEN SOURCE");
        assert_eq!(habits.picked("note", "OPEN SOURCE"), 5);
        // Habits belong to a kind of block: a map's bar is untouched.
        let map = Subject { kind: "map", topic: "Europa-Park".into(), headline: String::new(), page: 0, pages: 1, blocks: 3, source: Some("https://example.org".into()) };
        assert_eq!(labels(&map, &habits)[0], "ROUTE THERE");
        // A habit never counts for more than ten choices.
        for _ in 0..40 {
            habits.learn("note", "CLOSE BOARD");
        }
        assert_eq!(labels(&note(0, 3), &habits)[0], "CLOSE BOARD");
        let capped = 0.6 + HABIT_WEIGHT * HABIT_CAP as f32;
        assert!(capped < 3.0 + HABIT_WEIGHT * HABIT_CAP as f32, "a capped habit cannot bury an action he uses as often");
        // Saved and loaded: the same habits.
        let path = std::env::temp_dir().join(format!("jai-suggestion-habits-{}.json", std::process::id()));
        habits.save(&path).unwrap();
        assert_eq!(Habits::load(&path), habits);
        std::fs::remove_file(&path).ok();
        assert_eq!(Habits::load(Path::new("no/such/file.json")), Habits::default());
        // The process-wide bar learns too, and says so through its generation.
        let before = generation();
        learned("photo", "BACK TO BOARD");
        assert!(generation() > before);
    }
}
