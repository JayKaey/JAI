//! "Undo that": the last change JAI made to something of his own, taken back.
//!
//! After every tool call that succeeded, what it changed is read off a snapshot taken before
//! and after it (`way_back`): the look and layout of the HUD, a window or widget of his that
//! opened or closed, a volume the mixer moved, the music he stopped, a file he edited. The way
//! back is kept here, one step, the latest (`keep`); "undo that" carries it out and forgets it
//! (`take`), so a second "undo that" finds nothing rather than redoing. Nothing of Sir's
//! (another program's window, a message) is ever taken back from here.

use crate::interface::InterfaceSettings;
use crate::widgets::{DynamicWidgetSpec, NativeWindowSpec};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long "that" still means the last change.
pub const FRESH: Duration = Duration::from_secs(10 * 60);

/// What takes one change back.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// Said once it is done: "the volume is back as it was".
    pub done: &'static str,
    /// The interface as it was before the change.
    pub interface: Option<InterfaceSettings>,
    /// His windows and widgets the change closed, as they stood.
    pub windows: Vec<NativeWindowSpec>,
    pub widgets: Vec<DynamicWidgetSpec>,
    /// The calls that take the rest back, in order.
    pub calls: Vec<(&'static str, Value)>,
}

impl Step {
    fn empty() -> Self {
        Step { done: "", interface: None, windows: Vec::new(), widgets: Vec::new(), calls: Vec::new() }
    }

    fn takes_something_back(&self) -> bool {
        self.interface.is_some() || !self.windows.is_empty() || !self.widgets.is_empty() || !self.calls.is_empty()
    }
}

/// What JAI looked like around a call: enough to see what it changed of his own.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub interface: InterfaceSettings,
    pub windows: Vec<NativeWindowSpec>,
    pub widgets: Vec<DynamicWidgetSpec>,
    /// How many volume changes the mixer has carried out so far.
    pub volume_changes: u64,
    /// The mixer holds a change it can take back.
    pub volume_undoable: bool,
}

/// The one step kept, and since when.
struct Slot(Option<(Step, Instant)>);

impl Slot {
    fn keep(&mut self, step: Step) {
        self.0 = Some((step, Instant::now()));
    }

    /// Taken out, so it is taken back once; a step gone stale is not "that" any more.
    fn take(&mut self) -> Option<Step> {
        self.0.take().filter(|(_, at)| at.elapsed() < FRESH).map(|(step, _)| step)
    }

    /// Taken out only when it is younger than `max` (an older step stays for "undo that").
    fn take_within(&mut self, max: Duration) -> Option<Step> {
        if self.0.as_ref().is_some_and(|(_, at)| at.elapsed() < max.min(FRESH)) {
            self.take()
        } else {
            None
        }
    }
}

static LAST: Mutex<Slot> = Mutex::new(Slot(None));
/// An undo is under way: what its calls change is not a new step.
static UNDOING: AtomicBool = AtomicBool::new(false);

/// The way back from a call that succeeded, from what was there before and after it and what
/// the tool said of it. None: it changed nothing of his own that can be put back.
pub fn way_back(tool: &str, args: &Value, said: &str, before: &Snapshot, after: &Snapshot) -> Option<Step> {
    // Taking something back, or bringing it back, is no change to take back.
    if tool == "undo_last_action" || args.get("restore").is_some_and(|r| !r.is_null()) {
        return None;
    }
    let action = args["action"].as_str().unwrap_or("");
    let mut step = Step::empty();
    // The volume, through whichever tool it went (the mixer, the media keys, the player).
    if after.volume_changes != before.volume_changes && after.volume_undoable {
        step.calls.push(("control_audio_mixer", json!({ "action": "undo_last" })));
        step.done = "the volume is back as it was";
    }
    // Windows and widgets of his that are gone come back; ones that appeared go away.
    let gone = |was: &[String], is: &[String]| -> Vec<String> { was.iter().filter(|id| !is.contains(id)).cloned().collect() };
    let (windows_before, windows_after): (Vec<String>, Vec<String>) = (before.windows.iter().map(|w| w.id.clone()).collect(), after.windows.iter().map(|w| w.id.clone()).collect());
    let (widgets_before, widgets_after): (Vec<String>, Vec<String>) = (before.widgets.iter().map(|w| w.id.clone()).collect(), after.widgets.iter().map(|w| w.id.clone()).collect());
    let closed_windows = gone(&windows_before, &windows_after);
    let closed_widgets = gone(&widgets_before, &widgets_after);
    if !closed_windows.is_empty() || !closed_widgets.is_empty() {
        step.windows = before.windows.iter().filter(|w| closed_windows.contains(&w.id)).cloned().collect();
        step.widgets = before.widgets.iter().filter(|w| closed_widgets.contains(&w.id)).cloned().collect();
        step.done = if closed_windows.len() + closed_widgets.len() == 1 { "it is back" } else { "they are back" };
    }
    for id in gone(&windows_after, &windows_before) {
        step.calls.push(("close_native_window", json!({ "id": id })));
        step.done = "it is gone again";
    }
    for id in gone(&widgets_after, &widgets_before) {
        // (A widget that lives in a window of his goes with that window.)
        if !after.windows.iter().any(|w| w.widgets.contains(&id)) {
            step.calls.push(("remove_widget", json!({ "id": id })));
            step.done = "it is gone again";
        }
    }
    // The interface: everything that is saved with it.
    if before.interface != after.interface {
        let (was, is) = (&before.interface, &after.interface);
        step.done = if was.span_all_monitors != is.span_all_monitors || was.fullscreen != is.fullscreen {
            "the board is as it was"
        } else if was.hud_palette != is.hud_palette {
            "the theme is as it was"
        } else if was.orb_board_pos != is.orb_board_pos || was.orb_aside != is.orb_aside {
            "I am back where I was"
        } else {
            "the interface is as it was"
        };
        step.interface = Some(was.clone());
    }
    // What the window shows but the settings do not hold.
    if tool == "configure_interface" {
        // (And the requests of the moment: tonight's night mode, subtitles, lyrics, guests.)
        for key in ["core_view", "feature_lab", "theme_studio", "mixer_view", "text_console", "night_now", "subtitles", "lyrics", "voice_guest"] {
            if let Some(on) = args.get(key).and_then(Value::as_bool) {
                step.calls.push(("configure_interface", json!({ key: !on })));
                if step.done.is_empty() {
                    step.done = "it is as it was";
                }
            }
        }
    }
    match (tool, action) {
        ("control_music", "pause" | "stop_all") => {
            step.calls.push(("control_music", json!({ "action": "resume" })));
            step.done = "the music plays again";
        }
        ("control_music", "resume") => {
            step.calls.push(("control_music", json!({ "action": "pause" })));
            step.done = "the music is paused again";
        }
        ("control_music", "next_track") => {
            step.calls.push(("control_music", json!({ "action": "previous_track" })));
            step.done = "the previous song is back";
        }
        ("control_music", "previous_track") => {
            step.calls.push(("control_music", json!({ "action": "next_track" })));
            step.done = "the song is back";
        }
        ("edit_file_surgical" | "create_or_overwrite_file", _) => {
            step.calls.push(("undo_last_file_edit", json!({})));
            step.done = "that edit is undone";
        }
        // The board he took down (a named block is not the board; "no board up" took down nothing).
        ("hud_board", "close" | "close_board" | "clear" | "clear_view" | "clear_board") if said.contains("Board closed") || said.contains("and the board") => {
            step.calls.push(("hud_board", json!({ "action": "restore" })));
            step.done = "the board is back";
        }
        // The kit (2026-11-13): what it changed is taken back by its own tools.
        ("monitor_brightness", "set" | "step") => {
            step.calls.push(("monitor_brightness", json!({ "action": "back" })));
            step.done = "the monitors are as bright as they were";
        }
        ("notebook", "add") => {
            step.calls.push(("notebook", json!({ "action": "delete_last" })));
            step.done = "that note is gone";
        }
        ("sleep_timer", "set") => {
            step.calls.push(("sleep_timer", json!({ "action": "cancel" })));
            step.done = "the sleep timer is off";
        }
        ("whenever_rule", "add") => {
            step.calls.push(("whenever_rule", json!({ "action": "remove", "last": true })));
            step.done = "that rule is gone";
        }
        // An order he put off ("play music in one minute"): dropped again.
        ("scheduled_order", "set") => {
            step.calls.push(("scheduled_order", json!({ "action": "cancel", "which": "last" })));
            step.done = "that order is no longer planned";
        }
        ("subtitle_place", "set") => {
            step.calls.push(("subtitle_place", json!({ "action": "back" })));
            step.done = "the subtitles are back where they were";
        }
        // A reminder he just set ("Reminder #7 set …"): cancelled by its number.
        ("reminders", "set" | "add" | "create") => {
            if let Some(id) = said.strip_prefix("Reminder #").and_then(|rest| rest.split(' ').next()).filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())) {
                step.calls.push(("reminders", json!({ "action": "cancel", "id": id })));
                step.done = "that reminder is cancelled";
            }
        }
        // How he listens: back to the other of the two modes.
        ("set_listening_mode", _) => {
            let was = if said.contains("WAKE-WORD") { Some("hands_free") } else if said.contains("HANDS-FREE") { Some("wake_word") } else { None };
            if let Some(mode) = was {
                step.calls.push(("set_listening_mode", json!({ "mode": mode })));
                step.done = "I listen as I did before";
            }
        }
        ("take_dictation", "start") => {
            step.calls.push(("take_dictation", json!({ "action": "stop" })));
            step.done = "dictation is off";
        }
        // (A snippet that replaced an older one of its name is not taken back by deleting it.)
        ("type_snippet", "save") if !said.contains("\" replaced (") => {
            if let Some(name) = args["name"].as_str().filter(|n| !n.is_empty()) {
                step.calls.push(("type_snippet", json!({ "action": "delete", "name": name })));
                step.done = "that snippet is forgotten again";
            }
        }
        // A fact he stored or corrected (2026-11-08: "Call me Earth." became the user_title).
        ("remember_fact", _) => {
            if let Some(call) = memory_way_back(said) {
                step.calls.push(call);
                step.done = "your memory is as it was";
            }
        }
        _ => {}
    }
    step.takes_something_back().then_some(step)
}

/// The call that takes a `remember_fact` result back: a new fact is deleted, a corrected one gets
/// its old value (and kind) again. None for anything else ("Kept memory …" changed nothing).
fn memory_way_back(said: &str) -> Option<(&'static str, Value)> {
    let line = said.lines().next()?.trim();
    if let Some(rest) = line.strip_prefix("Stored memory '") {
        let key = rest.split('\'').next().filter(|k| !k.is_empty())?;
        return Some(("delete_memory", json!({ "id_or_key": key })));
    }
    let rest = line.strip_prefix("Updated memory '")?;
    let (key, rest) = rest.split_once('\'')?;
    let rest = rest.strip_prefix(" (was: \"")?;
    // `<old>" [<category>, <importance>]): <new>`, or `<old>"): <new>` without the kind (the
    // new value may hold anything, so only what comes before the first "]): " is read).
    let kinded = rest.find("]): ").and_then(|end| rest[..end].rfind("\" [").map(|i| (&rest[..i], &rest[i + 3..end])));
    let (old, kind) = match kinded {
        Some((old, kind)) => (old, Some(kind)),
        None => (rest.split("\"): ").next()?, None),
    };
    let mut args = json!({ "key": key, "value": old, "restore": true });
    if let Some((category, importance)) = kind.and_then(|k| k.split_once(", ")) {
        args["category"] = json!(category.trim());
        if let Ok(n) = importance.trim().parse::<i64>() {
            args["importance"] = json!(n);
        }
    }
    Some(("remember_fact", args))
}

/// Keeps `step` as the one "undo that" takes (not while an undo is under way: what its own
/// calls change is the undo, not a new step).
pub fn keep(step: Step) {
    if UNDOING.load(Ordering::Acquire) {
        return;
    }
    LAST.lock().unwrap_or_else(|e| e.into_inner()).keep(step);
}

/// The step "undo that" takes now, if there is one fresh enough.
pub fn take() -> Option<Step> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The step "that's not what I said" takes: only one from the last `max` (the request he just
/// misheard), never an older change.
pub fn take_within(max: Duration) -> Option<Step> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).take_within(max)
}

/// How long ago the kept step was made, while one is kept and fresh (the [ UNDO ] chip shows
/// for a few seconds after a change).
pub fn kept_for() -> Option<Duration> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).0.as_ref().map(|(_, at)| at.elapsed()).filter(|age| *age < FRESH)
}

/// Marks the start and the end of an undo.
pub fn set_undoing(on: bool) {
    UNDOING.store(on, Ordering::Release);
}

/// How many of his last actions are kept for "what did you just do?", and how many it tells.
const DONE_KEPT: usize = 12;
const DONE_TOLD: usize = 3;

/// His last actions, the latest last: when, and what the tool said of it.
static DONE: Mutex<Vec<(Instant, String)>> = Mutex::new(Vec::new());

/// An action went through (never a read): what its tool said, as it can be read out. (The calls
/// of an undo are the undo; it is noted once, by itself.)
pub fn note_done(said: &str) {
    if UNDOING.load(Ordering::Acquire) || changed_nothing(said) {
        return;
    }
    let line = plain(said);
    if line.is_empty() {
        return;
    }
    let mut done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    if done.len() >= DONE_KEPT {
        done.remove(0);
    }
    done.push((Instant::now(), line));
}

/// What a tool says when it moved nothing: never told as something he did (2026-11-07: "Did you
/// close the mixer?" was answered "Just now: No block 'mixer' is open").
fn changed_nothing(said: &str) -> bool {
    let first = said.lines().next().unwrap_or("").trim();
    const OPENINGS: &[&str] = &["No block", "No widget", "No window", "No open window", "Nothing ", "Noted, not carried out", "Nothing was done", "Not carried out"];
    OPENINGS.iter().any(|opening| first.starts_with(opening)) || first.contains(" already ") || first.contains(" isn't open") || first.contains(" isn't running")
}

/// A window's id as Sir would name it ("drive_space"; "window_2" has no name to speak of).
fn window_name(id: &str) -> String {
    if id.is_empty() || id.strip_prefix("window_").is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())) {
        "a window".to_string()
    } else {
        format!("the {} window", id.replace('_', " "))
    }
}

/// A widget's id as Sir would name it ("timer_300", "note_1794004337503_3").
fn widget_name(id: &str) -> &'static str {
    match id.split('_').next().unwrap_or("") {
        "note" => "a note",
        "timer" => "a timer",
        "stopwatch" => "a stopwatch",
        "checklist" => "a checklist",
        _ if id.ends_with("chart") => "a chart",
        _ if id.ends_with("list") => "a list",
        _ => "a widget",
    }
}

/// The first sentence of what a tool said, fit to be told: no asides in brackets ("window(s)",
/// sizes, how to detach it), no ids, cut at a word.
pub(crate) fn plain(said: &str) -> String {
    const LONGEST: usize = 120;
    // "Created widget 'id' (Checklist, "Shopping list") in the widgets panel of the HUD."
    for (opening, told) in [("Created widget '", "Added"), ("Updated widget '", "Updated")] {
        if let Some(title) = said.strip_prefix(opening).and_then(|rest| rest.split('"').nth(1)) {
            return format!("{told} {title}");
        }
    }
    let mut depth = 0usize;
    let mut flat = String::new();
    for c in said.lines().next().unwrap_or("").chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let first = flat.split(". ").next().unwrap_or("").trim().trim_end_matches('.').trim();
    let told = if let Some(rest) = first.strip_prefix("Opened window '").or_else(|| first.strip_prefix("Opened native window '")) {
        format!("Opened {}", window_name(rest.split('\'').next().unwrap_or("")))
    } else if let Some(id) = first.strip_prefix("Closed 1 window: ") {
        format!("Closed {}", window_name(id.trim()))
    } else if let Some((count, ids)) = first.strip_prefix("Closed ").and_then(|rest| rest.split_once(" window: ")) {
        format!("Closed {count} windows: {}", ids.replace('_', " "))
    } else if let Some(what) = first.strip_prefix("Undone: ") {
        format!("Took the last change back: {what}")
    } else if let Some(id) = first.strip_prefix("Removed widget '") {
        format!("Removed {}", widget_name(id.trim_end_matches('\'')))
    } else if let Some(what) = first.strip_prefix("Interface updated: ").or_else(|| first.strip_prefix("Interface layout: ")) {
        // "Interface updated: theme Stark Gold": the change, not the machinery around it.
        let mut letters = what.chars();
        letters.next().map_or(String::new(), |c| c.to_uppercase().chain(letters).collect())
    } else {
        first.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if told.chars().count() <= LONGEST {
        return told;
    }
    let cut: String = told.chars().take(LONGEST).collect();
    cut.rsplit_once(' ').map_or(cut.as_str(), |(kept, _)| kept).trim_end_matches([',', ';', ':', ' ']).to_string()
}

/// "What did you just do?": his last actions, the latest first.
pub fn report_done() -> String {
    let done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    told(&done.iter().rev().take(DONE_TOLD).map(|(at, said)| (at.elapsed(), said.as_str())).collect::<Vec<_>>())
}

/// "Did you mute Discord?": what he lately did to it, from the same record ("mixer" for "did you
/// close the mixer?").
pub fn report_about(about: &str) -> String {
    let about = about.trim();
    let wanted = about.to_lowercase();
    let done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    let hits = done.iter().rev().filter(|(_, said)| said.to_lowercase().contains(&wanted)).take(DONE_TOLD).map(|(at, said)| (at.elapsed(), said.as_str())).collect::<Vec<_>>();
    if hits.is_empty() {
        let name = if crate::apps::find_spec(about).is_some() || crate::appindex::is_known(about) { crate::apps::display_name(about) } else { format!("The {about}") };
        return format!("No, Sir. {name} isn't among the last things I did.");
    }
    told(&hits)
}

fn told(done: &[(Duration, &str)]) -> String {
    let when = |ago: Duration| match ago.as_secs() {
        0..=59 => "Just now".to_string(),
        60..=119 => "A minute ago".to_string(),
        120..=3599 => format!("{} minutes ago", ago.as_secs() / 60),
        3600..=7199 => "An hour ago".to_string(),
        secs => format!("{} hours ago", secs / 3600),
    };
    if done.is_empty() {
        return "Nothing so far, Sir: I have not changed anything in this session.".to_string();
    }
    // One "just now" for everything of the same minute (2026-11-07: "Just now: … Just now: …").
    let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
    for (ago, said) in done {
        let label = when(*ago);
        match groups.last_mut() {
            Some((last, items)) if *last == label => items.push(said),
            _ => groups.push((label, vec![*said])),
        }
    }
    groups.iter().map(|(label, items)| format!("{label}: {}.", listed(items))).collect::<Vec<_>>().join(" ")
}

/// "Removed a chart", "Closed the drive space window" → "Removed a chart and closed the drive
/// space window": a verb after the first is lowercased, a name ("Discord muted") stays.
fn listed(items: &[&str]) -> String {
    const IRREGULAR: &[&str] = &["Took", "Set", "Put", "Made", "Sent", "Shut", "Hid", "Ran", "Brought", "Gave", "Kept", "Left", "Went", "Undid", "Began", "Wrote"];
    let lowered = |said: &str| -> String {
        let first = said.split_whitespace().next().unwrap_or("");
        if first.ends_with("ed") || IRREGULAR.contains(&first) {
            let mut letters = said.chars();
            letters.next().map_or(String::new(), |c| c.to_lowercase().chain(letters).collect())
        } else {
            said.to_string()
        }
    };
    match items {
        [] => String::new(),
        [only] => only.to_string(),
        [first, rest @ ..] => {
            let mut parts = vec![first.to_string()];
            parts.extend(rest.iter().map(|said| lowered(said)));
            let last = parts.pop().unwrap_or_default();
            format!("{} and {last}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the kit changes has its way back, and so have the requests of the moment.
    #[test]
    fn the_kits_changes_and_the_requests_of_the_moment_are_taken_back() {
        let s = snapshot();
        let back = |tool: &str, args: Value| way_back(tool, &args, "", &s, &s).map(|step| (step.calls, step.done));
        assert_eq!(back("monitor_brightness", json!({"action": "set", "level": 40})), Some((vec![("monitor_brightness", json!({"action": "back"}))], "the monitors are as bright as they were")));
        assert_eq!(back("notebook", json!({"action": "add", "text": "x"})), Some((vec![("notebook", json!({"action": "delete_last"}))], "that note is gone")));
        assert_eq!(back("sleep_timer", json!({"action": "set", "minutes": 30})), Some((vec![("sleep_timer", json!({"action": "cancel"}))], "the sleep timer is off")));
        assert_eq!(back("whenever_rule", json!({"action": "add"})), Some((vec![("whenever_rule", json!({"action": "remove", "last": true}))], "that rule is gone")));
        assert_eq!(back("scheduled_order", json!({"action": "set", "order": "play music", "in_s": 60})), Some((vec![("scheduled_order", json!({"action": "cancel", "which": "last"}))], "that order is no longer planned")));
        assert_eq!(back("scheduled_order", json!({"action": "list"})), None);
        assert_eq!(back("type_snippet", json!({"action": "save", "name": "address"})).map(|(calls, _)| calls), Some(vec![("type_snippet", json!({"action": "delete", "name": "address"}))]));
        // Looking takes nothing back, nor does typing.
        assert_eq!(back("notebook", json!({"action": "show"})), None);
        assert_eq!(back("monitor_brightness", json!({"action": "status"})), None);
        assert_eq!(back("type_snippet", json!({"action": "type", "name": "address"})), None);
        // Guest mode and tonight's night mode are requests of the moment.
        assert_eq!(back("configure_interface", json!({"voice_guest": true})).map(|(calls, _)| calls), Some(vec![("configure_interface", json!({"voice_guest": false}))]));
        assert_eq!(back("configure_interface", json!({"night_now": false})).map(|(calls, _)| calls), Some(vec![("configure_interface", json!({"night_now": true}))]));
    }

    fn snapshot() -> Snapshot {
        Snapshot { interface: InterfaceSettings::default(), windows: Vec::new(), widgets: Vec::new(), volume_changes: 0, volume_undoable: false }
    }

    #[test]
    fn the_way_back_is_read_off_what_a_call_changed() {
        let before = snapshot();
        // A look change: the settings as they were, said for what they were.
        let mut after = snapshot();
        after.interface.hud_palette = "arc_reactor".to_string();
        let step = way_back("configure_interface", &json!({"hud_palette": "arc_reactor"}), "", &before, &after).expect("a theme change has a way back");
        assert_eq!((step.done, step.interface.as_ref(), step.calls.len()), ("the theme is as it was", Some(&before.interface), 0));
        let mut spanned = snapshot();
        spanned.interface.span_all_monitors = true;
        assert_eq!(way_back("configure_interface", &json!({"span_all_monitors": true}), "", &before, &spanned).unwrap().done, "the board is as it was");
        // The core view lives in the window, not in the settings: the inverse call.
        let step = way_back("configure_interface", &json!({"core_view": true}), "", &before, &snapshot()).unwrap();
        assert_eq!(step.calls, vec![("configure_interface", json!({"core_view": false}))]);
        // A volume change, through whichever tool: the mixer's own one step back.
        let mut louder = snapshot();
        louder.volume_changes = 1;
        assert!(way_back("media_and_volume_control", &json!({"action": "volume_up"}), "", &before, &louder).is_none(), "a change the mixer cannot take back is no step");
        louder.volume_undoable = true;
        let step = way_back("media_and_volume_control", &json!({"action": "volume_up"}), "", &before, &louder).unwrap();
        assert_eq!((step.done, step.calls), ("the volume is back as it was", vec![("control_audio_mixer", json!({"action": "undo_last"}))]));
        // A closed window of his comes back as it stood; an opened one goes away again.
        let window = NativeWindowSpec { id: "drive_space".to_string(), title: "Drive Space".to_string(), ..NativeWindowSpec::default() };
        let mut open = snapshot();
        open.windows.push(window.clone());
        let step = way_back("close_native_window", &json!({"id": "drive_space"}), "", &open, &before).unwrap();
        assert_eq!((step.done, step.windows.len(), step.calls.len()), ("it is back", 1, 0));
        let step = way_back("spawn_native_window", &json!({"live": "drive_space"}), "", &before, &open).unwrap();
        assert_eq!((step.done, step.calls), ("it is gone again", vec![("close_native_window", json!({"id": "drive_space"}))]));
        // The music he stopped plays again; a read changes nothing.
        assert_eq!(way_back("control_music", &json!({"action": "stop_all"}), "", &before, &before).unwrap().calls, vec![("control_music", json!({"action": "resume"}))]);
        assert!(way_back("get_system_status", &json!({}), "", &before, &before).is_none());
        // The board comes back when it was up; "no board up" took nothing down.
        assert_eq!(way_back("hud_board", &json!({"action": "close"}), "Board closed.", &before, &before).unwrap().calls, vec![("hud_board", json!({"action": "restore"}))]);
        assert!(way_back("hud_board", &json!({"action": "close"}), "No board up.", &before, &before).is_none());
        assert!(way_back("hud_board", &json!({"action": "close", "target": "feature lab"}), "Closed: Feature Lab", &before, &before).is_none());
        // Bringing things back and the undo itself are no steps.
        assert!(way_back("spawn_native_window", &json!({"restore": "recent"}), "", &before, &open).is_none());
        assert!(way_back("undo_last_action", &json!({}), "", &open, &before).is_none());
    }

    #[test]
    fn his_last_actions_are_told_latest_first_with_their_age() {
        assert!(told(&[]).starts_with("Nothing so far"));
        let said = told(&[(Duration::from_secs(4), "Firefox lowered to 50 %"), (Duration::from_secs(70), "The theme is Stark Gold"), (Duration::from_secs(600), "Done"), (Duration::from_secs(3700), "Older"), (Duration::from_secs(9000), "Oldest")]);
        assert_eq!(said, "Just now: Firefox lowered to 50 %. A minute ago: The theme is Stark Gold. 10 minutes ago: Done. An hour ago: Older. 2 hours ago: Oldest.");
        // Repro (silent session p4, 2026-11-07): "Just now: Removed a chart. Just now: Closed …".
        let said = told(&[(Duration::from_secs(2), "Removed a chart"), (Duration::from_secs(5), "Closed the drive space window"), (Duration::from_secs(9), "Opened the drive space window"), (Duration::from_secs(80), "Discord muted")]);
        assert_eq!(said, "Just now: Removed a chart, closed the drive space window and opened the drive space window. A minute ago: Discord muted.");
        let said = told(&[(Duration::from_secs(2), "Discord muted"), (Duration::from_secs(3), "Took the last change back: the theme")]);
        assert_eq!(said, "Just now: Discord muted and took the last change back: the theme.");
    }

    #[test]
    fn what_moved_nothing_is_not_told_as_done() {
        for said in ["No block 'mixer' is open. Spoken: The mixer isn't open, Sir.", "The master volume is already at 100 %.", "Firefox isn't running, Sir.", tools_refusal()] {
            assert!(changed_nothing(said), "{said}");
        }
        for said in ["Master volume now 90 %.", "Interface updated: Theme Studio closed.", "Removed widget 'note_1'.", "Closed 1 window(s): drive_space."] {
            assert!(!changed_nothing(said), "{said}");
        }
    }

    fn tools_refusal() -> &'static str {
        crate::tools::NOT_CARRIED_OUT
    }

    #[test]
    fn what_he_did_to_one_thing_is_told_alone() {
        // (The record is the process's own: what other tests did may be in it, but never a mute
        // of a made-up program.)
        assert_eq!(report_about("zorblax"), "No, Sir. The zorblax isn't among the last things I did.");
        assert!(report_about("mixer").contains("ixer"));
    }

    #[test]
    fn what_a_tool_said_is_told_in_plain_words() {
        // Repro: the window engine's summary came out whole, cut off inside a bracket.
        let opened = "Opened window 'drive_space' as a block inside the HUD (\"detach it\" gives it its own window) (\"Drive Space\", 640×306) with live drive_space (refreshed every second), 0 widget(s) and 1 content block(s).";
        assert_eq!(plain(opened), "Opened the drive space window");
        assert_eq!(plain("Opened native window 'window_2' (\"Plan\", 640×306)."), "Opened a window");
        assert_eq!(plain("Closed 1 window(s): drive_space."), "Closed the drive space window");
        assert_eq!(plain("Undone: it is gone again. Closed 1 window(s): drive_space."), "Took the last change back: it is gone again");
        assert_eq!(plain("Firefox lowered to 50 %."), "Firefox lowered to 50 %");
        assert_eq!(plain("Interface updated: core view open. Current interface: theme custom."), "Core view open");
        assert_eq!(plain("Interface updated: theme Stark Gold. Current interface: theme stark_gold, accent #ffb800."), "Theme Stark Gold");
        assert_eq!(plain("Interface layout: orb enlarged. Now: audio devices shown."), "Orb enlarged");
        assert_eq!(plain("Closed 3 window(s): a, b, drive_space."), "Closed 3 windows: a, b, drive space");
        assert_eq!(plain("Removed widget 'timer_300'."), "Removed a timer");
        assert_eq!(plain("Removed widget 'note_1794004337503_3'."), "Removed a note");
        assert_eq!(plain("Removed widget 'cpu_load_chart'."), "Removed a chart");
        assert_eq!(plain("Removed widget 'shopping_list'."), "Removed a list");
        assert_eq!(plain("Created widget 'note_17_3' (MarkdownNoteCard, \"Note: buy milk\") in the widgets panel of the HUD."), "Added Note: buy milk");
        assert_eq!(plain("Updated widget 'x' (Checklist, \"Shopping list\")."), "Updated Shopping list");
        // Long lines end at a word.
        let long = plain(&"word ".repeat(60));
        assert!(long.len() <= 120 && long.ends_with("word"), "{long}");
        assert_eq!(plain(""), "");
    }

    #[test]
    fn one_step_is_kept_and_taken_once() {
        let mut slot = Slot(None);
        assert!(slot.take().is_none());
        let step = Step { done: "it is as it was", ..Step::empty() };
        slot.keep(Step { done: "the music plays again", ..Step::empty() });
        slot.keep(step.clone());
        assert_eq!(slot.take().as_ref(), Some(&step), "the latest step, not the first");
        assert!(slot.take().is_none(), "taken back once");
        // A step gone stale is not "that" any more (and it is gone).
        let long_ago = Instant::now().checked_sub(FRESH + Duration::from_secs(1)).expect("the machine has been up long enough");
        slot.0 = Some((step, long_ago));
        assert!(slot.take().is_none() && slot.0.is_none());
    }

    /// "That's not what I said" only takes back the change of the request just misheard.
    #[test]
    fn a_misheard_change_is_taken_within_its_window_only() {
        let mut slot = Slot(None);
        slot.keep(Step { done: "your memory is as it was", ..Step::empty() });
        assert!(slot.take_within(Duration::from_secs(120)).is_some());
        let three_minutes_ago = Instant::now().checked_sub(Duration::from_secs(180)).expect("the machine has been up long enough");
        slot.0 = Some((Step { done: "x", ..Step::empty() }, three_minutes_ago));
        assert!(slot.take_within(Duration::from_secs(120)).is_none(), "an older change stays");
    }

    /// Repro (jai.log, 2026-11-08 23:3x): "Call me Earth." — misheard over JAI's voice —
    /// overwrote the seed "Always address the user respectfully as 'Sir'." (persona, 10) with
    /// "Sir should be addressed as Earth.", and nothing could take it back.
    #[test]
    fn a_remembered_fact_is_taken_back() {
        let said = "Updated memory 'user_title' (was: \"Always address the user respectfully as 'Sir'.\" [persona, 10]): Sir should be addressed as Earth.";
        assert_eq!(
            memory_way_back(said),
            Some(("remember_fact", json!({"key": "user_title", "value": "Always address the user respectfully as 'Sir'.", "restore": true, "category": "persona", "importance": 10})))
        );
        let step = way_back("remember_fact", &json!({"key": "user_title", "value": "Sir should be addressed as Earth."}), said, &snapshot(), &snapshot()).expect("a way back");
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].0, "remember_fact");
        // The restoring call is no new change of its own.
        assert!(way_back("remember_fact", &step.calls[0].1, "Updated memory 'user_title' (was: \"x\"): y", &snapshot(), &snapshot()).is_none());
        // Older results without the kind, new facts, and facts that stayed.
        assert_eq!(memory_way_back("Updated memory 'coffee' (was: \"black\"): with milk"), Some(("remember_fact", json!({"key": "coffee", "value": "black", "restore": true}))));
        assert_eq!(memory_way_back("Stored memory 'user_title' [persona]: Sir should be addressed as Earth."), Some(("delete_memory", json!({"id_or_key": "user_title"}))));
        assert_eq!(memory_way_back("Kept memory 'coffee' [preference]: black"), None);
        // A new value with brackets of its own does not confuse the old one.
        assert_eq!(
            memory_way_back("Updated memory 'k' (was: \"old\" [fact, 5]): new \"x\" [y]): z").map(|(_, a)| a["value"].clone()),
            Some(json!("old"))
        );
    }
}
