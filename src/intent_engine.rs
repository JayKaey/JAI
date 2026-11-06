//! The grounded intent engine: what Sir actually asked for, decided in Rust before (and
//! checked after) the language model.
//!
//! * "Forget that" / "never mind" / "vergiss es" (also misheard: "no fool forgets that idea")
//!   cancels: the conversation, any pending offer or dialogue and the working memory are
//!   cleared and Jarvis says one line — the model is never asked.
//! * "Bring up everything you know about Spotify" / "zeig mir alles über …" opens the topic
//!   knowledge workspace deterministically (overview dossier, knowledge graph, live research).
//! * A new, self-contained request is answered on its own (no earlier turns in the prompt); a
//!   follow-up ("make it bigger", "what about tomorrow?") sees the last two exchanges only.
//! * Every sentence the model writes passes the grounding filter: a claimed action ("I have
//!   lowered the opacity", "Opening the bookmark manager", "shall I create this window now?")
//!   with no matching tool executed in this turn never reaches Sir.

use crate::deep_research_engine as research;
use crate::intents::{Intent, SilentAction};
use crate::tools::ToolContext;
use crate::workspace_canvas::SpatialWindowKind;
use aho_corasick::{AhoCorasick, AhoCorasickKind, MatchKind};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

pub const CANCEL_REPLY: &str = "Understood, Sir. I have disregarded that.";
/// Earlier user/assistant exchanges a follow-up request may see.
pub const MAX_EXCHANGES: usize = 2;
/// Said when every sentence of a reply was an unbacked claim.
pub const GROUNDED_FALLBACK: &str = "Forgive me, Sir, I haven't done anything about that yet — tell me what you'd like and I'll do it.";

/// The hard grounding rule, appended to the system prompt.
pub const GROUNDING_RULE: &str = "CRITICAL RULE: You are ONLY allowed to say you performed an action (like changing opacity, opening a window, changing volume, or adding an event) if a [TOOL_EXECUTED: ...] result is explicitly present in THIS turn! Never invent actions, never mention widget opacity unless asked about opacity, and never ask 'shall I create this window now?'—just execute the requested tool immediately!";

/// The first line of every tool result in the history.
pub fn tool_marker(name: &str) -> String {
    format!("[TOOL_EXECUTED: {name}]")
}

/// Lower-case words (apostrophes kept), punctuation gone.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(String::from)
        .collect()
}

// --------------------------------------------------------------------------- cancel ----

/// Courtesy and hesitation words around a cancel ("no, Jarvis, just forget that idea").
const CANCEL_FILLERS: [&str; 34] = [
    "no", "nope", "nah", "nein", "oh", "um", "uh", "hm", "hmm", "well", "okay", "ok", "jarvis", "please", "bitte", "sir", "actually", "just", "wait", "hey",
    "the", "that's", "thats", "idea", "die", "idee", "a", "about", "sorry", "mal", "you", "can", "ach", "na",
];
/// Words Whisper puts into a mumbled "no, forget that" ("no fool forgets that idea").
const CANCEL_JUNK: [&str; 8] = ["fool", "full", "foo", "fall", "for", "four", "so", "then"];
/// Cancels that tolerate one junk word.
const CANCEL_PHRASES: [&[&str]; 22] = [
    &["forget", "that"],
    &["forget", "it"],
    &["forget", "this"],
    &["never", "mind"],
    &["nevermind"],
    &["scratch", "that"],
    &["scratch", "it"],
    &["cancel", "that"],
    &["cancel", "it"],
    &["drop", "it"],
    &["drop", "that"],
    &["disregard", "that"],
    &["ignore", "that"],
    &["vergiss", "es"],
    &["vergiss", "das"],
    &["vergiss", "dies"],
    &["doch", "nicht"],
    &["lass", "es"],
    &["lass", "gut", "sein"],
    &["schon", "gut"],
    &["ist", "egal"],
    &["forget", "everything"],
];
/// Cancels only as the whole request ("cancel the timer" is a command, not a cancel).
const CANCEL_ALONE: [&[&str]; 5] = [&["abort"], &["cancel"], &["egal"], &["forget"], &["stop", "that", "idea"]];

fn stem(word: &str) -> &str {
    match word {
        "forgets" | "forgot" | "forgetting" | "forgotten" | "forgit" | "foget" => "forget",
        "vergess" | "vergesse" | "vergessen" => "vergiss",
        "cancelled" | "canceled" => "cancel",
        other => other,
    }
}

/// "Forget that", "never mind", "scratch that", "vergiss es", "egal", "doch nicht" — also
/// misheard ("no fool forgets that idea"). Never a command with an object ("cancel the timer",
/// "forget that I like jazz").
pub fn is_cancel(text: &str) -> bool {
    let tokens: Vec<String> = words(text).iter().map(|w| stem(w).to_string()).filter(|w| !CANCEL_FILLERS.contains(&w.as_str())).collect();
    if tokens.is_empty() || tokens.len() > 5 {
        return false;
    }
    let refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
    if CANCEL_ALONE.contains(&refs.as_slice()) {
        return true;
    }
    CANCEL_PHRASES.iter().any(|phrase| {
        refs.windows(phrase.len()).enumerate().any(|(at, window)| {
            window == *phrase && {
                let rest: Vec<&&str> = refs[..at].iter().chain(&refs[at + phrase.len()..]).collect();
                rest.len() <= 1 && rest.iter().all(|w| CANCEL_JUNK.contains(*w))
            }
        })
    })
}

// ------------------------------------------------------------ deterministic actions ----

/// Commands decided and carried out in Rust before anything else: the model never sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentAction {
    /// "Close the windows you opened", "mach die Fenster wieder zu" (also after "No. Just …").
    CloseSecondaryWindows,
    /// "Schließ dich", "beende dich", "quit Jarvis", "close yourself".
    ExitJarvisApplication,
}

pub const WINDOWS_CLOSED_REPLY: &str = "Right away, Sir. All secondary windows have been closed.";
pub const NO_WINDOWS_REPLY: &str = "There are no secondary windows open, Sir.";

/// Corrections, courtesy and hesitation in front of the real command ("No. Just can you …",
/// "Nein, einfach …", "Wait, Jarvis, could you please …").
const LEAD_FILLERS: [&str; 40] = [
    "no", "nope", "nah", "nein", "wait", "warte", "just", "einfach", "actually", "eigentlich", "okay", "ok", "jarvis", "please", "bitte", "hey", "um", "uh",
    "ähm", "äh", "so", "also", "then", "dann", "sir", "well", "oh", "can", "could", "would", "will", "you", "kannst", "könntest", "würdest", "du", "mal",
    "now", "jetzt", "and",
];
/// Courtesy after the command ("… for me, please", "… bitte jetzt").
const TAIL_FILLERS: [&str; 12] = ["please", "bitte", "jarvis", "sir", "now", "jetzt", "mal", "too", "as", "well", "for", "me"];

/// The command itself: leading corrections and trailing courtesy stripped, lower case.
pub fn command_core(text: &str) -> String {
    strip_fillers(text, false)
}

/// `keep_name`: "Jarvis" stays ("Jarvis beenden", "quit Jarvis" name their object).
fn strip_fillers(text: &str, keep_name: bool) -> String {
    let filler = |t: &String, list: &[&str]| list.contains(&t.as_str()) && !(keep_name && t == "jarvis");
    let w = words(text);
    let start = w.iter().position(|t| !filler(t, &LEAD_FILLERS)).unwrap_or(w.len());
    let end = w.iter().rposition(|t| !filler(t, &TAIL_FILLERS)).map_or(start, |i| i + 1).max(start);
    w[start..end].join(" ")
}

static CLOSE_SECONDARY: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let adjectives = r"(?:(?:extra|secondary|research|sub|other|additional|new|open|opened|spatial|workspace|knowledge|floating|pop ?up|jarvis|your|my|those|these|the|all|of|remaining)\s+)*";
    let made = r"(?:\s+(?:that\s+|which\s+)?(?:you|jarvis)\s+(?:just\s+|have\s+|had\s+)?(?:opened|open|made|created|spawned|brought up|pulled up|put up))?";
    [
        // "close the windows you opened", "close all windows", "close the research windows",
        // "close subwindows", "get rid of those extra windows".
        format!(r"^(?:close|shut|dismiss|get rid of|remove|kill)\s+(?:down\s+)?{adjectives}(?:windows|sub ?windows|panels|popups|pop ups){made}(?:\s+again)?$"),
        // "close everything you opened", "close all of them".
        r"^(?:close|shut)\s+(?:everything|all of them|them all)(?:\s+(?:that\s+)?you\s+(?:just\s+)?(?:opened|open|made|spawned))?$".to_string(),
        // "schließ die Fenster", "mach die Fenster wieder zu", "schließ alle deine Fenster".
        r"^(?:schließ|schließe|schliess|schliesse|mach|mache)\s+(?:(?:alle|die|deine|diese|eure|zusätzlichen|extra|neuen|offenen|geöffneten|anderen|recherche|unter)\s+)*(?:fenster|unterfenster|nebenfenster)(?:\s+(?:die du|die du geöffnet hast|wieder|alle|bitte))*(?:\s+zu)?$".to_string(),
        // "Fenster schließen", "alle Fenster zu".
        r"^(?:alle\s+|die\s+)?(?:fenster|unterfenster)\s+(?:wieder\s+)?(?:schließen|schliessen|zumachen|zu)$".to_string(),
    ]
    .iter()
    .map(|p| Regex::new(p).expect("close-windows expression"))
    .collect()
});

static EXIT_JARVIS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // German.
        r"^(?:schließ|schließe|schliess|schliesse|beende|beend)\s+dich(?:\s+selbst)?$",
        r"^(?:mach|mache)\s+dich\s+(?:zu|aus)$",
        r"^(?:schalt|schalte)\s+dich\s+(?:aus|ab)$",
        r"^(?:fahr|fahre)\s+dich\s+(?:runter|herunter)$",
        r"^(?:jarvis|programm|das programm|app|die app)\s+(?:beenden|schließen|schliessen|ausschalten)$",
        r"^(?:beende|schließ|schließe)\s+(?:jarvis|das programm|die app)$",
        r"^geh\s+aus$",
        // English.
        r"^(?:close|quit|exit|terminate|end)\s+(?:yourself|urself|your self|jarvis|the jarvis app)$",
        r"^(?:shut|turn|switch|power)\s+(?:yourself|jarvis)\s+(?:down|off)$",
        r"^(?:shut|turn|switch|power)\s+(?:down|off)\s+(?:yourself|jarvis)$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("exit expression"))
    .collect()
});

/// The deterministic command in `text`, if any. Corrections in front ("No. Just can you …")
/// never stop the command behind them; a negated command ("don't close yourself") is none.
pub fn deterministic_action(text: &str) -> Option<IntentAction> {
    let core = command_core(text);
    if core.is_empty() || words(&core).iter().any(|w| matches!(w.as_str(), "don't" | "dont" | "not" | "never" | "nicht" | "nie")) {
        return None;
    }
    let named = strip_fillers(text, true);
    if EXIT_JARVIS.iter().any(|r| r.is_match(&core) || r.is_match(&named)) {
        return Some(IntentAction::ExitJarvisApplication);
    }
    if CLOSE_SECONDARY.iter().any(|r| r.is_match(&core)) {
        return Some(IntentAction::CloseSecondaryWindows);
    }
    None
}

/// Closes every secondary window Jarvis opened, at once: the in-app blocks (Feature Lab, Theme
/// Studio, telemetry …, which fade out), the spatial workspace windows, the native windows
/// (system stats, schedule …), the simulations and every pop-up card. Returns how many were
/// closed.
pub fn close_secondary_windows(tools: &ToolContext) -> usize {
    let started = std::time::Instant::now();
    // First, while the native windows are still among the published blocks.
    let blocks = crate::hud_dock::close_all_blocks();
    let spatial = crate::workspace_canvas::close_all_now();
    let native = tools.widgets.close_window("all").map(|out| closed_count(&out)).unwrap_or(0);
    let sims: Vec<String> = tools.stark.simlab.list().into_iter().map(|s| s.id).collect();
    let simulations = sims.iter().filter(|id| tools.stark.simlab.close(id)).count();
    let popups = crate::dynamic_popups::dismiss_all(true);
    tools.focus.forget_jarvis_window("all");
    crate::gui::refresh();
    info!(
        "[WINDOWS] Closed {blocks} in-app block(s), {spatial} workspace, {native} native, {simulations} simulation window(s) and {popups} pop-up(s) in {:.2} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    // Docked native windows and simulations are in-app blocks too: counted once.
    blocks.max(native + simulations) + spatial + popups
}

/// "Closed 3 window(s): a, b, c." → 3.
fn closed_count(out: &str) -> usize {
    out.strip_prefix("Closed ").and_then(|s| s.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0)
}

// ------------------------------------------------------------ knowledge workspace ----

static KNOWLEDGE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^(?:(?:hey |ok |okay )?jarvis[,!]?\s+)?(?:(?:can|could|would|will) you |please |kindly )*(?:bring up|pull up|show me|show|open up|give me|display|put up)\s+(?:everything|all(?: the)? (?:data|files|info|information|facts|stuff|details|knowledge)|all)\s+(?:that )?(?:you (?:know|have|got|can find|could find)\s+)?(?:about|on|regarding|for|concerning|of)\s+(.+?)\s*(?:please|for me|sir|jarvis)?[?.!]*$",
        r"(?i)^(?:(?:hey )?jarvis[,!]?\s+)?(?:(?:can|could|would) you |please )*research\s+(.+?)\s+and\s+(?:bring up|show|open|pull up|put up)(?: me)?\s+(?:the |all the )?(?:windows?|workspace|everything|results?|dossier)\s*(?:please|for me)?[?.!]*$",
        r"(?i)^(?:jarvis[,!]?\s+)?(?:zeig|zeige|bring)\s+mir\s+alles\s+(?:was du\s+)?(?:über|ueber|zu|von)\s+(.+?)(?:\s+(?:weißt|weisst|hast))?(?:\s+bitte)?[?.!]*$",
    ]
    .into_iter()
    .map(|p| Regex::new(p).expect("valid knowledge pattern"))
    .collect()
});

/// The topic of "bring up everything you know about X" (and its siblings).
pub fn knowledge_workspace_topic(text: &str) -> Option<String> {
    let text = text.trim();
    let raw = KNOWLEDGE.iter().find_map(|re| re.captures(text).map(|c| c[1].to_string()))?;
    let mut topic = raw.trim().trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.' | '?' | '!')).trim().to_string();
    for article in ["the ", "my ", "das ", "die ", "der "] {
        if topic.to_lowercase().starts_with(article) && topic.len() > article.len() + 2 {
            topic = topic[article.len()..].to_string();
        }
    }
    (!topic.is_empty() && topic.split_whitespace().count() <= 8).then_some(topic)
}

/// The three windows of a topic workspace, with their titles.
pub fn topic_workspace_windows() -> [(SpatialWindowKind, &'static str); 3] {
    [
        (SpatialWindowKind::ExecutiveDossier, "OVERVIEW & FACTS"),
        (SpatialWindowKind::KnowledgeGraphPinboard, "KNOWLEDGE GRAPH & CONNECTIONS"),
        (SpatialWindowKind::LiveCrawlerStream, "LIVE RESEARCH & WEB SOURCES"),
    ]
}

// ------------------------------------------------------------- isolation / history ----

/// Words that point back at earlier turns.
const REFERENTIAL: [&str; 30] = [
    "it", "its", "them", "him", "her", "those", "these", "there", "again", "same", "another", "previous", "earlier", "instead", "too", "else", "former",
    "latter", "es", "ihn", "ihm", "dort", "nochmal", "wieder", "gleiche", "selbe", "dasselbe", "davon", "damit", "dazu",
];
/// Openings that continue the conversation.
const CONTINUATIONS: [&str; 34] = [
    "and", "also", "then", "but", "so", "yes", "yeah", "yep", "yup", "no", "nope", "sure", "okay", "ok", "why", "how come", "really", "what about",
    "how about", "do it", "do that", "go ahead", "please do", "more", "less", "und", "auch", "dann", "ja", "nein", "mach das", "noch", "warum", "wieso",
];

/// A follow-up that needs the previous exchanges ("make it bigger", "what about tomorrow?",
/// "yes", "full screen"). Biased towards follow-up: a wrong "new request" loses context.
pub fn is_follow_up(text: &str) -> bool {
    let w = words(text);
    let w: Vec<&str> = w.iter().map(String::as_str).filter(|x| *x != "jarvis").collect();
    if w.len() <= 3 {
        return true;
    }
    let joined = w.join(" ");
    if CONTINUATIONS.iter().any(|c| joined == *c || joined.starts_with(&format!("{c} "))) {
        return true;
    }
    if w.iter().any(|x| REFERENTIAL.contains(x)) {
        return true;
    }
    // "put that in a window", "close this", "that one": a demonstrative without its noun.
    w.iter().enumerate().any(|(i, x)| {
        matches!(*x, "that" | "this" | "das" | "dies")
            && w.get(i + 1).is_none_or(|next| matches!(*next, "to" | "in" | "into" | "on" | "for" | "one" | "window" | "again" | "please" | "up" | "down" | "away" | "bitte" | "in's"))
    })
}

/// A new, self-contained request: the model sees only this turn.
pub fn isolate_turn(text: &str) -> bool {
    !is_follow_up(text)
}

/// Where the prompt's history starts: the newest user message (isolated), or the user message
/// `MAX_EXCHANGES` before it.
pub fn history_start(history: &[Value], isolate: bool) -> usize {
    let users: Vec<usize> = history.iter().enumerate().filter(|(_, m)| m["role"] == "user").map(|(i, _)| i).collect();
    let Some(&newest) = users.last() else { return 0 };
    if isolate {
        return newest;
    }
    users.len().checked_sub(MAX_EXCHANGES + 1).map(|k| users[k]).unwrap_or(users[0])
}

// ------------------------------------------------------------------ reasoning gears ----

/// How hard the language model works on a turn (deterministic commands never reach it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningGear {
    /// Quick questions and everyday requests: no hidden reasoning, a short reply budget.
    Reflex,
    /// Proofs, calculations, code, comparisons, long writing, "think carefully": full reasoning.
    Deep,
}

pub const REFLEX_TOKENS: i32 = 180;
pub const REFLEX_TEMPERATURE: f32 = 0.3;
pub const DEEP_TOKENS: i32 = 1024;
/// From this many words a request is no longer a quick one.
const REFLEX_MAX_WORDS: usize = 15;

const DEEP_CUES: &[&str] = &[
    " think carefully ", " think hard ", " analyze ", " analyse ", " analysis ", " in detail ", " step by step ", " prove ", " proof ",
    " derive ", " calculate ", " solve ", " why ", " compare ", " versus ", " pros and cons ", " debug ", " architecture ", " algorithm ",
    " refactor ", " code ", " script ", " write ", " essay ", " poem ", " story ", " equation ", " integral ", " derivative ", " physics ",
    " chemistry ", " warum ", " berechne ", " analysiere ", " ausführlich ", " beweise ", " vergleiche ", " erkläre ", " schreibe ",
];

/// Reflex for a short request without an analysis cue, Deep otherwise.
pub fn reasoning_gear(text: &str) -> ReasoningGear {
    let w = words(text);
    if w.len() >= REFLEX_MAX_WORDS || has_any(&format!(" {} ", w.join(" ")), DEEP_CUES) {
        ReasoningGear::Deep
    } else {
        ReasoningGear::Reflex
    }
}

// --------------------------------------------------------------- verification matrix ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentClass {
    Cancel,
    KnowledgeWorkspace(String),
    /// Windows, panels, look, orb.
    Ui,
    /// Play, pause, volume, mute, open an app.
    Media,
    /// WebUntis, timetable, calendar.
    Schedule,
    /// What is, who is, explain.
    Knowledge,
    Conversation,
}

fn has_any(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| lower.contains(n))
}

/// Sir's core verb and object, classified.
pub fn classify(text: &str) -> IntentClass {
    if is_cancel(text) {
        return IntentClass::Cancel;
    }
    if let Some(topic) = knowledge_workspace_topic(text) {
        return IntentClass::KnowledgeWorkspace(topic);
    }
    let lower = format!(" {} ", words(text).join(" "));
    if has_any(&lower, &[" webuntis ", " timetable ", " stundenplan ", " calendar ", " kalender ", " schedule ", " appointment ", " termin ", " lessons ", " event "]) {
        IntentClass::Schedule
    } else if has_any(&lower, &[" play ", " pause ", " stop the music ", " volume ", " mute ", " unmute ", " louder ", " quieter ", " skip ", " next song ", " next track ", " open ", " launch ", " start "]) && !has_any(&lower, &[" window ", " panel ", " workspace "]) {
        IntentClass::Media
    } else if has_any(&lower, &[" window ", " windows ", " bring up ", " hide ", " darker ", " brighter ", " orb mode ", " opacity ", " theme ", " panel ", " workspace ", " interface ", " hud "]) {
        IntentClass::Ui
    } else if has_any(&lower, &[" what is ", " what's ", " whats ", " who is ", " who's ", " explain ", " tell me about ", " everything you know ", " was ist ", " wer ist ", " erklär "]) {
        IntentClass::Knowledge
    } else {
        IntentClass::Conversation
    }
}

const LOOK_TOOLS: &[&str] = &["configure_interface", "control_gui_aspect", "customize_hud_and_windows", "control_jarvis_interface", "control_orb_mode", "create_or_update_widget", "remove_widget"];
const WINDOW_TOOLS: &[&str] = &[
    "spawn_native_window", "update_native_window", "close_native_window", "window_management", "move_window_to_monitor", "place_application_window", "open_jarvis_view",
    "spawn_multi_window_workspace", "control_spatial_window", "spawn_topic_knowledge_workspace", "manage_spatial_scene", "launch_interactive_simulation",
    "manage_deep_research", "open_application", "open_website", "close_all_secondary_windows",
];
const MEDIA_TOOLS: &[&str] = &["control_music", "media_and_volume_control", "control_audio_mixer", "open_application", "close_application"];
const SCHEDULE_TOOLS: &[&str] = &["create_calendar_event", "modify_or_delete_calendar_event", "get_calendar_schedule", "check_school_schedule", "reminders", "run_executive_briefing"];
const APP_TOOLS: &[&str] = &["open_application", "close_application", "open_website", "window_management", "control_music", "close_all_secondary_windows"];

/// The tools that fulfil a class (empty: no tool is required).
pub fn backing_tools(class: &IntentClass) -> &'static [&'static str] {
    match class {
        IntentClass::Ui => &["configure_interface", "control_gui_aspect", "customize_hud_and_windows", "control_jarvis_interface", "control_orb_mode", "spawn_native_window", "update_native_window", "close_native_window", "window_management", "open_jarvis_view", "spawn_multi_window_workspace", "control_spatial_window", "spawn_topic_knowledge_workspace", "manage_spatial_scene", "create_or_update_widget", "remove_widget"],
        IntentClass::Media => MEDIA_TOOLS,
        IntentClass::Schedule => SCHEDULE_TOOLS,
        IntentClass::KnowledgeWorkspace(_) => &["spawn_topic_knowledge_workspace"],
        IntentClass::Cancel | IntentClass::Knowledge | IntentClass::Conversation => &[],
    }
}

/// Whether the request was carried out by a real tool this turn (or needs none).
pub fn fulfilled(class: &IntentClass, tools_run: &[String]) -> bool {
    let needed = backing_tools(class);
    needed.is_empty() || tools_run.iter().any(|t| needed.contains(&t.as_str()))
}

// ------------------------------------------------------------------ grounding filter ----

const ACTION_VERBS: &str = "lowered|raised|increased|decreased|reduced|changed|set|adjusted|opened|closed|created|added|moved|launched|started|turned|switched|muted|unmuted|paused|played|restyled|enabled|disabled|deleted|removed|saved|sent|scheduled|made|put|brought|pulled|spawned|dimmed|brightened|hidden|hid|shown|showed|resized|maximi[sz]ed|minimi[sz]ed|lowering|raising|changing|setting|adjusting|opening|closing|creating|adding|moving|launching|starting|turning|switching|muting|pausing|playing|making|putting|bringing|pulling|sending|saving|deleting|removing|scheduling|spawning|dimming|hiding|showing|resizing|open|close|create|add|move|launch|start|turn|switch|mute|pause|play|make|put|bring|pull|send|save|delete|remove|schedule|spawn|lower|raise|change|set|adjust";

static FIRST_PERSON_CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b(?:i(?:'ve|’ve| have)|i(?:'m|’m| am)|i(?:'ll|’ll| will)|i just)\s+(?:(?:also|now|just|already|gone ahead and|taken the liberty of|quickly|then)\s+)*(?:{ACTION_VERBS})\b")).unwrap()
});
static GERUND_OPENER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:(?:got it|right|alright|very well|certainly|of course|okay|ok|sure|done)[,.!—–-]*\s+(?:sir[,.!]?\s+)?)?(?:opening|closing|creating|launching|starting|moving|lowering|raising|playing|pausing|switching|turning|adding|bringing up|pulling up|setting up|spawning|muting|unmuting|dimming|restyling)\b").unwrap()
});
static WINDOW_OFFER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:shall|should) i (?:go ahead and )?(?:create|open|make|spawn|set up|launch|bring up|pull up)\b.*\b(?:window|workspace|panel|tab|manager)\b").unwrap()
});
static NEW_WINDOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:right )?(?:into|in) a (?:new|separate|dedicated|second) window\b").unwrap());
/// Passive claims ("the dossier window should be closing now", "all windows have been closed as
/// requested"): the same roleplay without an "I".
static PASSIVE_CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b(?:should be|is being|are being|has been|have been|is now|are now|will now be|will be)\s+(?:(?:now|already|just|successfully|all)\s+)*(?:{ACTION_VERBS})\b")).unwrap()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Look,
    Window,
    Media,
    Calendar,
    App,
}

fn families(lower: &str) -> Vec<Family> {
    let mut f = Vec::new();
    if has_any(lower, &["opacity", "transparen", "darker", "brighter", "theme", "colour", "color", "accent", "background", "font", "widget", "panel", "interface", "orb"]) {
        f.push(Family::Look);
    }
    if has_any(lower, &["window", "workspace", "dossier", "graph", "pinboard", "manager", "tab", "screen"]) {
        f.push(Family::Window);
    }
    if has_any(lower, &["volume", "music", "song", "playlist", "track", "louder", "quieter", "mute", "paus", "playing", "played"]) {
        f.push(Family::Media);
    }
    if has_any(lower, &["calendar", "event", "appointment", "reminder", "termin", "timetable", "schedule"]) {
        f.push(Family::Calendar);
    }
    if has_any(lower, &["opened", "opening", "launch", "settings page", "page", "browser", "website", "app", "application", "closed", "closing"]) {
        f.push(Family::App);
    }
    f
}

fn family_tools(f: Family) -> &'static [&'static str] {
    match f {
        Family::Look => LOOK_TOOLS,
        Family::Window => WINDOW_TOOLS,
        Family::Media => MEDIA_TOOLS,
        Family::Calendar => SCHEDULE_TOOLS,
        Family::App => APP_TOOLS,
    }
}

/// A sentence that claims (or offers to create) an action no tool of this turn backs.
pub fn unbacked_claim(sentence: &str, tools_run: &[String]) -> bool {
    let claim = FIRST_PERSON_CLAIM.is_match(sentence) || PASSIVE_CLAIM.is_match(sentence) || GERUND_OPENER.is_match(sentence) || WINDOW_OFFER.is_match(sentence) || NEW_WINDOW.is_match(sentence);
    if !claim {
        return false;
    }
    let lower = sentence.to_lowercase();
    let found = families(&lower);
    if found.is_empty() {
        return tools_run.is_empty();
    }
    // Every kind of action the sentence names needs a tool of its kind.
    !found.iter().all(|f| tools_run.iter().any(|t| family_tools(*f).contains(&t.as_str())))
}

/// First words after which a sentence is held until it ends (it may be a claim).
const HOLD_OPENERS: [&str; 30] = [
    "i", "i've", "i'm", "i'll", "i’ve", "i’m", "i’ll", "opening", "closing", "creating", "launching", "starting", "moving", "lowering", "raising", "playing", "got",
    "right", "alright", "okay", "ok", "sure", "certainly", "of", "very", "done", "shall", "should", "absolutely", "understood",
];

/// Streams the model's text sentence by sentence and drops unbacked action claims. Sentences
/// that cannot be a claim pass at once (no added latency).
#[derive(Debug, Default)]
pub struct GroundingFilter {
    buf: String,
    /// The current sentence: Some(true) passes through, Some(false) is held.
    pass: Option<bool>,
    tools_run: Vec<String>,
    dropped: Vec<String>,
}

impl GroundingFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn note_tool(&mut self, name: &str) {
        if !self.tools_run.iter().any(|t| t == name) {
            self.tools_run.push(name.to_string());
        }
    }

    pub fn tools_run(&self) -> &[String] {
        &self.tools_run
    }

    /// The claims that were withheld this turn.
    pub fn dropped(&self) -> &[String] {
        &self.dropped
    }

    fn judge(&mut self, sentence: String, out: &mut String) {
        if self.pass != Some(true) && unbacked_claim(&sentence, &self.tools_run) {
            info!("[GROUND] Withheld an unbacked claim: '{}'", sentence.trim());
            self.dropped.push(sentence.trim().to_string());
        } else {
            out.push_str(&sentence);
        }
        self.pass = None;
    }

    pub fn push(&mut self, text: &str) -> String {
        let mut out = String::new();
        self.buf.push_str(text);
        loop {
            let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
            let end = chars.windows(2).find(|w| matches!(w[0].1, '.' | '!' | '?') && w[1].1.is_whitespace()).map(|w| w[1].0);
            match end {
                Some(at) => {
                    let sentence: String = self.buf[..at].to_string();
                    self.buf = self.buf[at..].to_string();
                    if self.pass == Some(true) {
                        out.push_str(&sentence);
                        self.pass = None;
                    } else {
                        self.judge(sentence, &mut out);
                    }
                }
                None => break,
            }
        }
        if self.pass.is_none() {
            let first = self.buf.split_whitespace().next().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’').to_lowercase());
            let enough = self.buf.split_whitespace().count() >= 2 || self.buf.trim().len() >= 12;
            if let Some(first) = first.filter(|_| enough) {
                self.pass = Some(!HOLD_OPENERS.contains(&first.as_str()));
            }
        }
        if self.pass == Some(true) {
            out.push_str(&std::mem::take(&mut self.buf));
        }
        out
    }

    /// The rest of the round.
    pub fn flush(&mut self) -> String {
        let mut out = String::new();
        let rest = std::mem::take(&mut self.buf);
        if self.pass == Some(true) {
            out.push_str(&rest);
            self.pass = None;
        } else if !rest.trim().is_empty() {
            self.judge(rest, &mut out);
        } else {
            out.push_str(&rest);
        }
        out
    }
}

// ------------------------------------------------------- the topic knowledge workspace ----

/// The first `n` sentences of a text.
fn sentences(text: &str, n: usize) -> Vec<String> {
    research::split_sentences(text).into_iter().filter(|s| s.split_whitespace().count() >= 4).take(n).collect()
}

/// The spoken overview: two factual sentences, never more.
pub fn compose_spoken(topic: &str, facts: &[String], installed: Option<&str>) -> String {
    let mut lines: Vec<String> = facts.iter().take(2).cloned().collect();
    if lines.len() < 2 {
        if let Some(app) = installed {
            lines.push(format!("{app} is installed on this PC, and the windows show what I can do with it."));
        }
    }
    if lines.is_empty() {
        lines.push(format!("I have no reliable facts on {topic} yet, Sir."));
    }
    if lines.len() < 2 {
        lines.push("The research windows are up and the crawler is gathering sources.".into());
    }
    lines.into_iter().take(2).map(|l| l.trim().to_string()).collect::<Vec<_>>().join(" ")
}

/// Everything Jarvis can tell about a topic right now: local memories, the installed app and
/// files, his own tools for it, Wikipedia and DuckDuckGo. Opens the three topic windows and
/// returns the display text with a two-sentence spoken overview.
pub async fn spawn_topic_knowledge_workspace(tools: &ToolContext, topic: &str) -> Result<String, String> {
    let topic = topic.trim();
    if topic.is_empty() {
        return Err("Everything about what, Sir?".into());
    }
    let lower = topic.to_lowercase();
    let wait = Duration::from_secs(5);
    let (wiki, ddg) = tokio::join!(tokio::time::timeout(wait, research::wikipedia_brief(topic)), tokio::time::timeout(wait, research::ddg_instant(topic)));
    let wiki = wiki.ok().flatten();
    let ddg = ddg.ok().flatten();
    // On this PC: the app, whether it runs, files with its name, what Jarvis can do with it.
    let app = crate::appindex::lookup(topic);
    let needle = lower.replace(' ', "");
    let running = tokio::task::spawn_blocking(move || {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
        let found = sys.processes().values().any(|p| p.name().to_string_lossy().to_lowercase().replace(' ', "").contains(&needle));
        found
    })
    .await
    .unwrap_or(false);
    let files: Vec<String> = crate::file_ops::find(topic, None, None, 5).into_iter().map(|h| h.path.display().to_string()).collect();
    let memories = tools.memory.recall(Some(topic), None, 6).unwrap_or_default();
    let mut abilities: Vec<(String, String)> = tools
        .definitions()
        .iter()
        .filter_map(|d| {
            let name = d["function"]["name"].as_str()?.to_string();
            let desc = d["function"]["description"].as_str().unwrap_or("").to_string();
            (desc.to_lowercase().contains(&lower) || name.contains(&lower.replace(' ', "_"))).then(|| (name, desc.split(". ").next().unwrap_or(&desc).chars().take(160).collect()))
        })
        .take(6)
        .collect();
    let music_app = ["spotify", "sonora", "youtube music", "apple music", "deezer", "tidal"].contains(&lower.as_str());
    if music_app && !abilities.iter().any(|(n, _)| n == "control_music") {
        abilities.push(("control_music".into(), "Play, pause, skip, search songs and playlists, and control the media session".into()));
    }

    // The overview dossier (Markdown) and the facts for the spoken summary.
    let title = wiki.as_ref().map(|w| w.title.clone()).or_else(|| ddg.as_ref().and_then(|d| (!d.heading.is_empty()).then(|| d.heading.clone()))).unwrap_or_else(|| topic.to_string());
    let mut md = format!("# {title}\n\n");
    if let Some(w) = &wiki {
        if !w.description.is_empty() {
            md.push_str(&format!("*{}*\n\n", w.description));
        }
    }
    let what = wiki.as_ref().map(|w| w.extract.clone()).filter(|e| !e.is_empty()).or_else(|| ddg.as_ref().map(|d| d.abstract_text.clone()).filter(|a| !a.is_empty()));
    md.push_str("## What it is\n\n");
    md.push_str(&format!("{}\n\n", what.clone().unwrap_or_else(|| "No encyclopedia entry was found; the live research is gathering sources.".into())));
    md.push_str("## On this PC\n\n");
    match &app {
        Some(a) => md.push_str(&format!("- Installed: {} ({})\n", a.name, a.target.display())),
        None => md.push_str("- Not found among the installed apps.\n"),
    }
    md.push_str(&format!("- Running right now: {}\n", if running { "yes" } else { "no" }));
    for f in &files {
        md.push_str(&format!("- File: {f}\n"));
    }
    if let Some(d) = &ddg {
        if !d.facts.is_empty() {
            md.push_str("\n## Key facts\n\n");
            for (k, v) in &d.facts {
                md.push_str(&format!("- {k}: {v}\n"));
            }
        }
    }
    if !abilities.is_empty() {
        md.push_str("\n## What I can do with it\n\n");
        for (name, desc) in &abilities {
            md.push_str(&format!("- {name}: {desc}\n"));
        }
    }
    if !memories.is_empty() {
        md.push_str("\n## What I remember\n\n");
        for m in &memories {
            md.push_str(&format!("- {}: {}\n", m.key, m.value));
        }
    }
    if let Some(d) = &ddg {
        if !d.related.is_empty() {
            md.push_str("\n## Related\n\n");
            for r in d.related.iter().take(8) {
                md.push_str(&format!("- {r}\n"));
            }
        }
    }
    let mut sources: Vec<String> = Vec::new();
    if let Some(w) = &wiki {
        sources.push(w.url.clone());
    }
    if let Some(u) = ddg.as_ref().map(|d| d.url.clone()).filter(|u| !u.is_empty()) {
        sources.push(u);
    }
    if !sources.is_empty() {
        md.push_str("\n## Sources\n\n");
        for s in &sources {
            md.push_str(&format!("- {s}\n"));
        }
    }

    // The live research mission behind the graph and the crawler window.
    let mission = research::start_or_reuse_mission(tools, topic, 20)?;
    let local = format!(
        "# What Jarvis knows about {title}\n\n{}\n\n{}{}{}",
        what.clone().unwrap_or_default(),
        app.as_ref().map(|a| format!("{} is installed on this computer as {} and {} running at the moment.\n\n", title, a.name, if running { "is" } else { "is not" })).unwrap_or_default(),
        abilities.iter().map(|(n, d)| format!("Jarvis can work with {title} through the {} tool: {d}.\n\n", n.replace('_', " "))).collect::<String>(),
        memories.iter().map(|m| format!("Sir's note about {title}: {} — {}.\n\n", m.key, m.value)).collect::<String>()
    );
    research::add_local_source(mission, &format!("jarvis://jarvis.local/knowledge/{}", lower.replace(' ', "-")), &format!("Jarvis knowledge: {title}"), &local, 0.75);
    if let Some(w) = &wiki {
        research::add_local_source(mission, &format!("{}#summary", w.url), &format!("{} (Wikipedia summary)", w.title), &format!("# {}\n\n{}\n\n{}", w.title, w.description, w.extract), research::reliability(&w.url));
    }
    if let Some(d) = ddg.as_ref().filter(|d| !d.abstract_text.is_empty() && !d.url.is_empty()) {
        research::add_local_source(mission, &d.url, &format!("{} (DuckDuckGo instant answer)", d.heading), &format!("# {}\n\n{}", d.heading, d.abstract_text), research::reliability(&d.url));
    }
    crate::workspace_canvas::set_brief(mission, &md);
    crate::workspace_canvas::spawn_labeled(&topic_workspace_windows(), Some(mission));

    let facts = what.as_deref().map(|w| sentences(w, 2)).unwrap_or_default();
    let spoken = compose_spoken(&title, &facts, app.as_ref().map(|a| a.name.as_str()));
    info!("[KNOWLEDGE] Workspace for '{topic}': mission #{mission}, {} source(s), app {}", sources.len(), app.is_some());
    Ok(crate::features::spoken(
        format!(
            "Knowledge workspace for {title}: overview & facts, knowledge graph and live research windows (mission #{mission}){}.",
            if sources.is_empty() { String::new() } else { format!("; sources: {}", sources.join(", ")) }
        ),
        spoken,
    ))
}

pub fn is_tool(name: &str) -> bool {
    matches!(name, "spawn_topic_knowledge_workspace" | "close_all_secondary_windows")
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "spawn_topic_knowledge_workspace",
            "description": "Bring up everything Jarvis knows about a topic: three floating windows (overview & facts dossier with what it is, whether it is installed, key facts and what Jarvis can do with it; a knowledge graph of its connections; live web research). Use for 'bring up / show me / pull up everything you know about X', 'zeig mir alles über X'.",
            "parameters": {"type": "object", "properties": {"topic": {"type": "string"}}, "required": ["topic"]}
        }}),
        json!({"type": "function", "function": {
            "name": "close_all_secondary_windows",
            "description": "Close every extra window YOU opened (workspace/research/dossier/graph windows, system stats, schedule, simulations, pop-up cards) at once. Use for 'close the windows you opened', 'close all windows', 'mach die Fenster zu'. Never closes Sir's apps or your main HUD.",
            "parameters": {"type": "object", "properties": {}}
        }}),
    ]
}

pub async fn run_tool(tools: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    if name == "close_all_secondary_windows" {
        let n = close_secondary_windows(tools);
        return Ok(format!("Closed {n} secondary window(s)."));
    }
    let topic = crate::features::str_arg(args, &["topic", "subject", "query"]).ok_or("Everything about what, Sir?")?.to_string();
    spawn_topic_knowledge_workspace(tools, &topic).await
}

// ================================================================== the reflex router ====
//
// Tier 1 of the cognitive router: Jarvis' own controls, decided in microseconds and in a fixed
// order, before the turn lock, the conversation log and the older parsers.
//
//   1. Ultron ⇄ Jarvis    "switch to Ultron", "change back" (never a track: this comes first)
//   2. the privacy lock   "stop listening", "you can listen again"
//   3. the emblem         "move your orb a little bit to the left", "center your core", "a bit more"
//   4. his own blocks     "close the HUD board / web search / feature lab", "clear the board"
//   5. the board's pages  "next page", "go back", "show result 3"
//
// One Aho-Corasick pass over the sentence (`domains`) says which vocabularies it touches; only
// the stages those allow run at all, and a sentence with no command vocabulary is dismissed at
// once. Everything else stays with `intents` and the language model. `route` itself changes
// nothing: whoever carries a reflex out reports it with `committed`.

/// The vocabularies a sentence can touch: the bit flags `domains` returns.
pub mod domain {
    pub const MODE: u32 = 1;
    pub const PRIVACY: u32 = 1 << 1;
    pub const ORB: u32 = 1 << 2;
    pub const MOTION: u32 = 1 << 3;
    pub const PLACE: u32 = 1 << 4;
    pub const CLOSE: u32 = 1 << 5;
    pub const BLOCK: u32 = 1 << 6;
    pub const PAGE: u32 = 1 << 7;
    pub const THEME: u32 = 1 << 8;
    pub const MONITOR: u32 = 1 << 9;
    pub const AUDIO: u32 = 1 << 10;
    pub const MEDIA: u32 = 1 << 11;
    pub const WINDOW: u32 = 1 << 12;
    pub const APP: u32 = 1 << 13;
    pub const SCHEDULE: u32 = 1 << 14;
    pub const MESSAGES: u32 = 1 << 15;
    pub const WEATHER: u32 = 1 << 16;
    pub const SYSTEM: u32 = 1 << 17;
    pub const POWER: u32 = 1 << 18;
    pub const SEARCH: u32 = 1 << 19;
    pub const MEMORY: u32 = 1 << 20;
    pub const CLOCK: u32 = 1 << 21;

    pub const NAMES: [(u32, &str); 22] = [
        (MODE, "mode"),
        (PRIVACY, "privacy"),
        (ORB, "orb"),
        (MOTION, "motion"),
        (PLACE, "place"),
        (CLOSE, "close"),
        (BLOCK, "block"),
        (PAGE, "page"),
        (THEME, "theme"),
        (MONITOR, "monitor"),
        (AUDIO, "audio"),
        (MEDIA, "media"),
        (WINDOW, "window"),
        (APP, "app"),
        (SCHEDULE, "schedule"),
        (MESSAGES, "messages"),
        (WEATHER, "weather"),
        (SYSTEM, "system"),
        (POWER, "power"),
        (SEARCH, "search"),
        (MEMORY, "memory"),
        (CLOCK, "clock"),
    ];

    /// "orb+motion+place".
    pub fn describe(mask: u32) -> String {
        NAMES.iter().filter(|(bit, _)| mask & bit != 0).map(|(_, name)| *name).collect::<Vec<_>>().join("+")
    }
}

/// The words (and short phrases) of each vocabulary, as they stand in a canonical sentence.
const VOCABULARY: &[(u32, &[&str])] = &[
    (
        domain::MODE,
        &[
            "ultron", "jarvis mode", "change back", "switch back", "turn back", "back to normal", "normal mode", "back to jarvis", "jarvis again", "zurück zu jarvis", "wieder jarvis",
            "normaler modus", "zurück wechseln", "wechsel zurück", "wechsle zurück",
        ],
    ),
    (domain::PRIVACY, &["listening", "listen", "microphone", "mic", "zuhören", "zuzuhören", "zuhoeren", "zuzuhoeren", "hör", "hoer"]),
    (domain::ORB, &["orb", "emblem", "core", "sphere", "kugel", "reactor", "kern"]),
    (
        domain::MOTION,
        &[
            "move", "put", "place", "drag", "bring", "send", "shift", "slide", "push", "nudge", "position", "park", "center", "centre", "recenter", "beweg", "bewege", "schieb", "schiebe",
            "setz", "setze", "pack", "packe", "verschieb", "verschiebe", "stell", "stelle", "zentrier", "zentriere",
        ],
    ),
    (
        domain::PLACE,
        &[
            "left", "right", "up", "down", "top", "bottom", "upper", "lower", "higher", "middle", "center", "centre", "corner", "links", "rechts", "oben", "unten", "mitte", "hoch", "runter",
            "home", "more", "further",
        ],
    ),
    (domain::CLOSE, &["close", "hide", "dismiss", "shut", "remove", "clear", "get rid of", "schließ", "schließe", "schliess", "schliesse", "schließen", "schliessen", "zumachen", "zu", "leere"]),
    (
        domain::BLOCK,
        &[
            "board", "hud", "block", "blocks", "web search", "search results", "search result", "feature lab", "features", "telemetry", "theme studio", "report", "window", "windows",
            "plotter", "holomat", "simulation", "sim lab", "qr", "pairing", "view", "screen", "fenster", "websuche", "everything", "console",
        ],
    ),
    (
        domain::PAGE,
        &[
            "page", "pages", "slide", "result", "results", "card", "note", "next", "previous", "back", "forward", "flip", "seite", "karte", "notiz", "weiter", "zurück", "zurueck", "nächste",
            "vorherige", "ergebnis",
        ],
    ),
    (domain::THEME, &["theme", "themes", "colour", "color", "palette", "seam", "steam", "scheme", "arc cyan", "gold", "crimson", "farbe", "thema", "darker", "brighter"]),
    (domain::MONITOR, &["monitor", "monitors", "screen", "screens", "display", "displays", "bildschirm", "bildschirme"]),
    (domain::AUDIO, &["volume", "louder", "quieter", "mute", "unmute", "lautstärke", "lauter", "leiser", "stumm", "sound"]),
    (domain::MEDIA, &["play", "pause", "resume", "skip", "song", "track", "playlist", "music", "spotify", "sonora", "musik", "lied", "stop"]),
    (domain::WINDOW, &["maximize", "maximise", "minimize", "minimise", "fullscreen", "full screen", "snap", "maximieren", "minimieren", "vollbild"]),
    (domain::APP, &["open", "launch", "start", "quit", "öffne", "starte", "beende"]),
    (
        domain::SCHEDULE,
        &[
            "timetable", "stundenplan", "webuntis", "calendar", "kalender", "lesson", "lessons", "class", "classes", "itslearning", "appointment", "termin", "schedule", "homework",
            "hausaufgaben", "unterricht",
        ],
    ),
    (domain::MESSAGES, &["email", "emails", "mail", "mails", "inbox", "whatsapp", "message", "messages", "nachricht", "nachrichten"]),
    (domain::WEATHER, &["weather", "wetter", "rain", "forecast", "temperature", "regen", "umbrella"]),
    (domain::SYSTEM, &["cpu", "gpu", "ram", "vram", "telemetry", "system", "stats", "status", "processes"]),
    (domain::POWER, &["shut down", "shutdown", "restart", "reboot", "herunterfahren", "neustart", "neu starten"]),
    (domain::SEARCH, &["search", "google", "look up", "research", "suche", "such"]),
    (domain::MEMORY, &["remember", "forget", "recall", "merk", "merke", "erinnere", "vergiss"]),
    (domain::CLOCK, &["time", "date", "clock", "uhr", "datum", "spät", "today", "tomorrow"]),
];

/// The vocabularies the reflex stages listen for.
const REFLEX_STAGES: u32 = domain::MODE | domain::PRIVACY | domain::ORB | domain::CLOSE | domain::PAGE;

struct Lexicon {
    automaton: AhoCorasick,
    /// The vocabularies of each pattern.
    bits: Vec<u32>,
}

/// Every vocabulary word as one automaton (" word ", so only whole words match); built once.
static LEXICON: LazyLock<Lexicon> = LazyLock::new(|| {
    let mut patterns: Vec<String> = Vec::new();
    let mut bits: Vec<u32> = Vec::new();
    for (bit, words) in VOCABULARY {
        for word in *words {
            let padded = format!(" {word} ");
            match patterns.iter().position(|p| *p == padded) {
                Some(i) => bits[i] |= bit,
                None => {
                    patterns.push(padded);
                    bits.push(*bit);
                }
            }
        }
    }
    let automaton = AhoCorasick::builder().match_kind(MatchKind::Standard).kind(Some(AhoCorasickKind::DFA)).build(&patterns).expect("the reflex vocabulary builds");
    Lexicon { automaton, bits }
});

/// A sentence as the automaton reads it: lower case, letters and digits only, one space between
/// words and one at each end (" move your orb left ").
pub fn canonical(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(' ');
    let mut gap = true;
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            gap = false;
        } else if matches!(c, '\'' | '’' | '`') {
            // "don't" reads "dont".
        } else if !gap {
            out.push(' ');
            gap = true;
        }
    }
    if !gap {
        out.push(' ');
    }
    out
}

/// The vocabularies a canonical sentence touches: one pass over it, nothing allocated.
pub fn domains(canon: &str) -> u32 {
    let lexicon = &*LEXICON;
    let mut mask = 0;
    for m in lexicon.automaton.find_overlapping_iter(canon) {
        mask |= lexicon.bits[m.pattern().as_usize()];
    }
    mask
}

/// What is on screen when a sentence arrives; it decides what "change back", "go back" or "next"
/// can mean. One atomic and one short lock to read, no I/O.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReflexContext {
    /// Ultron Mode is on.
    pub ultron: bool,
    /// A board is on screen.
    pub board_up: bool,
    /// One of its blocks is maximised.
    pub spotlight: bool,
    /// The board has a stack of pages to turn.
    pub paged: bool,
}

impl ReflexContext {
    pub fn live() -> Self {
        let (board_up, spotlight, paged) = crate::hud_board::reflex_flags();
        Self { ultron: crate::ultron_mode::active(), board_up, spotlight, paged }
    }
}

/// A reflex: what the router decided, for the dispatcher (mode, privacy) or the brain (silent).
#[derive(Debug, Clone, PartialEq)]
pub enum Reflex {
    /// Ultron Mode on / off.
    Mode(bool),
    /// The privacy lock.
    Privacy(crate::listening_lock::LockCommand),
    /// One of Jarvis' own controls, carried out without a word (a HUD badge only).
    Silent(SilentAction),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Routed {
    pub reflex: Reflex,
    /// The stage that answered: "mode", "privacy", "orb", "blocks" or "pages".
    pub stage: &'static str,
    /// An emblem move's step (zero for a jump to a fixed spot), remembered for "a bit more".
    orb_step: Option<[f32; 2]>,
}

/// The last emblem move: when, and its step, so "a bit more" and "the other way" know what to
/// repeat.
static LAST_ORB_MOVE: Mutex<Option<(Instant, [f32; 2])>> = Mutex::new(None);
/// How long after an emblem move a bare "more" / "further up" still means the emblem.
const FOLLOW_UP_WINDOW: Duration = Duration::from_secs(20);

fn last_orb_move() -> Option<[f32; 2]> {
    let last = LAST_ORB_MOVE.lock().unwrap_or_else(|e| e.into_inner());
    last.filter(|(at, _)| at.elapsed() <= FOLLOW_UP_WINDOW).map(|(_, step)| step)
}

/// A reflex was carried out: an emblem move opens the follow-up window, anything else closes it.
pub fn committed(routed: &Routed) {
    *LAST_ORB_MOVE.lock().unwrap_or_else(|e| e.into_inner()) = routed.orb_step.map(|step| (Instant::now(), step));
}

/// Any other request ends the emblem's follow-up window ("more" then means something else).
pub fn forget_follow_up() {
    *LAST_ORB_MOVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The command's own words: corrections and courtesy in front and politeness behind removed.
fn command_span<'a>(words: &'a [&'a str]) -> &'a [&'a str] {
    let start = words.iter().position(|w| !LEAD_FILLERS.contains(w)).unwrap_or(words.len());
    let end = words.iter().rposition(|w| !TAIL_FILLERS.contains(w)).map_or(start, |i| i + 1).max(start);
    &words[start..end]
}

/// The reflex in `text`, if it is one of Jarvis' own controls (see the stage list above).
pub fn route(text: &str, ctx: &ReflexContext) -> Option<Routed> {
    let canon = canonical(text);
    let mask = domains(&canon);
    let follow_up = last_orb_move();
    if mask & REFLEX_STAGES == 0 && follow_up.is_none() {
        return None;
    }
    let hit = |reflex: Reflex, stage: &'static str, orb_step: Option<[f32; 2]>| Some(Routed { reflex, stage, orb_step });
    let words: Vec<&str> = canon.split_ascii_whitespace().collect();
    let negated = words.iter().any(|w| matches!(*w, "dont" | "not" | "never" | "nicht" | "nie" | "kein" | "keine" | "doesnt" | "wont" | "cant"));
    // 1. Ultron ⇄ Jarvis.
    if mask & domain::MODE != 0 && !negated {
        if let Some(on) = crate::ultron_mode::parse(text, ctx.ultron) {
            return hit(Reflex::Mode(on), "mode", None);
        }
    }
    // 2. The privacy lock (its phrases carry their own "don't": "don't listen").
    if mask & domain::PRIVACY != 0 {
        if let Some(cmd) = crate::listening_lock::parse(text) {
            return hit(Reflex::Privacy(cmd), "privacy", None);
        }
    }
    if negated {
        return None;
    }
    let core = command_span(&words);
    // 3. The emblem's place, and the follow-ups to a move ("a bit more", "the other way").
    let orb = if mask & domain::ORB != 0 { orb_reflex(core) } else { follow_up.and_then(|last| orb_follow_up(core, last)) };
    if let Some((m, place)) = orb {
        let step = match m {
            crate::interface::OrbMove::Step(d) => d,
            _ => [0.0, 0.0],
        };
        let label = match m {
            crate::interface::OrbMove::Home => "ORB // CENTRE".to_string(),
            crate::interface::OrbMove::Step(_) => "ORB // NUDGED".to_string(),
            crate::interface::OrbMove::Spot(_) => format!("ORB // {}", place.to_uppercase()),
        };
        return hit(Reflex::Silent(SilentAction { tool: "configure_interface", args: json!({ "orb_board_position": place }), label }), "orb", Some(step));
    }
    // 4. Jarvis' own blocks.
    if mask & domain::CLOSE != 0 {
        if let Some(action) = block_close(core) {
            return hit(Reflex::Silent(action), "blocks", None);
        }
    }
    // 5. The board's pages.
    if mask & domain::PAGE != 0 && ctx.board_up {
        if let Some(action) = page_reflex(text, core, ctx) {
            return hit(Reflex::Silent(action), "pages", None);
        }
    }
    None
}

const ORB_NOUNS: &[&str] = &["orb", "emblem", "core", "sphere", "kugel", "reactor", "kern"];
const ORB_MOVE_VERBS: &[&str] = &[
    "move", "put", "place", "drag", "bring", "send", "shift", "slide", "push", "nudge", "position", "park", "set", "take", "beweg", "bewege", "schieb", "schiebe", "setz", "setze", "pack",
    "packe", "verschieb", "verschiebe", "stell", "stelle", "tu", "tue",
];
const ORB_CENTRE_VERBS: &[&str] = &["center", "centre", "recenter", "recentre", "zentrier", "zentriere"];
const ORB_DETERMINERS: &[&str] = &["the", "your", "my", "that", "this", "den", "deinen", "die", "deine", "dein", "der", "mein", "meinen", "meine"];

/// "[move] [your] orb <place>": the emblem's move and its place phrase. Only a verb and the
/// noun's own small words may stand before the noun ("show me your core" is the core view), and
/// everything after it must be a place on the board ("… of monitor 2" is the floating orb).
fn orb_reflex(core: &[&str]) -> Option<(crate::interface::OrbMove, String)> {
    use crate::interface::{orb_move, OrbMove};
    let at = core.iter().position(|w| ORB_NOUNS.contains(w))?;
    let (before, after) = (&core[..at], &core[at + 1..]);
    let mut centring = false;
    for w in before {
        if ORB_CENTRE_VERBS.contains(w) {
            centring = true;
        } else if !ORB_MOVE_VERBS.contains(w) && !ORB_DETERMINERS.contains(w) {
            return None;
        }
    }
    if after.is_empty() {
        // "Center your orb."
        return centring.then(|| (OrbMove::Home, "center".to_string()));
    }
    let place = after.join(" ");
    orb_move(&place).map(|m| (m, place))
}

/// The words a follow-up to an emblem move may consist of; anything else is a new request.
const FOLLOW_UP_WORDS: &[&str] = &[
    "a", "little", "bit", "more", "further", "farther", "again", "to", "the", "same", "way", "other", "opposite", "direction", "left", "right", "up", "down", "higher", "lower", "and",
    "noch", "mehr", "weiter", "etwas", "bisschen", "ein", "nach", "links", "rechts", "hoch", "runter", "tiefer", "höher", "slightly", "much", "lot", "far", "keep", "going", "nochmal",
    "mal", "stück", "andere", "richtung", "wrong", "tad", "just", "one", "time", "once", "it",
];

/// A step as a place phrase `interface::orb_move` reads back ("a bit left", "way more right up").
fn step_phrase(step: [f32; 2]) -> String {
    let size = step[0].abs().max(step[1].abs());
    let amount = if size <= (crate::interface::ORB_SMALL_STEP + crate::interface::ORB_STEP) / 2.0 {
        "a bit"
    } else if size <= (crate::interface::ORB_STEP + crate::interface::ORB_BIG_STEP) / 2.0 {
        "more"
    } else {
        "way more"
    };
    let mut phrase = amount.to_string();
    if step[0].abs() > 1e-4 {
        phrase.push_str(if step[0] < 0.0 { " left" } else { " right" });
    }
    if step[1].abs() > 1e-4 {
        phrase.push_str(if step[1] < 0.0 { " up" } else { " down" });
    }
    phrase
}

/// "A bit more", "further left", "again", "the other way" within the follow-up window of an
/// emblem move whose step was `last`.
fn orb_follow_up(core: &[&str], last: [f32; 2]) -> Option<(crate::interface::OrbMove, String)> {
    use crate::interface::{orb_move, OrbMove};
    if core.is_empty() || core.len() > 7 || !core.iter().all(|w| FOLLOW_UP_WORDS.contains(w)) {
        return None;
    }
    let has = |list: &[&str]| core.iter().any(|w| list.contains(w));
    let moved = last != [0.0, 0.0];
    if has(&["other", "opposite", "andere", "wrong"]) {
        // Back past where it started, as far again on the other side.
        let phrase = step_phrase([-last[0] * 2.0, -last[1] * 2.0]);
        return orb_move(&phrase).filter(|_| moved).map(|m| (m, phrase));
    }
    let phrase = core.join(" ");
    match orb_move(&phrase) {
        Some(m @ OrbMove::Step(_)) => Some((m, phrase)),
        // "A bit more", "again", "keep going": the same step once more.
        None if moved && has(&["more", "further", "farther", "again", "mehr", "weiter", "nochmal", "going", "same"]) => {
            let phrase = step_phrase(last);
            orb_move(&phrase).map(|m| (m, phrase))
        }
        _ => None,
    }
}

const CLOSE_VERBS: &[&str] = &["close", "hide", "dismiss", "shut", "remove", "schließ", "schließe", "schliess", "schliesse"];

/// Jarvis' own views by the names Sir uses for them. Closing one never asks Windows for a
/// program called "Feature Lab".
fn is_own_view(name: &str) -> bool {
    matches!(
        name,
        "feature lab" | "features" | "feature list" | "telemetry" | "system telemetry" | "system stats" | "pc stats" | "stats" | "theme studio" | "plotter" | "function plotter" | "simulation"
            | "simulation lab" | "sim lab" | "holomat" | "qr" | "qr code" | "phone qr" | "pairing" | "schedule" | "timetable" | "chronos" | "discord routing" | "audio routing"
    )
}

/// "Close the HUD board", "close to web search", "hide the feature lab window", "mach das Board
/// zu", "clear the board": the board, one of Jarvis' own blocks, or everything on the canvas.
/// None for anything that is not his ("close Spotify", "hide the system panel").
fn block_close(core: &[&str]) -> Option<SilentAction> {
    let (object, clearing): (&[&str], bool) = match core {
        ["get", "rid", "of", rest @ ..] => (rest, false),
        ["mach" | "mache", rest @ .., "zu"] => (rest, false),
        ["clear" | "leere", rest @ ..] => (rest, true),
        [rest @ .., "leeren"] if !rest.is_empty() => (rest, true),
        [rest @ .., "schließen" | "schliessen" | "zumachen"] if rest.first().is_some_and(|w| !CLOSE_VERBS.contains(w)) => (rest, false),
        [verb, rest @ ..] if CLOSE_VERBS.contains(verb) => (rest, false),
        _ => return None,
    };
    let mut object: Vec<&str> = object
        .iter()
        .copied()
        .filter(|w| !matches!(*w, "the" | "to" | "your" | "that" | "this" | "my" | "up" | "down" | "away" | "again" | "now" | "das" | "die" | "den" | "dein" | "deine" | "wieder" | "mal" | "whole" | "entire"))
        .collect();
    // A HUD panel is the layout's ("hide the system panel"), not a block.
    if object.is_empty() || object.iter().any(|w| matches!(*w, "panel" | "panels" | "tab" | "tabs" | "sidebar" | "section")) {
        return None;
    }
    // "The feature lab window": the last word names the kind, not the thing.
    while object.len() > 1 && matches!(object[object.len() - 1], "block" | "blocks" | "window" | "windows" | "card" | "cards" | "view" | "fenster") {
        object.pop();
    }
    let name = object.join(" ");
    let silent = |args: Value, label: &str| Some(SilentAction { tool: "hud_board", args, label: label.to_string() });
    if clearing {
        // "Clear my calendar" / "clear the chat" are other things entirely.
        return matches!(name.as_str(), "board" | "hud" | "hud board" | "view" | "screen" | "display" | "blocks" | "everything" | "all" | "canvas" | "bildschirm").then(|| silent(json!({ "action": "clear" }), "CLEARED")).flatten();
    }
    if crate::hud_board::is_board_name(&name) {
        return silent(json!({ "action": "close" }), "BOARD CLOSED");
    }
    if is_own_view(&name) {
        return silent(json!({ "action": "close", "target": name }), "CLOSED");
    }
    None
}

/// `block_close` on a sentence as Sir said it.
pub fn close_command(text: &str) -> Option<SilentAction> {
    let canon = canonical(text);
    let words: Vec<&str> = canon.split_ascii_whitespace().collect();
    if words.iter().any(|w| matches!(*w, "dont" | "not" | "never" | "nicht" | "nie")) {
        return None;
    }
    block_close(command_span(&words))
}

/// "Next page", "previous result", "show result 3", and — only while a stack of pages is on
/// the board — a bare "next", "back", "go back", "weiter", "zurück" (otherwise those are the
/// music player's).
fn page_reflex(text: &str, core: &[&str], ctx: &ReflexContext) -> Option<SilentAction> {
    let bare = match core {
        ["next"] | ["forward"] | ["go", "forward"] | ["weiter"] => Some("next"),
        ["previous"] | ["back"] | ["go", "back"] | ["zurück"] | ["zurueck"] => Some("previous"),
        _ => None,
    };
    if let Some(direction) = bare {
        return ctx.paged.then(|| SilentAction { tool: "hud_board", args: json!({ "action": "flip_note", "direction": direction }), label: "PAGE".to_string() });
    }
    match crate::intents::board_command(text, true, ctx.spotlight)? {
        Intent::Silent(action) if action.tool == "hud_board" && action.args["action"] == "flip_note" => Some(action),
        _ => None,
    }
}

// ------------------------------------------------------------------ phonetic rescue ----
//
// Whisper sometimes writes a command as the words it sounds like: "fight the core" (hide the
// core), "Jarvis disabled the car" (disable the core), "which do old run" (switch to Ultron),
// "10 discord call on" (turn …). When a short spoken sentence found no route at all, its sound
// is compared with the sound of the commands that make sense right now; a near-identical one
// (and only one) is what Sir said.

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'ä' | 'ö' | 'ü')
}

/// One word's sound appended to `out`: consonants by family (b/p → p, d/t/th → t, g/k/c → k,
/// f/v → f, s/z → s, sh/ch → x), vowel groups by colour (a, e, i, o), silent letters and the
/// non-rhotic r dropped.
fn word_sound(word: &str, out: &mut String) {
    match word {
        // Whisper writes digits for the words that sound like them.
        "10" => return out.push_str("ten"),
        "2" | "to" | "too" | "two" | "do" => return out.push_str("to"),
        "4" | "for" | "four" => return out.push_str("fo"),
        "the" => return out.push_str("te"),
        "your" | "youre" => return out.push_str("yo"),
        _ => {}
    }
    let all: Vec<char> = word.chars().collect();
    // A silent final e ("hide", "core", "page") — it softens a g or c and lengthens an a.
    let magic = all.len() > 2 && all[all.len() - 1] == 'e' && !is_vowel(all[all.len() - 2]) && all[..all.len() - 2].iter().any(|c| is_vowel(*c));
    let c = if magic { &all[..all.len() - 1] } else { &all[..] };
    let mut i = 0;
    while i < c.len() {
        let rest = &c[i..];
        let last = i + 1 == c.len();
        let (sound, used): (&str, usize) = match rest {
            ['t', 'i', 'o', 'n', ..] => ("xn", 4),
            ['e', 'i', 'g', 'h', ..] => ("e", 4),
            ['o', 'u', 'g', 'h', ..] | ['a', 'u', 'g', 'h', ..] => ("o", 4),
            ['i', 'g', 'h', ..] => ("i", 3),
            ['s', 'c', 'h', ..] | ['t', 'c', 'h', ..] => ("x", 3),
            ['d', 'g', ..] => ("j", 2),
            ['p', 'h', ..] => ("f", 2),
            ['w', 'h', ..] => ("w", 2),
            ['c', 'k', ..] => ("k", 2),
            ['q', 'u', ..] => ("kw", 2),
            ['t', 'h', ..] => ("t", 2),
            ['s', 'h', ..] | ['c', 'h', ..] => ("x", 2),
            ['g', 'h', ..] if i > 0 => ("", 2),
            ['k', 'n', ..] if i == 0 => ("n", 2),
            ['w', 'r', ..] if i == 0 => ("r", 2),
            ['c', n, ..] if matches!(n, 'e' | 'i' | 'y') => ("s", 1),
            ['c'] if magic => ("s", 1),
            ['g'] if magic && i > 0 => ("j", 1),
            ['b', ..] | ['p', ..] => ("p", 1),
            ['d', ..] | ['t', ..] => ("t", 1),
            ['g', ..] | ['k', ..] | ['c', ..] | ['q', ..] => ("k", 1),
            ['f', ..] | ['v', ..] => ("f", 1),
            ['s', ..] | ['z', ..] | ['ß', ..] => ("s", 1),
            ['x', ..] => ("ks", 1),
            ['j', ..] => ("j", 1),
            ['m', ..] => ("m", 1),
            ['n', ..] => ("n", 1),
            ['l', ..] => ("l", 1),
            ['w', ..] => ("w", 1),
            // Non-rhotic: an r after a vowel and before a consonant or the end is silent.
            ['r', ..] => (if i > 0 && is_vowel(c[i - 1]) && (last || !is_vowel(c[i + 1])) { "" } else { "r" }, 1),
            ['h', ..] => (if i == 0 { "h" } else { "" }, 1),
            ['y', n, ..] if i == 0 && is_vowel(*n) => ("y", 1),
            [v, ..] if is_vowel(*v) => {
                let run = rest.iter().take_while(|ch| is_vowel(**ch)).count();
                let group: String = rest[..run].iter().collect();
                // "ar" / "or" keep their vowel; "er", "ir", "ur" all sound alike ("turn", "first").
                let r_coloured = c.get(i + run) == Some(&'r') && c.get(i + run + 1).is_none_or(|n| !is_vowel(*n));
                let colour = match group.as_str() {
                    "a" if magic && i + 2 == c.len() => "e",
                    "au" | "aw" => "o",
                    "ai" | "ay" | "ei" | "ey" => "e",
                    "ee" | "ea" | "ie" => "i",
                    "eu" => "o",
                    g if g.starts_with('a') => "a",
                    g if g.starts_with('e') || g.starts_with('ä') => "e",
                    g if g.starts_with('i') || g.starts_with('y') => "i",
                    _ => "o",
                };
                let colour = if r_coloured && (group.starts_with('u') || !matches!(colour, "a" | "o")) { "e" } else { colour };
                out.push_str(colour);
                i += run;
                continue;
            }
            _ => ("", 1),
        };
        out.push_str(sound);
        i += used;
    }
}

/// The sound of a sentence, its words run together ("do old run" sounds like "to Ultron") and
/// doubled sounds collapsed.
pub fn sound_key(text: &str) -> String {
    let mut raw = String::new();
    for word in text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        word_sound(&word.to_lowercase(), &mut raw);
    }
    let mut key = String::with_capacity(raw.len());
    for c in raw.chars() {
        if !key.ends_with(c) {
            key.push(c);
        }
    }
    key
}

/// How alike two sound keys are (1 = identical): an edit distance in which mistaking one vowel
/// for another, m for n, s for sh or f for h costs half a step.
pub fn sound_similarity(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o');
    let swap = |x: char, y: char| -> f32 {
        if x == y {
            0.0
        } else if (vowel(x) && vowel(y)) || matches!((x, y), ('m', 'n') | ('n', 'm') | ('s', 'x') | ('x', 's') | ('f', 'h') | ('h', 'f')) {
            0.5
        } else {
            1.0
        }
    };
    let mut row: Vec<f32> = (0..=b.len()).map(|j| j as f32).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = (i + 1) as f32;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + swap(*x, *y)).min(above + 1.0).min(row[j] + 1.0);
            diagonal = above;
        }
    }
    1.0 - row[b.len()] / a.len().max(b.len()) as f32
}

/// A command Whisper is known to mishear: how it sounds, what to run instead, and when it
/// makes sense at all.
struct Rescue {
    sounds_like: &'static str,
    means: &'static str,
    when: fn(&ReflexContext) -> bool,
}

const RESCUES: &[Rescue] = &[
    Rescue { sounds_like: "switch to ultron", means: "switch to Ultron", when: |c| !c.ultron },
    Rescue { sounds_like: "switch back to jarvis", means: "switch back to Jarvis", when: |c| c.ultron },
    Rescue { sounds_like: "change back to jarvis", means: "switch back to Jarvis", when: |c| c.ultron },
    Rescue { sounds_like: "hide the core", means: "hide your core", when: |_| true },
    Rescue { sounds_like: "hide your core", means: "hide your core", when: |_| true },
    Rescue { sounds_like: "close your core", means: "hide your core", when: |_| true },
    Rescue { sounds_like: "disable the core", means: "hide your core", when: |_| true },
    Rescue { sounds_like: "show the core", means: "show me your core", when: |_| true },
    Rescue { sounds_like: "show me your core", means: "show me your core", when: |_| true },
    Rescue { sounds_like: "open your core", means: "show me your core", when: |_| true },
    Rescue { sounds_like: "close the hud board", means: "close the HUD board", when: |c| c.board_up },
    Rescue { sounds_like: "next page", means: "next page", when: |c| c.paged },
    Rescue { sounds_like: "previous page", means: "previous page", when: |c| c.paged },
    Rescue { sounds_like: "move your orb to the middle", means: "move your orb to the middle", when: |_| true },
    Rescue { sounds_like: "center your orb", means: "center your orb", when: |_| true },
    Rescue { sounds_like: "turn discord call on", means: "turn on the discord call", when: |_| true },
];

/// A spoken sentence must sound this much like a command to be read as it …
pub const RESCUE_SIMILARITY: f32 = 0.84;
/// … and this much more like it than like the next different command.
const RESCUE_MARGIN: f32 = 0.05;

/// What Sir most likely said, when a short spoken sentence that matched no command sounds almost
/// exactly like one that makes sense right now ("fight the core" → "hide your core"). None when
/// nothing is that close, or when two different commands are.
pub fn phonetic_rescue(text: &str, ctx: &ReflexContext) -> Option<&'static str> {
    let canon = canonical(text);
    let words: Vec<&str> = canon.split_ascii_whitespace().collect();
    let core = command_span(&words);
    if !(2..=6).contains(&core.len()) {
        return None;
    }
    let key = sound_key(&core.join(" "));
    if key.len() < 5 {
        return None;
    }
    let scored: Vec<(&'static str, f32)> = RESCUES.iter().filter(|r| (r.when)(ctx)).map(|r| (r.means, sound_similarity(&key, &sound_key(r.sounds_like)))).collect();
    let (means, score) = scored.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    // The closest command that would do something else.
    let rival = scored.iter().filter(|(m, _)| *m != means).map(|(_, s)| *s).fold(0.0f32, f32::max);
    (score >= RESCUE_SIMILARITY && score - rival >= RESCUE_MARGIN).then_some(means)
}

// ------------------------------------------------------------------------ benchmark ----

/// What the routing benchmark feeds the router: Jarvis' own controls, other commands, questions
/// and chatter, English and German.
pub const BENCH_CORPUS: &[&str] = &[
    "Jarvis, switch to Ultron.",
    "Change back to Jarvis.",
    "Ultron, switch back.",
    "Please stop listening for five minutes.",
    "You can listen again now.",
    "Move your orb back to the middle.",
    "Move your orb a little bit to the left.",
    "Put your core in the top right corner.",
    "Center your orb.",
    "Schieb den Orb etwas nach rechts.",
    "Close the HUD board, please.",
    "Close to web search.",
    "Close the feature lab.",
    "Hide the telemetry window.",
    "Clear the board.",
    "Next page.",
    "Go back.",
    "Show result 3.",
    "Previous page, please.",
    "Nächste Seite.",
    "Close Spotify.",
    "Open my browser.",
    "Set the volume to 40 percent.",
    "Pause the music.",
    "Play my JaysRock playlist on Sonora.",
    "Move to my third monitor.",
    "Use all monitors.",
    "Change the theme to Arc Cyan.",
    "What classes do I have tomorrow?",
    "Was steht morgen im Stundenplan?",
    "Do I have any new emails?",
    "What's the weather like in Musterstadt?",
    "How much VRAM is the language model using right now?",
    "Remember that my last name is Mustermann.",
    "What time is it?",
    "Explain in detail how a transformer network computes attention.",
    "Ich glaube, dass die Keycaps nicht für die Switches gemacht sind, ist das Problem.",
    "Yes.",
    "Thanks, that will be all.",
    "Can you write a Rust function that reverses a linked list and explain the borrow checker errors I might hit?",
];

/// Routing latency over the benchmark corpus, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteBench {
    pub samples: usize,
    /// How many sentences of one pass a reflex stage answered.
    pub routed: usize,
    pub p50_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
}

impl RouteBench {
    pub fn describe(&self) -> String {
        format!("{} routings: p50 {:.1} µs, p99 {:.1} µs, worst {:.1} µs ({} of {} sentences were reflexes)", self.samples, self.p50_us, self.p99_us, self.max_us, self.routed, BENCH_CORPUS.len())
    }
}

/// Times `decide` on every sentence of the corpus, `rounds` times over (after one warm-up pass,
/// so the automaton and every pattern are compiled, as they are after `intents::warm_up`).
fn bench_with(rounds: usize, mut decide: impl FnMut(&str) -> bool) -> RouteBench {
    let mut routed = 0;
    for sentence in BENCH_CORPUS {
        routed += decide(sentence) as usize;
    }
    let mut times: Vec<f64> = Vec::with_capacity(rounds.max(1) * BENCH_CORPUS.len());
    for _ in 0..rounds.max(1) {
        for sentence in BENCH_CORPUS {
            let started = Instant::now();
            std::hint::black_box(decide(std::hint::black_box(sentence)));
            times.push(started.elapsed().as_secs_f64() * 1e6);
        }
    }
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    RouteBench { samples: times.len(), routed, p50_us: at(0.5), p99_us: at(0.99), max_us: at(1.0) }
}

/// The reflex router alone (a board with a stack of pages up, Ultron off).
pub fn benchmark(rounds: usize) -> RouteBench {
    let ctx = ReflexContext { ultron: false, board_up: true, spotlight: false, paged: true };
    bench_with(rounds, |sentence| route(sentence, &ctx).is_some())
}

/// The whole deterministic path of a turn: the reflex router, then (when it passes) the older
/// parsers of `intents`.
pub fn benchmark_full(rounds: usize) -> RouteBench {
    let ctx = ReflexContext { ultron: false, board_up: true, spotlight: false, paged: true };
    bench_with(rounds, |sentence| route(sentence, &ctx).is_some() || crate::intents::silent_quick_command(sentence).is_some() || crate::intents::parse(sentence).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_requests_take_the_reflex_gear_and_analysis_the_deep_one() {
        use ReasoningGear::*;
        for quick in ["What's the capital of France?", "Make the interface the typical Jarvis blue", "Tell me a joke", "Wie spät ist es?"] {
            assert_eq!(reasoning_gear(quick), Reflex, "{quick}");
        }
        for heavy in [
            "Think carefully about whether I should buy a new GPU",
            "Analyze this function",
            "Explain in detail how a transformer works",
            "Warum ist der Himmel blau?",
            "Berechne das Integral von x²",
            "Compare Rust and Go for a web server",
            "Write a short poem about the sea",
            "so tell me what you think about the weather today and the plans I have for the whole long weekend",
        ] {
            assert_eq!(reasoning_gear(heavy), Deep, "{heavy}");
        }
    }

    #[test]
    fn screenshot_bug_close_the_windows_you_opened_is_deterministic() {
        // The patterns compile once at startup (`intents::warm_up`); the decision itself is timed.
        let _ = deterministic_action("warm up the patterns");
        let started = std::time::Instant::now();
        assert_eq!(deterministic_action("No. Just can you close the windows you opened?"), Some(IntentAction::CloseSecondaryWindows));
        assert!(started.elapsed() < Duration::from_millis(15), "decided in {:?}", started.elapsed());
        for yes in [
            "close the windows you opened",
            "Close those windows.",
            "close all windows",
            "close the research windows",
            "close the extra windows",
            "close subwindows",
            "Wait, close everything you opened",
            "schließ die Fenster",
            "Nein, einfach die Fenster wieder zumachen",
            "mach die Fenster wieder zu",
            "Fenster schließen",
            "Jarvis, could you please close all of your windows?",
        ] {
            assert_eq!(deterministic_action(yes), Some(IntentAction::CloseSecondaryWindows), "{yes}");
        }
        for no in ["close Discord", "close the Chrome windows", "don't close the windows", "what windows did you open?", "open the windows settings", "No."] {
            assert_ne!(deterministic_action(no), Some(IntentAction::CloseSecondaryWindows), "{no}");
        }
        assert_eq!(command_core("No. Just can you close the windows you opened?"), "close the windows you opened");
        assert_eq!(closed_count("Closed 3 window(s): a, b, c."), 3);
    }

    #[test]
    fn german_and_english_self_close_exit_jarvis() {
        for yes in [
            "schließ dich", "Schließe dich", "beende dich", "mach dich zu", "schalt dich aus", "schalte dich ab", "Jarvis beenden", "Jarvis schließen",
            "Programm beenden", "App schließen", "geh aus", "Nein, beende dich bitte", "close yourself", "shut yourself down", "exit Jarvis", "quit jarvis",
            "terminate yourself", "turn yourself off", "Jarvis, could you close yourself please",
        ] {
            assert_eq!(deterministic_action(yes), Some(IntentAction::ExitJarvisApplication), "{yes}");
        }
        for no in ["don't close yourself", "schließ Discord", "beende Spotify", "close the app store website", "shut down my PC", "nicht beenden"] {
            assert_ne!(deterministic_action(no), Some(IntentAction::ExitJarvisApplication), "{no}");
        }
    }

    #[test]
    fn passive_roleplay_claims_are_withheld() {
        let none: Vec<String> = Vec::new();
        assert!(unbacked_claim("The overview & facts dossier window should be closing now, followed by the knowledge graph window.", &none));
        assert!(unbacked_claim("All related Jarvis windows have been closed as requested.", &none));
        let closed = vec!["close_all_secondary_windows".to_string()];
        assert!(!unbacked_claim("All related Jarvis windows have been closed as requested.", &closed));
        assert!(!unbacked_claim("The window has a dark theme.", &none));
    }

    #[test]
    fn cancel_detector_catches_misheard_and_german_cancels() {
        for yes in [
            "No fool forgets that idea.",
            "No, forget that idea",
            "forget it",
            "Never mind.",
            "nevermind jarvis",
            "scratch that",
            "cancel that",
            "Abort!",
            "Vergiss es",
            "vergiss das bitte",
            "egal",
            "doch nicht",
            "no full forget that",
        ] {
            assert!(is_cancel(yes), "{yes}");
        }
        for no in ["cancel the timer", "forget that I like jazz", "abort the shutdown and restart", "never mind the weather, play music", "what is egal in English"] {
            assert!(!is_cancel(no), "{no}");
        }
        assert_eq!(classify("No fool forgets that idea."), IntentClass::Cancel);
        assert!(!CANCEL_REPLY.to_lowercase().contains("bookmark") && !CANCEL_REPLY.contains("window"));
    }

    #[test]
    fn knowledge_requests_route_to_the_topic_workspace() {
        assert_eq!(knowledge_workspace_topic("Can you bring up everything you know about Spotify?").as_deref(), Some("Spotify"));
        assert_eq!(knowledge_workspace_topic("show me everything on solid-state batteries").as_deref(), Some("solid-state batteries"));
        assert_eq!(knowledge_workspace_topic("Pull up all data on the RTX 3060 Ti").as_deref(), Some("RTX 3060 Ti"));
        assert_eq!(knowledge_workspace_topic("research quantum computing and bring up the windows").as_deref(), Some("quantum computing"));
        assert_eq!(knowledge_workspace_topic("Zeig mir alles über Musterstadt").as_deref(), Some("Musterstadt"));
        assert_eq!(knowledge_workspace_topic("open Spotify"), None);
        assert_eq!(classify("Can you bring up everything you know about Spotify?"), IntentClass::KnowledgeWorkspace("Spotify".into()));
        let windows = topic_workspace_windows();
        assert_eq!(windows.len(), 3);
        assert!(windows.iter().any(|(k, _)| *k == SpatialWindowKind::KnowledgeGraphPinboard));
        let spoken = compose_spoken("Spotify", &["Spotify is a Swedish audio streaming service.".into(), "It was founded in 2006.".into(), "A third sentence.".into()], Some("Spotify"));
        assert_eq!(spoken, "Spotify is a Swedish audio streaming service. It was founded in 2006.");
        assert!(!spoken.to_lowercase().contains("opacity"));
        assert_eq!(compose_spoken("Foo", &[], None).matches(". ").count(), 1);
    }

    #[test]
    fn new_requests_are_isolated_and_follow_ups_keep_two_exchanges() {
        assert!(isolate_turn("Can you bring up everything you know about Spotify?"));
        assert!(isolate_turn("what is the capital of australia"));
        for follow in ["make it bigger", "yes", "full screen", "what about tomorrow?", "and put that in a window", "close this", "do the same for Discord"] {
            assert!(is_follow_up(follow), "{follow}");
        }
        let msg = |role: &str| json!({ "role": role, "content": "x" });
        let history = vec![msg("user"), msg("assistant"), msg("user"), msg("tool"), msg("assistant"), msg("user"), msg("assistant"), msg("user")];
        assert_eq!(history_start(&history, true), 7);
        assert_eq!(history_start(&history, false), 2);
        assert_eq!(history_start(&history[..3], false), 0);
    }

    #[test]
    fn grounding_filter_withholds_hallucinated_actions() {
        // Screenshot 1: no tool ran.
        let mut g = GroundingFilter::new();
        let mut out = String::new();
        for chunk in ["Of course, Sir. I have lowered the opac", "ity of that widget slightly and opened Spotify's settings page where you can manage your playlists. ", "Would you like me to show any specific information related to Spotify?"] {
            out.push_str(&g.push(chunk));
        }
        out.push_str(&g.flush());
        assert!(!out.contains("opacity") && !out.contains("settings page"), "{out}");
        assert!(out.starts_with("Of course, Sir.") && out.contains("Would you like me to show"), "{out}");
        assert_eq!(g.dropped().len(), 1);
        // Screenshot 2: the bookmark manager.
        let mut g = GroundingFilter::new();
        let mut out = g.push("Got it, right into a new window then. Opening the bookmark manager separately from your browser tabs should be straightforward—shall I create this dedicated window now?");
        out.push_str(&g.flush());
        assert!(out.trim().is_empty(), "{out}");
        // A real tool backs the claim.
        let mut g = GroundingFilter::new();
        g.note_tool("control_gui_aspect");
        let mut out = g.push("I have lowered the opacity to sixty percent, Sir. ");
        out.push_str(&g.flush());
        assert!(out.contains("lowered the opacity"));
        // Plain information passes untouched.
        let mut g = GroundingFilter::new();
        let mut out = g.push("Spotify is a Swedish streaming service with over 600 million users. I'm afraid I can't see your account.");
        out.push_str(&g.flush());
        assert_eq!(out, "Spotify is a Swedish streaming service with over 600 million users. I'm afraid I can't see your account.");
    }

    #[test]
    fn verification_matrix() {
        assert_eq!(classify("open the hardware window"), IntentClass::Ui);
        assert_eq!(classify("pause the music"), IntentClass::Media);
        assert_eq!(classify("what's on my timetable tomorrow"), IntentClass::Schedule);
        assert_eq!(classify("who is Tony Stark"), IntentClass::Knowledge);
        assert!(!fulfilled(&IntentClass::Media, &[]));
        assert!(fulfilled(&IntentClass::Media, &["control_music".into()]));
        assert!(fulfilled(&IntentClass::Knowledge, &[]));
        assert_eq!(tool_marker("control_music"), "[TOOL_EXECUTED: control_music]");
        assert!(GROUNDING_RULE.contains("[TOOL_EXECUTED: ...]"));
    }

    #[test]
    fn calendar_request_extracts_title_time_and_date() {
        let typed = "add a new in my calender called test for 15h tommorw";
        let text = crate::typos::normalize_user_input_typos(typed);
        assert_eq!(classify(&text), IntentClass::Schedule);
        let route = crate::integration_router::route_text(&text).expect("calendar route");
        assert_eq!(route.tool, "create_calendar_event");
        let tomorrow = (chrono::Local::now().date_naive() + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        assert_eq!(route.args["title"], "Test");
        assert_eq!(route.args["start_time"].as_str(), Some(format!("{tomorrow} 15:00").as_str()));
        assert!(fulfilled(&classify(&text), &[route.tool.to_string()]));
    }

    // ------------------------------------------------------------------ reflex router ----

    const IDLE: ReflexContext = ReflexContext { ultron: false, board_up: false, spotlight: false, paged: false };
    const ULTRON: ReflexContext = ReflexContext { ultron: true, board_up: false, spotlight: false, paged: false };
    const BOARD: ReflexContext = ReflexContext { ultron: false, board_up: true, spotlight: false, paged: false };
    const PAGED: ReflexContext = ReflexContext { ultron: false, board_up: true, spotlight: false, paged: true };

    /// The emblem's follow-up window is global: tests that open it, or that route the bare words
    /// it listens for ("weiter", "more"), run one at a time.
    fn follow_up_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: Mutex<()> = Mutex::new(());
        L.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The stage and, for a silent reflex, its tool arguments.
    fn reflex(text: &str, ctx: &ReflexContext) -> Option<(&'static str, Value)> {
        route(text, ctx).map(|r| {
            let args = match r.reflex {
                Reflex::Silent(a) => json!({ "tool": a.tool, "args": a.args }),
                Reflex::Mode(on) => json!({ "ultron": on }),
                Reflex::Privacy(cmd) => json!({ "privacy": format!("{cmd:?}") }),
            };
            (r.stage, args)
        })
    }

    #[test]
    fn the_automaton_reads_whole_words_in_one_pass() {
        assert_eq!(canonical("  Move your ORB,  left!  "), " move your orb left ");
        assert_eq!(canonical("Don't close it."), " dont close it ");
        assert_eq!(canonical("…"), " ");
        let mask = domains(&canonical("Move your orb a little bit to the left"));
        assert_eq!(mask & (domain::ORB | domain::MOTION | domain::PLACE), domain::ORB | domain::MOTION | domain::PLACE);
        assert_eq!(domain::describe(domains(&canonical("close the HUD board"))), "close+block");
        // Whole words only: "explore" has no "lore", "orbit" no "orb", "corner" no "core".
        assert_eq!(domains(&canonical("orbital mechanics explore corners")), 0);
        // Phrases of two words, and words that belong to two vocabularies.
        assert_ne!(domains(&canonical("change back")) & domain::MODE, 0);
        assert_ne!(domains(&canonical("what's on my timetable tomorrow")) & domain::SCHEDULE, 0);
        assert_eq!(domains(&canonical("I think the keycaps are the problem")), 0, "chatter touches nothing");
    }

    #[test]
    fn ultron_and_back_is_stage_one_before_music_or_anything_else() {
        assert_eq!(reflex("Jarvis, switch to Ultron.", &IDLE), Some(("mode", json!({"ultron": true}))));
        // The screenshot: every way Sir asked to come back.
        for back in ["Change back to Jarvis.", "Ultron switch back to Jarvis.", "Switch back to Jarvis", "change back", "Ultron, switch back.", "No, change back to Jarvis.", "Zurück zu Jarvis", "normal mode"] {
            assert_eq!(reflex(back, &ULTRON), Some(("mode", json!({"ultron": false}))), "{back}");
            // And it is never the music player's "back" / "change …".
            assert!(crate::intents::silent_quick_command(back).is_none() || route(back, &ULTRON).is_some(), "{back}");
        }
        // Outside Ultron Mode a bare "change back" is not his to take.
        assert_eq!(reflex("change back", &IDLE), None);
        assert_eq!(reflex("change back to the previous track", &ULTRON), None, "a track is still music");
        assert_eq!(reflex("Don't switch to Ultron", &IDLE), None);
        assert_eq!(reflex("What is Ultron?", &IDLE), None);
    }

    #[test]
    fn the_privacy_lock_is_stage_two() {
        assert_eq!(reflex("Jarvis, please stop listening.", &IDLE), Some(("privacy", json!({"privacy": "Lock"}))));
        assert_eq!(reflex("You can listen again now.", &IDLE), Some(("privacy", json!({"privacy": "Resume"}))));
        assert_eq!(route("Stop listening for 5 minutes", &IDLE).map(|r| r.stage), Some("privacy"));
        assert_eq!(reflex("Stop listening to the call", &IDLE), None, "the call bridge, not the lock");
        assert_eq!(reflex("Listen to this song", &IDLE), None);
    }

    #[test]
    fn the_emblem_moves_wherever_sir_says() {
        let moved = |place: &str| Some(("orb", json!({"tool": "configure_interface", "args": {"orb_board_position": place}})));
        // jarvis.log 2026-11-04 00:53:46: this went to window snapping ("Your Orb A Little Bit").
        assert_eq!(reflex("Jarvis, move your orb a little bit to the left.", &IDLE), moved("a little bit to the left"));
        // The screenshot sentence, and its siblings.
        assert_eq!(reflex("Jarvis, move your orb back to the middle.", &IDLE), moved("back to the middle"));
        assert_eq!(reflex("Center your orb", &IDLE), moved("center"));
        assert_eq!(reflex("Put your core in the middle", &IDLE), moved("in the middle"));
        assert_eq!(reflex("Move your orb to the top right", &IDLE), moved("to the top right"));
        assert_eq!(reflex("Can you please move the emblem to the bottom left corner for me?", &IDLE), moved("to the bottom left corner"));
        assert_eq!(reflex("Schieb den Orb etwas nach rechts", &IDLE), moved("etwas nach rechts"));
        assert_eq!(reflex("Bewege deinen Orb in die Mitte", &IDLE), moved("in die mitte"));
        // Every place the router hands over is one the interface can carry out.
        for text in ["move your orb a little bit to the left", "put your core in the middle", "move the sphere way further up", "orb to the top", "setz die Kugel ganz nach unten links"] {
            let Some(Routed { reflex: Reflex::Silent(a), .. }) = route(text, &IDLE) else { panic!("no reflex for {text}") };
            let place = a.args["orb_board_position"].as_str().unwrap();
            assert!(crate::interface::orb_move(place).is_some(), "{text} → '{place}'");
            assert!(a.label.starts_with("ORB // "), "{}", a.label);
        }
        // Not a move of the emblem on the board: the core view, the floating orb, its size.
        for other in [
            "Show me your core", "Hide the core", "Close your core", "Make the orb bigger", "Move the orb to the top right of monitor 2", "Move your orb", "Bring back your core",
            "Spin the core faster", "Switch to orb mode", "Don't move your orb to the left", "What is at the core of the left ventricle?",
        ] {
            assert_eq!(route(other, &IDLE).map(|r| r.stage), None, "{other}");
        }
    }

    #[test]
    fn a_follow_up_keeps_moving_the_emblem_until_something_else_is_asked() {
        let _g = follow_up_lock();
        forget_follow_up();
        assert_eq!(reflex("a bit more", &IDLE), None, "nothing to follow up");
        let first = route("move your orb a little bit to the left", &IDLE).unwrap();
        committed(&first);
        let place = |text: &str| match route(text, &IDLE).map(|r| r.reflex) {
            Some(Reflex::Silent(a)) => a.args["orb_board_position"].as_str().map(String::from),
            _ => None,
        };
        assert_eq!(place("a bit more").as_deref(), Some("a bit left"), "the same step again");
        assert_eq!(place("again").as_deref(), Some("a bit left"));
        assert_eq!(place("further up").as_deref(), Some("further up"), "a new direction is taken as said");
        assert_eq!(place("the other way").as_deref(), Some("more right"), "back past where it started");
        assert_eq!(place("what's the weather"), None, "anything else is a new request");
        // A jump to a fixed spot has no step to repeat, but a direction still follows on.
        committed(&route("move your orb to the top left", &IDLE).unwrap());
        assert_eq!(place("more"), None);
        assert_eq!(place("a little lower").as_deref(), Some("a little lower"));
        // Another reflex (or any other turn) closes the window.
        committed(&route("close the HUD board", &BOARD).unwrap());
        assert_eq!(place("a bit more"), None);
        forget_follow_up();
    }

    #[test]
    fn his_own_blocks_close_inside_jarvis_never_as_programs() {
        let closed = |args: Value| Some(("blocks", json!({"tool": "hud_board", "args": args})));
        // The screenshot: "Hud Board isn't running, Sir."
        for text in ["Close the HUD board, please.", "Close the HUD board", "Jarvis close to web search.", "Close web search", "Schließe das HUD Board"] {
            assert_eq!(reflex(text, &BOARD), closed(json!({"action": "close"})), "{text}");
        }
        assert_eq!(reflex("Close the feature lab", &IDLE), closed(json!({"action": "close", "target": "feature lab"})));
        assert_eq!(reflex("Close telemetry", &IDLE), closed(json!({"action": "close", "target": "telemetry"})));
        assert_eq!(reflex("Clear the board", &BOARD), closed(json!({"action": "clear"})));
        // Not his: programs, HUD panels, the core view (its own rule), and "don't".
        for other in ["Close Spotify", "Close Discord", "Hide the system panel", "Close your core", "Don't close the board", "Clear my calendar", "Close the door"] {
            assert_eq!(route(other, &BOARD).map(|r| r.stage), None, "{other}");
        }
    }

    #[test]
    fn pages_turn_by_voice_and_bare_words_only_with_a_stack_on_screen() {
        let _g = follow_up_lock();
        let page = |args: Value| Some(("pages", json!({"tool": "hud_board", "args": args})));
        for text in ["Jarvis, next page", "Next result", "Show me the next result", "Nächste Seite"] {
            assert_eq!(reflex(text, &PAGED), page(json!({"action": "flip_note", "direction": "next"})), "{text}");
        }
        for text in ["Previous page", "Go back", "Zurück", "back"] {
            assert_eq!(reflex(text, &PAGED), page(json!({"action": "flip_note", "direction": "previous"})), "{text}");
        }
        assert_eq!(reflex("Show result 3", &PAGED), page(json!({"action": "flip_note", "page": 3})));
        assert_eq!(reflex("next", &PAGED), page(json!({"action": "flip_note", "direction": "next"})));
        // Without a stack on the board the bare words stay the music player's.
        for text in ["next", "back", "weiter", "previous"] {
            assert_eq!(reflex(text, &BOARD), None, "{text}");
            assert!(crate::intents::silent_quick_command(text).is_some(), "{text} is a media key then");
        }
        // No board at all: no pages.
        assert_eq!(reflex("Next page", &IDLE), None);
        assert_eq!(reflex("Next song", &PAGED), None, "a song is never a page");
    }

    #[test]
    fn everything_else_is_left_to_the_other_parsers() {
        let _g = follow_up_lock();
        for text in [
            "What classes do I have tomorrow?", "Open my browser", "Set the volume to 40 percent", "Pause", "Play some music", "What's the weather like?", "Yes.", "Thanks, that will be all.",
            "Immer wenn du das sagst, klingt das so als hättest du so eine richtige Scheißtastatur.", "Remember that my last name is Mustermann", "Move Sonora to monitor 2", "Maximize that",
            "Use all monitors", "",
        ] {
            assert_eq!(route(text, &PAGED), None, "{text}");
        }
    }

    #[test]
    fn misheard_commands_are_recognised_by_their_sound() {
        // The sounds themselves.
        assert_eq!(sound_key("hide the core"), "hiteko");
        assert_eq!(sound_key("Fight the core."), "fiteko");
        assert_eq!(sound_key("switch to Ultron"), "swixtoltron");
        assert_eq!(sound_key("Which do old run?"), "wixtoltron");
        assert_eq!(sound_key("turn"), "ten");
        assert_eq!(sound_key("10"), "ten");
        assert!((sound_similarity("hiteko", "hiteko") - 1.0).abs() < 1e-6);
        assert!(sound_similarity("fiteko", "hiteko") > 0.9, "f and h are easily confused");
        assert!(sound_similarity("nekstwik", "nekstpej") < 0.75, "week is no page");
        // jarvis.log 2026-11-03 22:47-22:52: what Whisper wrote, and what Sir had said.
        assert_eq!(phonetic_rescue("Fight the core.", &IDLE), Some("hide your core"));
        assert_eq!(phonetic_rescue("disabled the car.", &IDLE), Some("hide your core"));
        assert_eq!(phonetic_rescue("Which do old run?", &IDLE), Some("switch to Ultron"), "Whisper's question mark included");
        assert_eq!(phonetic_rescue("10 discord call on.", &IDLE), Some("turn on the discord call"));
        // Only what makes sense right now: Ultron is already on / no stack of pages is up.
        assert_eq!(phonetic_rescue("Which do old run", &ULTRON), None);
        assert_eq!(phonetic_rescue("next beige", &IDLE), None);
        // Ordinary sentences stay what they are.
        for text in [
            "next week", "What's the score", "write the code", "hide the board", "Thanks, that will be all", "Yes", "I hate this game", "Open the map", "Let's go", "You can tell me",
            "Press the button", "On the second checkpoint", "Put some right there", "That's not all transverse", "Ich hasse ihn", "Ja, was denn", "Guten Tag", "show me the door",
        ] {
            assert_eq!(phonetic_rescue(text, &PAGED), None, "{text}");
        }
        // Every rescue leads somewhere: a reflex, or a command the older parsers know.
        for rescue in RESCUES {
            let ctx = ReflexContext { ultron: rescue.means.contains("back to Jarvis"), board_up: true, spotlight: false, paged: true };
            assert!(route(rescue.means, &ctx).is_some() || crate::intents::parse(rescue.means).is_some(), "'{}' runs nothing", rescue.means);
        }
    }

    /// Never fires on real chatter: every transcript in Sir's own log, if it is here.
    #[test]
    #[ignore = "reads jarvis.log from the working directory"]
    fn rescue_stays_silent_on_the_real_log() {
        let Ok(log) = std::fs::read_to_string("jarvis.log") else { return };
        let mut heard = 0;
        let mut rescued: Vec<(String, &'static str)> = Vec::new();
        for line in log.lines().filter(|l| l.contains("[STT] Transcribed")) {
            let Some(text) = line.split_once("]: '").map(|(_, t)| t.trim_end_matches('\'').to_string()) else { continue };
            heard += 1;
            for ctx in [IDLE, PAGED] {
                if let Some(said) = phonetic_rescue(&text, &ctx) {
                    if !rescued.iter().any(|(t, _)| *t == text) {
                        rescued.push((text.clone(), said));
                    }
                }
            }
        }
        println!("{heard} logged transcripts, {} read as a command by sound:", rescued.len());
        for (text, said) in &rescued {
            println!("  '{text}' → '{said}'");
        }
    }

    #[test]
    fn routing_a_sentence_takes_well_under_a_millisecond_and_a_half() {
        // (`bench_with` warms every pattern itself; `intents::warm_up` would also load Sir's
        // real music library into this test process.)
        let reflexes = benchmark(40);
        let full = benchmark_full(40);
        println!("reflex router: {}", reflexes.describe());
        println!("whole deterministic path: {}", full.describe());
        assert!(reflexes.routed >= 18, "the corpus' own controls are all reflexes: {}", reflexes.routed);
        // A debug build runs unoptimised regexes; the budget is the release build's.
        let budget_us = if cfg!(debug_assertions) { 30_000.0 } else { 1_500.0 };
        assert!(reflexes.p99_us < budget_us, "reflex router: {}", reflexes.describe());
        assert!(full.p99_us < budget_us, "whole deterministic path: {}", full.describe());
    }
}
