//! The fifteen field systems on top of the Stark hub: Stark Air-Drop, Instant Replay, the Project
//! Architect, the Exam Coach, the Network Guardian, the Deal Sniper, the Media Grabber, the Stress
//! Tester, the Deep Researcher, the Sandbox Runner, the Minecraft Doctor, the Mobile Scanner, the
//! Finance Tracker, the File Janitor and the Smart Alarm.
//!
//! This hub owns their shared state, tool schemas, dispatcher, background work, deterministic
//! voice routes ("clip that", "quiz me on photosynthesis", "wake me at 6:30") and the small
//! helpers they share (PowerShell, child processes, native result windows, folders).

use crate::integration_router::{Mode, Route};
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

/// Shared state of the field systems (cheap to clone).
#[derive(Clone)]
pub struct Features {
    pub replay: crate::instant_replay::Replay,
    pub exam: crate::exam_coach::ExamCoach,
    pub deals: crate::deal_sniper::DealSniper,
    pub finance: crate::finance_tracker::Finance,
    pub janitor: crate::file_janitor::Janitor,
    pub alarm: crate::smart_alarm::AlarmClock,
    pub airdrop: crate::airdrop_bridge::AirDrop,
    pub scanner: crate::mobile_scanner::Scanner,
    pub media: crate::media_grabber::MediaJobs,
    /// The workbench: flashcards, screen time, routines, thermals, clipboard history.
    pub bench: crate::workbench::Workbench,
}

impl Features {
    pub fn open(db: &Path) -> Self {
        let warn = |what: &str, e: String| tracing::warn!("[FEATURES] {what} cannot be persisted ({e}); it lasts for this session only.");
        let deals = crate::deal_sniper::DealSniper::open(db).unwrap_or_else(|e| {
            warn("Price snipers", e);
            crate::deal_sniper::DealSniper::in_memory()
        });
        let finance = crate::finance_tracker::Finance::open(db).unwrap_or_else(|e| {
            warn("Finances", e);
            crate::finance_tracker::Finance::in_memory()
        });
        let alarm = crate::smart_alarm::AlarmClock::open(db).unwrap_or_else(|e| {
            warn("Alarms", e);
            crate::smart_alarm::AlarmClock::in_memory()
        });
        let dir = db.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        Self::with(deals, finance, alarm, crate::file_janitor::Janitor::new(dir.join("jarvis_cleanup")), crate::workbench::Workbench::open(db))
    }

    pub fn in_memory() -> Self {
        Self::with(
            crate::deal_sniper::DealSniper::in_memory(),
            crate::finance_tracker::Finance::in_memory(),
            crate::smart_alarm::AlarmClock::in_memory(),
            crate::file_janitor::Janitor::new(std::env::temp_dir().join("jarvis_cleanup_test")),
            crate::workbench::Workbench::in_memory(),
        )
    }

    fn with(deals: crate::deal_sniper::DealSniper, finance: crate::finance_tracker::Finance, alarm: crate::smart_alarm::AlarmClock, janitor: crate::file_janitor::Janitor, bench: crate::workbench::Workbench) -> Self {
        Self {
            bench,
            replay: crate::instant_replay::Replay::default(),
            exam: crate::exam_coach::ExamCoach::default(),
            deals,
            finance,
            janitor,
            alarm,
            airdrop: crate::airdrop_bridge::AirDrop::default(),
            scanner: crate::mobile_scanner::Scanner::default(),
            media: crate::media_grabber::MediaJobs::default(),
        }
    }

    /// The rolling screen buffer, the deal checks and the alarm clock.
    pub fn start_background(&self, tools: &ToolContext) {
        self.replay.start(tools.config.clone());
        self.deals.start(tools.clone());
        self.alarm.start(tools.clone());
        self.bench.start_background(tools);
    }

    /// Lines for the live context block.
    pub fn live_context(&self) -> String {
        let mut out = String::new();
        if let Some(line) = self.exam.describe() {
            out.push_str(&line);
            out.push('\n');
        }
        if let Some(line) = self.alarm.describe() {
            out.push_str(&line);
            out.push('\n');
        }
        let snipers = self.deals.active_count();
        if snipers > 0 {
            out.push_str(&format!("Price snipers watching: {snipers} (manage_price_sniper)\n"));
        }
        if let Some(line) = self.media.describe() {
            out.push_str(&line);
            out.push('\n');
        }
        if let Some(line) = self.bench.live_context() {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

// --------------------------------------------------------------------------- tools ----

/// (tool, core node) of every field-system tool.
const TOOL_NODES: &[(&str, &str)] = &[
    ("beam_to_device", "airdrop"),
    ("save_instant_replay", "replay"),
    ("spawn_coding_project", "architect"),
    ("start_oral_exam_session", "exam"),
    ("answer_oral_exam", "exam"),
    ("diagnose_network_and_ping", "network"),
    ("manage_price_sniper", "deals"),
    ("download_or_extract_media", "media"),
    ("stress_test_idea_or_code", "stress"),
    ("start_background_research", "research"),
    ("run_sandbox_calculation_or_script", "sandbox"),
    ("diagnose_minecraft_or_server", "minecraft"),
    ("scan_with_phone", "scanner"),
    ("manage_finances_and_goals", "finance"),
    ("smart_folder_cleanup", "janitor"),
    ("undo_last_cleanup", "janitor"),
    ("set_smart_morning_alarm", "alarm"),
];

pub fn is_tool(name: &str) -> bool {
    TOOL_NODES.iter().any(|(t, _)| *t == name) || crate::workbench::is_tool(name)
}

/// The tool name as `'static` (routes and fast-path intents need one).
pub fn static_name(name: &str) -> Option<&'static str> {
    TOOL_NODES.iter().map(|(t, _)| *t).find(|t| *t == name).or_else(|| crate::workbench::static_name(name))
}

pub fn node_of(name: &str) -> Option<&'static str> {
    TOOL_NODES.iter().find(|(t, _)| *t == name).map(|(_, n)| *n).or_else(|| crate::workbench::is_tool(name).then_some("context"))
}

/// Tools that take a few seconds (Jarvis acknowledges first).
pub fn is_slow(name: &str) -> bool {
    matches!(
        name,
        "diagnose_network_and_ping"
            | "spawn_coding_project"
            | "stress_test_idea_or_code"
            | "run_sandbox_calculation_or_script"
            | "diagnose_minecraft_or_server"
            | "start_oral_exam_session"
            | "answer_oral_exam"
            | "save_instant_replay"
            | "smart_folder_cleanup"
    )
}

pub fn tool_definitions() -> Vec<Value> {
    let mut all = field_definitions();
    all.extend(crate::workbench::tool_definitions());
    all
}

fn field_definitions() -> Vec<Value> {
    vec![
        crate::airdrop_bridge::tool_definition(),
        crate::instant_replay::tool_definition(),
        crate::project_architect::tool_definition(),
        crate::exam_coach::tool_definition(),
        crate::exam_coach::answer_tool_definition(),
        crate::network_guardian::tool_definition(),
        crate::deal_sniper::tool_definition(),
        crate::media_grabber::tool_definition(),
        crate::stress_tester::tool_definition(),
        crate::deep_researcher::tool_definition(),
        crate::sandbox_runner::tool_definition(),
        crate::minecraft_doctor::tool_definition(),
        crate::mobile_scanner::tool_definition(),
        crate::finance_tracker::tool_definition(),
        crate::file_janitor::tool_definition(),
        crate::file_janitor::undo_tool_definition(),
        crate::smart_alarm::tool_definition(),
    ]
}

pub async fn run_tool(tools: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    if crate::workbench::is_tool(name) {
        return crate::workbench::run_tool(tools, name, args).await;
    }
    let node = node_of(name).unwrap_or("brain");
    tools.stark.pulse(&["brain", node], false);
    let f = &tools.stark.features;
    match name {
        "beam_to_device" => crate::airdrop_bridge::run_tool(tools, args).await,
        "save_instant_replay" => f.replay.save(tools, args).await,
        "spawn_coding_project" => crate::project_architect::run_tool(tools, args).await,
        "start_oral_exam_session" => f.exam.start(tools, args).await,
        "answer_oral_exam" => f.exam.answer(tools, args).await,
        "diagnose_network_and_ping" => crate::network_guardian::run_tool(tools, args).await,
        "manage_price_sniper" => f.deals.tool(tools, args).await,
        "download_or_extract_media" => f.media.tool(tools, args).await,
        "stress_test_idea_or_code" => crate::stress_tester::run_tool(tools, args).await,
        "start_background_research" => crate::deep_researcher::run_tool(tools, args),
        "run_sandbox_calculation_or_script" => crate::sandbox_runner::run_tool(tools, args).await,
        "diagnose_minecraft_or_server" => crate::minecraft_doctor::run_tool(tools, args).await,
        "scan_with_phone" => f.scanner.tool(tools, args),
        "manage_finances_and_goals" => f.finance.tool(tools, args),
        "smart_folder_cleanup" => f.janitor.tool(tools, args).await,
        "undo_last_cleanup" => f.janitor.undo(),
        "set_smart_morning_alarm" => f.alarm.tool(tools, args),
        other => Err(format!("Unknown field-system tool '{other}'.")),
    }
}

// ------------------------------------------------------------------------- voice routes ----

static AMOUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(\d+(?:[.,]\d{1,2})?)\s*(?:€|euros?|eur\b|bucks)|€\s*(\d+(?:[.,]\d{1,2})?)").unwrap());
static CLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})(?:[:.](\d{2}))?\s*(a\.?m\.?|p\.?m\.?|uhr|o'?clock)?\b").unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s"'<>]+"#).unwrap());
static SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)from\s+(\d{1,2}:\d{2}(?::\d{2})?)\s+(?:to|until|till|-)\s+(\d{1,2}:\d{2}(?::\d{2})?)").unwrap());
static SECONDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,3})\s*(seconds?|secs?|minutes?|mins?)\b").unwrap());

fn has(lower: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| lower.contains(p))
}

fn words(lower: &str) -> Vec<String> {
    lower.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(String::from).collect()
}

fn verbatim(tool: &'static str, args: Value) -> Option<Route> {
    Some(Route { tool, args, mode: Mode::Verbatim })
}

/// The text after the first of `markers` ("quiz me on |the french revolution|").
fn after<'a>(text: &'a str, lower: &'a str, markers: &[&str]) -> Option<&'a str> {
    markers
        .iter()
        .find_map(|m| lower.find(m).map(|i| text.get(i + m.len()..).unwrap_or(&lower[i + m.len()..]).trim().trim_end_matches(['?', '.', '!']).trim()))
        .filter(|s| !s.is_empty())
}

/// `s` up to the first of `stops` ("StarkNet in rust on monitor 2" → "StarkNet").
fn cut<'a>(s: &'a str, stops: &[&str]) -> &'a str {
    let lower = s.to_lowercase();
    let end = stops.iter().filter_map(|st| lower.find(st)).min().unwrap_or(s.len());
    s.get(..end).unwrap_or(s).trim()
}

/// "on my second monitor" / "on monitor 2" / "on the left screen".
fn monitor_in(lower: &str) -> Option<String> {
    let ws = words(lower);
    let i = ws.iter().position(|w| matches!(w.as_str(), "monitor" | "screen" | "display" | "bildschirm"))?;
    let near: Vec<&str> = ws[i.saturating_sub(2)..(i + 2).min(ws.len())].iter().map(String::as_str).collect();
    for (word, value) in [("left", "left"), ("right", "right"), ("primary", "primary"), ("main", "primary"), ("first", "1"), ("second", "2"), ("third", "3"), ("1", "1"), ("2", "2"), ("3", "3"), ("other", "next")] {
        if near.contains(&word) {
            return Some(value.to_string());
        }
    }
    None
}

/// Deterministic routes for the field systems (the small local model rarely calls tools).
pub fn route_text(text: &str) -> Option<Route> {
    let lower = text.to_lowercase();
    let ws = words(&lower);
    if ws.is_empty() || ws.len() > 40 {
        return None;
    }
    // The network radar, the disk map and the Earth are panels of Jarvis' own ("check the
    // network" sweeps it on the radar now, "scan my wifi" too): `intents::panel_request`.
    if crate::intents::panel_request(text).is_some() {
        return None;
    }
    let lead = lower.trim_start_matches("jarvis").trim_start_matches([',', ' ']).trim_start_matches("please ").trim();

    // ---- The workbench first: "quiz me on my flashcards" is the deck, not an oral exam ----
    if let Some(route) = crate::workbench::route_text(text) {
        return Some(route);
    }

    // ---- Instant Replay ----
    if has(&lower, &["clip that", "clip this", "clip it", "clipped it", "clipped that", "save that clip", "save the clip", "instant replay", "save a replay", "save the replay", "save the last", "clip the last", "clip das", "speicher das als clip", "nimm das auf"]) {
        let seconds = SECONDS.captures(&lower).and_then(|c| {
            let n: u64 = c[1].parse().ok()?;
            Some(if c[2].starts_with('m') { n * 60 } else { n })
        });
        return verbatim("save_instant_replay", json!({ "seconds": seconds }));
    }

    // ---- Exam Coach ----
    if let Some(topic) = after(text, &lower, &["quiz me on ", "quiz me about ", "test me on ", "test me about ", "examine me on ", "oral exam on ", "oral exam about ", "prüf mich in ", "prüfe mich in ", "frag mich ab in ", "frag mich ab zu "]) {
        return verbatim("start_oral_exam_session", json!({ "subject": topic, "source_file_or_topic": topic }));
    }
    if has(lead, &["quiz me", "start an oral exam", "start a quiz", "start the quiz", "frag mich ab"]) {
        return verbatim("start_oral_exam_session", json!({}));
    }

    // ---- Project Architect (before Minecraft: "a new minecraft modpack project") ----
    let commands = |verbs: &[&str]| verbs.iter().any(|v| lead.starts_with(v));
    if has(&lower, &["project", "scaffold", "web app", "modpack", "projekt"])
        && commands(&["create", "make", "start", "set up", "setup", "spawn", "scaffold", "new ", "build", "open a new", "erstell", "leg ein", "neues"])
    {
        let name = after(text, &lower, &["called ", "named ", "name it ", "namens "]).map(|s| cut(s, &[" on ", " in ", " with ", " using ", " for ", " as "]).to_string());
        let template = if has(&lower, &["pytorch", "torch", "cuda", "machine learning", "neural"]) {
            "PyTorch_CUDA"
        } else if has(&lower, &["rust", "cargo"]) {
            "Rust_Binary"
        } else if has(&lower, &["web", "website", "html", "javascript"]) {
            "Web_App"
        } else if has(&lower, &["modpack", "minecraft"]) {
            "Minecraft_Modpack"
        } else {
            "Python_App"
        };
        return verbatim("spawn_coding_project", json!({ "project_name": name, "template_type": template, "target_monitor": monitor_in(&lower) }));
    }

    // ---- Network Guardian ----
    let scan = has(&lower, &["scan my network", "scan the network", "scan my wifi", "scan the wifi", "devices on my network", "devices are on my", "what's on my network", "who is on my wifi", "who's on my wifi", "devices in my wifi", "lan scan", "netzwerk scannen"]);
    let hog = has(&lower, &["bandwidth", "hogging", "using my internet", "eating my internet", "eating my bandwidth", "what's downloading", "what is downloading"]);
    let lag = ws.iter().any(|w| w == "ping" || w == "jitter" || w == "latency")
        || has(&lower, &["packet loss", "internet is slow", "internet slow", "internet lag", "network lag", "lagging", "why is my internet", "is my internet", "connection is bad", "high ping", "internet ist langsam"]);
    // "Diagnose my network": everything at once (before the integrations check claims "diagnose").
    let full = has(&lower, &["diagnose my network", "diagnose the network", "diagnose my internet", "diagnose my connection", "network diagnos", "check my network", "check the network", "test my network", "check my internet", "test my internet", "test my connection", "netzwerk prüfen", "netzwerk diagnose", "internet prüfen"]);
    if scan || hog || lag || full {
        let action = match (scan, hog, lag) {
            _ if full => "full",
            (true, false, false) => "scan_lan",
            (false, true, false) => "bandwidth",
            (false, false, true) => "ping",
            _ => "full",
        };
        return verbatim("diagnose_network_and_ping", json!({ "action": action }));
    }

    // ---- Minecraft Doctor ----
    if has(&lower, &["minecraft", "modpack", "mc server", "my server crashed"]) {
        let action = if has(&lower, &["who is on", "who's on", "players on", "players online", "server status", "status of", "is my server up", "is the server up", "is my server online", "is the server online"]) {
            "status"
        } else if let Some(cmd) = after(text, &lower, &["run the command ", "run command ", "execute "]) {
            return verbatim("diagnose_minecraft_or_server", json!({ "action": "rcon", "log_path_or_command": cmd }));
        } else if has(&lower, &["crash", "error", "broken", "won't start", "doesn't start", "does not start", "not starting", "won't load", "latest.log", "the log", "diagnose", "fix my", "abgestürzt", "stürzt"]) {
            "diagnose"
        } else {
            ""
        };
        if !action.is_empty() {
            return verbatim("diagnose_minecraft_or_server", json!({ "action": action }));
        }
    }

    // ---- Finance Tracker ----
    let amount = AMOUNT.captures(&lower).and_then(|c| c.get(1).or_else(|| c.get(2)).and_then(|m| m.as_str().replace(',', ".").parse::<f64>().ok()));
    if let Some(amount) = amount {
        let category = after(text, &lower, &[" for ", " on "]).map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
        if has(&lower, &["saving for", "savings goal", "save up for", "goal of", "sparziel"]) {
            let goal = after(text, &lower, &["saving for ", "save up for ", "goal for ", "goal called "]).map(|s| s.split(" with ").next().unwrap_or(s).to_string());
            return verbatim("manage_finances_and_goals", json!({ "action": "set_goal", "amount_eur": amount, "goal_name": goal }));
        }
        if has(&lower, &["towards", "to my savings", "into my savings", "to the goal", "put aside", "set aside"]) {
            let goal = after(text, &lower, &["towards ", "to my ", "into my "]).map(|s| s.trim_end_matches(" goal").to_string());
            return verbatim("manage_finances_and_goals", json!({ "action": "add_to_goal", "amount_eur": amount, "goal_name": goal }));
        }
        if has(&lower, &["earned", "got paid", "received", "income", "salary", "pocket money", "taschengeld", "verdient"]) {
            return verbatim("manage_finances_and_goals", json!({ "action": "add_income", "amount_eur": amount, "category": category.clone().unwrap_or_else(|| "income".into()), "description": text }));
        }
        if has(&lower, &["spent", "spend", "paid", "bought", "expense", "ausgegeben", "bezahlt", "gekauft"]) {
            return verbatim("manage_finances_and_goals", json!({ "action": "add_expense", "amount_eur": amount, "category": category, "description": text }));
        }
    }
    if has(&lower, &["how much have i spent", "how much did i spend", "my spending", "my finances", "my budget", "finance summary", "savings goals", "my savings", "wie viel habe ich ausgegeben"]) {
        return verbatim("manage_finances_and_goals", json!({ "action": "summary" }));
    }

    // ---- File Janitor ----
    if has(&lower, &["undo the cleanup", "undo last cleanup", "undo the last cleanup", "revert the cleanup", "put my files back"]) {
        return verbatim("undo_last_cleanup", json!({}));
    }
    if has(&lower, &["apply the cleanup", "go ahead with the cleanup", "do the cleanup", "run the cleanup", "execute the cleanup"]) {
        return verbatim("smart_folder_cleanup", json!({ "action": "apply" }));
    }
    if has(&lower, &["clean up my", "clean my", "tidy up my", "tidy my", "organize my", "organise my", "sort my", "clean up the", "aufräumen"])
        && has(&lower, &["download", "desktop", "documents", "pictures", "videos", "folder", "files", "ordner", "schreibtisch"])
    {
        let folder = ["downloads", "desktop", "documents", "pictures", "videos", "music"].iter().find(|f| lower.contains(&f[..f.len() - 1])).copied().unwrap_or("downloads");
        return verbatim("smart_folder_cleanup", json!({ "target_folder": folder, "dry_run_first": true }));
    }

    // ---- Smart Alarm ----
    if has(&lower, &["alarm", "wake me", "weck mich", "wecker"]) {
        if has(&lower, &["cancel", "delete", "remove", "turn off", "switch off", "no alarm", "lösch"]) {
            return verbatim("set_smart_morning_alarm", json!({ "action": "cancel" }));
        }
        if ws.iter().any(|w| w == "list") || has(&lower, &["what alarm", "which alarm", "my alarms", "is my alarm", "when is my alarm"]) {
            return verbatim("set_smart_morning_alarm", json!({ "action": "list" }));
        }
        let setting = has(&lower, &["wake me", "weck mich", "set an alarm", "set alarm", "set the alarm", "set my alarm", "alarm for", "alarm at", "new alarm", "an alarm", "wecker auf", "wecker für", "stell"]);
        if let Some(c) = CLOCK.captures(&lower).filter(|_| setting) {
            let mut hour: u32 = c[1].parse().ok()?;
            let minute: u32 = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
            if c.get(3).is_some_and(|m| m.as_str().starts_with('p')) && hour < 12 {
                hour += 12;
            }
            if hour < 24 && minute < 60 {
                let playlist = after(text, &lower, &["playlist ", "play ", "with "]).map(|s| s.split_whitespace().next().unwrap_or(s).trim_end_matches(['.', ',']).to_string());
                let adjust = !has(&lower, &["don't adjust", "do not adjust", "no matter what", "fixed"]);
                return verbatim("set_smart_morning_alarm", json!({ "wake_time": format!("{hour:02}:{minute:02}"), "adjust_for_cancelled_classes": adjust, "playlist_name": playlist }));
            }
        }
    }

    // ---- Media Grabber ----
    let url = URL.find(text).map(|m| m.as_str().to_string());
    let media_words = has(&lower, &["as mp3", "as an mp3", "as mp4", "as an mp4", "as wav", "as a wav", "rip the audio", "extract the audio", "download this video", "download the video", "download that video", "download this song", "download the song", "download this as", "download it as", "grab this video", "grab the audio"]);
    if media_words || (url.is_some() && has(&lower, &["download", "grab", "rip"])) {
        let format = if has(&lower, &["wav"]) {
            "wav"
        } else if has(&lower, &["mp3", "audio", "song", "music"]) {
            "mp3"
        } else {
            "mp4"
        };
        let (start, end) = SPAN.captures(&lower).map(|c| (Some(c[1].to_string()), Some(c[2].to_string()))).unwrap_or((None, None));
        return verbatim("download_or_extract_media", json!({ "url_or_active_browser": url.unwrap_or_else(|| "active_browser".into()), "format": format, "start_time": start, "end_time": end }));
    }

    // ---- Deep Researcher ----
    static RESEARCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:can you |could you )?(?:do |run |start )?(?:a |some )?(?:(deep|thorough|quick|background) )?research(?:ing)? (?:on |about |into )?(.+)$").unwrap());
    // "Research X for two hours (, focus on cost and safety)": a multi-hour Deep Research mission.
    static DURATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*\bfor (\d+(?:[.,]\d+)?|an?|one|two|three|four|five|six|half an?) (hours?|hrs?|h|minutes?|mins?)\b").unwrap());
    static FOCUS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[,;]?\s*\b(?:focus(?:ing)? on|with (?:a )?focus on)\s+(.+)$").unwrap());
    let request = RESEARCH.captures(lead).map(|c| c[2].to_string()).unwrap_or_default();
    if let Some(d) = DURATION.captures(&request) {
        let c = &request;
        let n = match d[1].to_lowercase().as_str() {
            "a" | "an" | "one" => 1.0,
            "two" => 2.0,
            "three" => 3.0,
            "four" => 4.0,
            "five" => 5.0,
            "six" => 6.0,
            s if s.starts_with("half") => 0.5,
            s => s.replace(',', ".").parse::<f64>().unwrap_or(1.0),
        };
        let minutes = if d[2].to_lowercase().starts_with('h') { n * 60.0 } else { n };
        let rest = DURATION.replace(c, "").to_string();
        let focus: Vec<String> = FOCUS
            .captures(&rest)
            .map(|f| f[1].trim_end_matches(['?', '.', '!']).split([',', '&', ';']).flat_map(|p| p.split(" and ")).map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        let topic = FOCUS.replace(&rest, "").trim_end_matches(['?', '.', '!', ',']).trim().to_string();
        if !topic.is_empty() {
            return verbatim("manage_deep_research", json!({ "action": "start", "topic": topic, "duration_minutes": minutes.round() as u64, "focus_areas": focus }));
        }
    }
    if let Some(c) = RESEARCH.captures(lead) {
        let topic = c[2].trim_end_matches(['?', '.', '!']).trim();
        let statement = ["shows", "says", "suggests", "has", "is", "indicates", "found", "proves"].contains(&topic.split_whitespace().next().unwrap_or(""));
        // A plain "research X" is the HUD board's research agent (`intents::board_command`): its
        // report goes up on the board. The background dossier is asked for by name: "deep /
        // thorough / quick / background research on X", "a dossier on X".
        let depth = match c.get(1).map(|m| m.as_str()) {
            Some("deep") | Some("thorough") => Some("deep"),
            Some("quick") => Some("quick"),
            Some(_) => Some("standard"),
            None => None,
        };
        if let Some(depth) = depth.filter(|_| !topic.is_empty() && !statement) {
            return verbatim("start_background_research", json!({ "topic": topic, "depth": depth }));
        }
    }
    if let Some(topic) = after(text, &lower, &["find out everything about ", "investigate ", "write me a dossier on ", "dossier about ", "dossier on "]) {
        return verbatim("start_background_research", json!({ "topic": topic, "depth": "deep" }));
    }

    // ---- Stress Tester ----
    if lead.starts_with("stress test") || lead.starts_with("stress-test") || has(&lower, &["stress test my", "stress test this", "stress test the", "poke holes", "find the flaws", "find flaws in", "tear apart", "tear this apart", "what could go wrong with", "find the bottlenecks", "bottlenecks in"]) {
        let target = after(text, &lower, &["stress test ", "stress-test ", "poke holes in ", "flaws in ", "tear apart ", "what could go wrong with ", "bottlenecks in "])
            .filter(|t| !matches!(t.to_lowercase().as_str(), "this" | "that" | "it" | "my plan" | "this plan" | "my code" | "this code"))
            .unwrap_or("clipboard");
        return verbatim("stress_test_idea_or_code", json!({ "target_plan_or_file": target }));
    }

    // ---- Sandbox Runner ----
    let python = has(&lower, &["in python", "with python", "using python", "python script"]);
    let rust = has(&lower, &["in rust", "with rust", "using rust"]);
    if ws.iter().any(|w| w == "sandbox")
        || ((python || rust) && commands(&["calculate", "compute", "simulate", "plot", "run", "solve", "work out", "berechne", "use python", "use rust", "write a script", "write a program", "make a chart", "draw a graph", "can you calculate"]))
    {
        return verbatim("run_sandbox_calculation_or_script", json!({ "language": if rust { "rust" } else { "python" }, "code_or_task": text }));
    }

    // ---- Stark Air-Drop ----
    let device = has(&lower, &["phone", "iphone", "ipad", "handy", "tablet", "mobile"]);
    if (device && has(&lower, &["send this", "send that", "send it", "beam", "push this", "push that", "put this on", "put that on", "share this", "share that", "throw this", "throw that"])) || has(&lower, &["airdrop", "air-drop", "air drop"]) {
        let content = after(text, &lower, &[": "]).map(String::from).or(url).unwrap_or_else(|| if has(&lower, &["selected", "selection", "highlighted"]) { "selection".into() } else { "clipboard".into() });
        return verbatim("beam_to_device", json!({ "content": content, "target": "phone" }));
    }

    // ---- Mobile Scanner ----
    if has(&lower, &["scan a qr", "scan the qr", "scan this qr", "scan a document", "scan the document", "scan a barcode", "scan the barcode", "scan with my phone", "open the scanner", "phone scanner", "document scanner"]) {
        let mode = if has(&lower, &["document", "page", "worksheet", "paper"]) { "document" } else { "code" };
        return verbatim("scan_with_phone", json!({ "mode": mode }));
    }
    None
}

// ---------------------------------------------------------------------------- helpers ----

/// A tool result Jarvis reads himself: the block in the chat, one short line aloud.
pub fn spoken(display: impl AsRef<str>, spoken: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref().trim_end(), spoken.as_ref().trim())
}

/// Opens (or refreshes) a native result window with Markdown content.
pub fn show_window(tools: &ToolContext, id: &str, title: &str, markdown: &str, width: f32, height: f32) {
    if !crate::gui::windows_supported() {
        return;
    }
    let _ = tools.widgets.spawn_window(&json!({ "id": id, "title": title, "markdown": markdown, "replace_content": true, "width": width, "height": height }));
    crate::gui::sync_windows();
}

/// A number argument (`12`, `"12,50"`, `"12 €"`).
pub fn num_arg(args: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| match args.get(*k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches(['€', '%']).trim().replace(',', ".").parse().ok(),
        _ => None,
    })
}

pub fn str_arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    crate::tools::str_arg(args, keys)
}

pub fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    crate::tools::bool_arg(args, key).unwrap_or(default)
}

/// `C:\Users\<Sir>\<name>` (Videos, Downloads, Music, Documents, Desktop, Pictures).
pub fn user_folder(name: &str) -> PathBuf {
    let home = crate::apps::home_dir().unwrap_or_else(std::env::temp_dir);
    // OneDrive may have taken over Documents / Desktop / Pictures.
    let onedrive = std::env::var_os("OneDrive").map(PathBuf::from).map(|o| o.join(name)).filter(|p| p.is_dir());
    let local = home.join(name);
    if local.is_dir() {
        local
    } else {
        onedrive.unwrap_or(local)
    }
}

/// A name safe for a file or folder.
pub fn safe_name(raw: &str) -> String {
    let s: String = raw.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let s = s.trim().trim_matches('.').trim();
    if s.is_empty() {
        "untitled".into()
    } else {
        s.chars().take(80).collect()
    }
}

pub fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// What a child process left behind.
#[derive(Debug, Clone, Default)]
pub struct ProcOut {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ProcOut {
    /// The first meaningful error line.
    pub fn error_line(&self) -> String {
        match self.stderr.lines().chain(self.stdout.lines()).map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => line.chars().take(240).collect(),
            None => self.code.map(|c| format!("exit code {c}")).unwrap_or_else(|| "unknown error".into()),
        }
    }
}

/// Runs a program without a console window (and without the busy cursor); killed when
/// `timeout` passes.
pub async fn run(program: &Path, args: &[&str], cwd: Option<&Path>, timeout: Duration) -> Result<ProcOut, String> {
    // .cmd / .bat shims (code.cmd, yt-dlp from pip) need the command interpreter.
    let script = program.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let name = program.file_name().and_then(|n| n.to_str()).unwrap_or("The program").to_string();
    if !script {
        return match crate::shell::run_helper(program, args, cwd, &[], timeout).await {
            Ok(out) => Ok(ProcOut {
                ok: out.success(),
                code: out.code,
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(format!("{name} took longer than {} s and was stopped.", timeout.as_secs())),
            Err(e) => Err(format!("{} could not start: {e}", program.display())),
        };
    }
    // Batch shims keep std's spawner: it escapes arguments for cmd.exe safely.
    let mut cmd = tokio::process::Command::new("cmd");
    cmd.arg("/C").arg(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    crate::shell::hide_console(&mut cmd);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("{name} took longer than {} s and was stopped.", timeout.as_secs()))?
        .map_err(|e| format!("{} could not start: {e}", program.display()))?;
    Ok(ProcOut {
        ok: out.status.success(),
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// Runs a PowerShell script (UTF-8 output); Err with its first error line when it fails.
pub async fn powershell(script: &str, timeout: Duration) -> Result<String, String> {
    let full = format!("$ErrorActionPreference='Stop'\n[Console]::OutputEncoding=[System.Text.Encoding]::UTF8\n{script}");
    let encoded: Vec<u8> = full.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let b64 = crate::screen::base64(&encoded);
    let out = run(Path::new("powershell"), &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &b64], None, timeout).await?;
    if out.ok {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(format!("PowerShell: {}", out.error_line()))
    }
}

/// A real Python interpreter (never the Microsoft Store stub in WindowsApps).
pub fn python() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.to_string_lossy().to_lowercase().contains("windowsapps") {
            continue;
        }
        for name in ["python.exe", "python3.exe", "python"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let manager = local.join("Python").join("bin").join("python.exe");
    if manager.is_file() {
        return Some(manager);
    }
    let programs = local.join("Programs").join("Python");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&programs).ok()?.flatten().map(|e| e.path().join("python.exe")).filter(|p| p.is_file()).collect();
    found.sort();
    found.pop().or_else(|| crate::apps::which("py"))
}

/// A program on PATH or next to Jarvis (`runtime\tools\<name>.exe`).
pub fn find_tool(name: &str) -> Option<PathBuf> {
    crate::apps::which(name).or_else(|| {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        [dir.join("runtime").join("tools").join(format!("{name}.exe")), dir.join("runtime").join(format!("{name}.exe"))].into_iter().find(|p| p.is_file())
    })
    .or_else(|| {
        // winget installs portable tools here.
        let links = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?.join("Microsoft").join("WinGet").join("Links").join(format!("{name}.exe"));
        links.is_file().then_some(links)
    })
}

/// Text of a document (PDF, Markdown, text, HTML, source code).
pub async fn read_document(path: &Path) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let p = path.to_path_buf();
    match ext.as_str() {
        "pdf" => tokio::task::spawn_blocking(move || pdf_extract::extract_text(&p).map_err(|e| format!("the PDF could not be read: {e}"))).await.map_err(|e| e.to_string())?,
        "html" | "htm" => tokio::fs::read_to_string(&p).await.map(|h| crate::websearch::extract_readable(&h, 20_000).text).map_err(|e| e.to_string()),
        _ => {
            let bytes = tokio::fs::read(&p).await.map_err(|e| format!("{} could not be read: {e}", path.display()))?;
            if bytes.iter().take(4096).filter(|b| **b == 0).count() > 4 {
                return Err(format!("{} is not a text file.", path.display()));
            }
            Ok(String::from_utf8_lossy(&bytes).to_string())
        }
    }
}

/// A path Sir named, expanded ("~\Downloads\x.pdf", "%USERPROFILE%\…", "Downloads/x.pdf").
pub fn resolve_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() {
        return None;
    }
    let expanded = crate::apps::expand_env(raw);
    let p = if let Some(rest) = expanded.strip_prefix("~") {
        crate::apps::home_dir()?.join(rest.trim_start_matches(['\\', '/']))
    } else {
        PathBuf::from(&expanded)
    };
    if p.exists() {
        return Some(p);
    }
    let home = crate::apps::home_dir()?;
    [home.join(&expanded), user_folder("Downloads").join(&expanded), user_folder("Documents").join(&expanded), user_folder("Desktop").join(&expanded)].into_iter().find(|c| c.exists())
}

/// A text bar for Markdown cards: `██████░░░░ 60 %`.
pub fn bar(fraction: f64, width: usize) -> String {
    let f = fraction.clamp(0.0, 1.0);
    let full = (f * width as f64).round() as usize;
    format!("{}{} {:.0} %", "█".repeat(full), "░".repeat(width - full.min(width)), f * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(text: &str) -> Option<&'static str> {
        route_text(text).map(|r| r.tool)
    }

    #[test]
    fn field_system_routes() {
        assert_eq!(tool("Jarvis, clip that"), Some("save_instant_replay"));
        assert_eq!(route_text("save the last 30 seconds").unwrap().args["seconds"], 30);
        assert_eq!(tool("quiz me on the french revolution"), Some("start_oral_exam_session"));
        assert_eq!(tool("why is my ping so high"), Some("diagnose_network_and_ping"));
        assert_eq!(route_text("lan scan").unwrap().args["action"], "scan_lan");
        // The radar's now (`intents::panel_request`).
        for text in ["scan my network", "Hey Jarvis, check the network.", "Who's on my wifi?"] {
            assert!(route_text(text).is_none(), "{text}");
        }
        let net = route_text("Diagnose my network").unwrap();
        assert_eq!((net.tool, net.args["action"].as_str()), ("diagnose_network_and_ping", Some("full")));
        assert_eq!(route_text("clip it").unwrap().tool, "save_instant_replay");
        assert_eq!(route_text("Jarvis clipped it").unwrap().tool, "save_instant_replay");
        assert_eq!(tool("I'm going shopping later"), None);
        let r = route_text("I spent 12,50 euro on pizza").unwrap();
        assert_eq!((r.tool, r.args["action"].as_str(), r.args["amount_eur"].as_f64()), ("manage_finances_and_goals", Some("add_expense"), Some(12.5)));
        assert_eq!(route_text("clean up my downloads folder").unwrap().args["target_folder"], "downloads");
        let a = route_text("wake me at 6:30 with JaysRock").unwrap();
        assert_eq!((a.tool, a.args["wake_time"].as_str(), a.args["playlist_name"].as_str()), ("set_smart_morning_alarm", Some("06:30"), Some("JaysRock")));
        assert_eq!(route_text("set an alarm for 7 pm").unwrap().args["wake_time"], "19:00");
        let m = route_text("download this video as mp3 from 1:20 to 2:05").unwrap();
        assert_eq!((m.args["format"].as_str(), m.args["start_time"].as_str()), (Some("mp3"), Some("1:20")));
        assert_eq!(route_text("do some deep research on solid state batteries").unwrap().args["depth"], "deep");
        let p = route_text("create a project called StarkNet in rust on my second monitor").unwrap();
        assert_eq!((p.args["template_type"].as_str(), p.args["target_monitor"].as_str()), (Some("Rust_Binary"), Some("2")));
        assert_eq!(p.args["project_name"], "StarkNet");
        assert_eq!(route_text("wake me at 6:30 with playlist JaysRock").unwrap().args["playlist_name"], "JaysRock");
        assert_eq!(tool("why did my minecraft crash"), Some("diagnose_minecraft_or_server"));
        assert_eq!(tool("send this to my phone"), Some("beam_to_device"));
        assert_eq!(tool("calculate the orbit period in python"), Some("run_sandbox_calculation_or_script"));
        assert_eq!(tool("stress test my plan"), Some("stress_test_idea_or_code"));
        assert_eq!(tool("what's the weather"), None);
        // Statements are not commands.
        assert_eq!(tool("why is minecraft so popular"), None);
        assert_eq!(tool("my alarm didn't go off at 7"), None);
        assert_eq!(tool("I'm starting a new project at school"), None);
        assert_eq!(tool("research shows that coffee is healthy"), None);
        assert_eq!(tool("I have a stress test at the doctor"), None);
        assert_eq!(tool("I'm learning to plot graphs in python"), None);
        assert_eq!(tool("new rust project called Forge"), Some("spawn_coding_project"));
        // A plain "research X" is left to the HUD board; the dossier is asked for by name.
        assert_eq!(tool("research solid state batteries"), None);
        assert!(matches!(crate::intents::parse("research solid state batteries"), Some(crate::intents::Intent::Tool { name: "hud_board", .. })));
        assert_eq!(tool("do some deep research on solid state batteries"), Some("start_background_research"));
        assert_eq!(tool("background research on solid state batteries"), Some("start_background_research"));
        assert_eq!(tool("research solid state batteries for two hours"), Some("manage_deep_research"));
        let deep = route_text("Research solid-state batteries for 3 hours, focus on cost and safety").expect("deep research route");
        assert_eq!(deep.args["topic"], "solid-state batteries");
        assert_eq!(deep.args["duration_minutes"], 180);
        assert_eq!(deep.args["focus_areas"], json!(["cost", "safety"]));
        assert_eq!(tool("who is on my minecraft server"), Some("diagnose_minecraft_or_server"));
        assert_eq!(bar(0.5, 10), "█████░░░░░ 50 %");
        for (t, _) in TOOL_NODES {
            assert!(tool_definitions().iter().any(|d| d["function"]["name"] == *t), "{t} has no schema");
        }
    }
}
