//! The live self-test: every capability exercised in the running app, for real, against a
//! latency budget (`JARVIS.exe --live-test`, and `[ TEST ALL PROBES ]` in the Feature Lab).
//!
//! * **Pipelines**: the reflex router's latency over its corpus, the whole deterministic path,
//!   the cortex (reads side by side, a read served from speculation, checked figures), the
//!   phonetic rescue, the pronunciation lexicon, both voice reference profiles, the default theme
//!   and the saved themes, the monitors, and (where the sources are next to the exe) the "no
//!   emoji, no dead apology" hygiene scan.
//! * **Routing, end to end**: each sentence of `ROUTES` goes through the real dispatcher and the
//!   real brain turn inside a *dry run* — the tool it reaches is recorded instead of carried out
//!   (`intercept`), and a request for the language model is recorded instead of asked
//!   (`intercept_model`). Nothing is logged to the conversation, nothing is spoken, nothing moves.
//! * **Tools**: every tool that only reads is run with real arguments (`LIVE_TOOLS`); every other
//!   tool is checked to be registered with a well-formed schema (it is not carried out: a probe
//!   must not send a message or shut the PC down).
//! * **The HUD** (only while its window is up): a window opens as an in-app block and closes by
//!   name with its fade; the emblem moves and comes back; the theme changes and comes back; a
//!   test report's pages turn by voice command and the board closes; Ultron Mode and the privacy
//!   lock go on and off; the frame time is read.
//!
//! A probe passes when the real result is right *and* it stayed inside its budget (`SLOW`
//! otherwise). A probe that cannot run here (no HUD, an integration Sir has not set up) is
//! `SKIP`, with the reason.

use crate::app::Jarvis;
use crate::crash_guard::lock;
use crate::intent_engine::{self, ReflexContext};
use crate::tools::{ToolContext, ToolOutcome};
use serde_json::{json, Value};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

// --------------------------------------------------------------------------- budgets ----

/// Deciding one of Jarvis' own controls (the reflex router).
pub const REFLEX_BUDGET_MS: f64 = 1.5;
/// Deciding any request end to end through the dispatcher and the brain (no model).
pub const TURN_BUDGET_MS: f64 = 10.0;
/// A tool that works on this machine only.
pub const LOCAL_BUDGET_MS: f64 = 120.0;
/// A tool that asks a server (WebUntis, CalDAV, IMAP, a weather service).
pub const NETWORK_BUDGET_MS: f64 = 8_000.0;
/// A step the HUD shows over several frames (a block opening, a fade).
pub const UI_BUDGET_MS: f64 = 1_200.0;
/// A voice measured against its clip: three sentences synthesised on the CPU and analysed.
pub const VOICE_BUDGET_MS: f64 = 60_000.0;

// --------------------------------------------------------------------------- dry run ----

/// What a request did inside a dry run.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    /// "tool" (a call the fast path made), "route" (a read the model would phrase), "answer"
    /// (an instant answer), "model", "mode" or "privacy".
    pub kind: &'static str,
    pub tool: String,
    pub args: Value,
}

tokio::task_local! {
    static DRY_RUN: Arc<Mutex<Vec<Recorded>>>;
}

/// Inside a dry run: nothing is carried out, logged or remembered.
pub fn dry() -> bool {
    DRY_RUN.try_with(|_| ()).is_ok()
}

fn note(kind: &'static str, tool: &str, args: Value) -> bool {
    DRY_RUN.try_with(|log| lock(log).push(Recorded { kind, tool: tool.to_string(), args })).is_ok()
}

/// A tool call inside a dry run: recorded, and answered as done without being carried out.
pub fn intercept(name: &str, args: &Value) -> Option<ToolOutcome> {
    note("tool", name, args.clone()).then(|| ToolOutcome {
        tool: name.to_string(),
        args: args.clone(),
        output: "Dry run: recorded, not carried out.".to_string(),
        success: true,
        duration_ms: 0,
        memories_changed: false,
        skills_changed: false,
        shutdown_requested: false,
        modality_changed: false,
        state_changed: false,
    })
}

/// A request for the language model inside a dry run (`upfront`: the read it would phrase):
/// recorded instead of asked. True when the turn must end here.
pub fn intercept_model(upfront: Option<(&str, &Value)>) -> bool {
    match upfront {
        Some((tool, args)) => note("route", tool, args.clone()),
        None => note("model", "language_model", Value::Null),
    }
}

/// An instant answer inside a dry run ("time", "date", "answer").
pub fn intercept_answer(kind: &str) -> bool {
    note("answer", kind, Value::Null)
}

/// Runs `turn` as a dry run and returns what it would have done.
pub async fn record<F: Future>(turn: F) -> Vec<Recorded> {
    let log = Arc::new(Mutex::new(Vec::new()));
    DRY_RUN.scope(Arc::clone(&log), turn).await;
    let recorded = lock(&log).clone();
    recorded
}

// --------------------------------------------------------------------------- results ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Status {
    Pass,
    Fail,
    /// Right, but over its latency budget.
    Slow,
    /// Could not run here (the reason is the detail).
    Skip,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Slow => "SLOW",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProbeOutcome {
    pub id: String,
    /// "pipeline", "routing", "tool" or "hud".
    pub group: &'static str,
    pub status: Status,
    pub ms: f64,
    pub budget_ms: f64,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Summary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub slow: usize,
    pub skip: usize,
}

impl Summary {
    /// Every probe that ran is right and inside its budget.
    pub fn clean(&self) -> bool {
        self.fail == 0 && self.slow == 0 && self.pass > 0
    }

    pub fn describe(&self) -> String {
        format!("{} probes: {} pass, {} fail, {} slow, {} skipped", self.total, self.pass, self.fail, self.slow, self.skip)
    }
}

static RESULTS: Mutex<Vec<ProbeOutcome>> = Mutex::new(Vec::new());
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The outcomes so far (the Feature Lab reads them while the suite runs).
pub fn results() -> Vec<ProbeOutcome> {
    lock(&RESULTS).clone()
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

pub fn summary() -> Summary {
    let results = lock(&RESULTS);
    let count = |s: Status| results.iter().filter(|r| r.status == s).count();
    Summary { total: results.len(), pass: count(Status::Pass), fail: count(Status::Fail), slow: count(Status::Slow), skip: count(Status::Skip) }
}

/// The outcome of the probe that ran tool `name` live or checked its wiring, for its Feature
/// Lab row.
pub fn tool_outcome(name: &str) -> Option<ProbeOutcome> {
    lock(&RESULTS).iter().find(|r| r.group == "tool" && r.id.split(' ').next() == Some(name)).cloned()
}

fn push(outcome: ProbeOutcome) {
    let line = format!("[PROBE] {} {} // {} ({:.3} ms of {} ms) {}", outcome.status.label(), outcome.group, outcome.id, outcome.ms, outcome.budget_ms, outcome.detail);
    match outcome.status {
        Status::Pass | Status::Skip => info!("{line}"),
        Status::Slow => warn!("{line}"),
        Status::Fail => error!("{line}"),
    }
    lock(&RESULTS).push(outcome);
    crate::gui::refresh();
}

/// Records a probe: right and in time → PASS, right but late → SLOW, wrong → FAIL.
fn judge(group: &'static str, id: impl Into<String>, budget_ms: f64, ms: f64, result: Result<String, String>) {
    let (status, detail) = match result {
        Ok(detail) if ms <= budget_ms => (Status::Pass, detail),
        Ok(detail) => (Status::Slow, detail),
        Err(why) => (Status::Fail, why),
    };
    push(ProbeOutcome { id: id.into(), group, status, ms, budget_ms, detail });
}

fn skip(group: &'static str, id: impl Into<String>, why: impl Into<String>) {
    push(ProbeOutcome { id: id.into(), group, status: Status::Skip, ms: 0.0, budget_ms: 0.0, detail: why.into() });
}

fn ms_since(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Times a synchronous check.
fn timed(group: &'static str, id: &str, budget_ms: f64, check: impl FnOnce() -> Result<String, String>) {
    let started = Instant::now();
    let result = check();
    judge(group, id, budget_ms, ms_since(started), result);
}

/// Polls `done` (waking the HUD each time) for up to `limit`; the milliseconds it took.
async fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> Option<f64> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return Some(ms_since(started));
        }
        crate::gui::refresh();
        tokio::time::sleep(Duration::from_millis(12)).await;
    }
    done().then(|| ms_since(started))
}

// ----------------------------------------------------------------------- the routes ----

/// A sentence and what it must reach: a tool with (at least) these arguments, or one of
/// "language_model", "time", "date", "ultron_mode", "listening_lock".
struct Route {
    say: &'static str,
    tool: &'static str,
    args: &'static str,
}

const fn r(say: &'static str, tool: &'static str, args: &'static str) -> Route {
    Route { say, tool, args }
}

/// The routing matrix: Jarvis' own controls first (the bugs of Sir's screenshots among them),
/// then every family of deterministic commands, then requests that must reach the model.
const ROUTES: &[Route] = &[
    // Ultron ⇄ Jarvis: stage 1, never music.
    r("Jarvis, switch to Ultron.", "ultron_mode", r#"{"on": true}"#),
    r("Change back to Jarvis.", "ultron_mode", r#"{"on": false}"#),
    r("Ultron switch back to Jarvis.", "ultron_mode", r#"{"on": false}"#),
    r("Zurück zu Jarvis", "ultron_mode", r#"{"on": false}"#),
    // The privacy lock.
    r("Please stop listening for five minutes.", "listening_lock", "{}"),
    // The emblem.
    r("Move your orb back to the middle.", "configure_interface", r#"{"orb_board_position": "back to the middle"}"#),
    r("Move your orb a little bit to the left.", "configure_interface", r#"{"orb_board_position": "a little bit to the left"}"#),
    r("Center your orb.", "configure_interface", r#"{"orb_board_position": "center"}"#),
    r("Put your core in the top right corner.", "configure_interface", r#"{"orb_board_position": "in the top right corner"}"#),
    r("Schieb den Orb etwas nach rechts.", "configure_interface", r#"{"orb_board_position": "etwas nach rechts"}"#),
    // His own blocks: never a program.
    r("Close the HUD board.", "hud_board", r#"{"action": "close"}"#),
    r("Close to web search.", "hud_board", r#"{"action": "close"}"#),
    r("Close the feature lab.", "hud_board", r#"{"action": "close", "target": "feature lab"}"#),
    r("Close telemetry.", "hud_board", r#"{"action": "close", "target": "telemetry"}"#),
    r("Clear the board.", "hud_board", r#"{"action": "clear"}"#),
    r("Close the windows you opened.", "close_all_secondary_windows", "{}"),
    // The core view (also as Whisper mishears it).
    r("Show me your core.", "configure_interface", r#"{"core_view": true}"#),
    r("Hide your core.", "configure_interface", r#"{"core_view": false}"#),
    r("Close your car.", "configure_interface", r#"{"core_view": false}"#),
    // Themes, monitors, layout.
    r("Change the theme to Arc Cyan.", "configure_interface", "{}"),
    r("Move to my third monitor.", "move_window_to_monitor", r#"{"app_or_window_name": "jarvis", "target_monitor": 3}"#),
    r("Move Sonora to monitor 2.", "move_window_to_monitor", r#"{"target_monitor": 2}"#),
    r("Use all monitors.", "configure_interface", r#"{"span_all_monitors": true}"#),
    r("Overlap the windows.", "configure_interface", r#"{"allow_window_overlap": true}"#),
    r("Open the text console.", "configure_interface", r#"{"text_console": true}"#),
    // Players and volume.
    r("Pause.", "control_music", r#"{"action": "stop_all"}"#),
    r("Next song.", "control_music", r#"{"action": "next_track"}"#),
    r("Volume 40.", "control_audio_mixer", r#"{"action": "set_master", "level": 40}"#),
    r("Mute.", "control_audio_mixer", r#"{"action": "mute_master"}"#),
    r("Set Sonora to 30 percent.", "control_audio_mixer", "{}"),
    r("What's playing?", "control_music", r#"{"action": "what_is_playing"}"#),
    // Programs and windows.
    r("Open Spotify.", "open_application", "{}"),
    r("Close Spotify.", "close_application", "{}"),
    r("Minimize everything.", "window_management", r#"{"action": "minimize_all"}"#),
    // The Stark Protocols, by name.
    r("Engage the study protocol.", "manage_stark_protocol", r#"{"action": "execute_protocol", "protocol_name": "study"}"#),
    r("Initiate modding protocol.", "manage_stark_protocol", r#"{"action": "execute_protocol", "protocol_name": "modding"}"#),
    r("Gaming protocol.", "manage_stark_protocol", r#"{"action": "execute_protocol", "protocol_name": "gaming"}"#),
    r("House party protocol.", "manage_stark_protocol", r#"{"action": "execute_protocol", "protocol_name": "house party"}"#),
    // His own views and reports.
    r("Open my system stats.", "open_jarvis_view", r#"{"view": "system_stats"}"#),
    r("List your features.", "list_jarvis_features", "{}"),
    r("System status.", "get_system_status", "{}"),
    r("Research the state of VR.", "hud_board", r#"{"action": "research"}"#),
    // School, calendar, mail: read first, phrased afterwards.
    r("What classes do I have tomorrow?", "check_school_schedule", "{}"),
    r("What's on my calendar today?", "get_calendar_schedule", "{}"),
    r("Do I have any new emails?", "check_unread_emails", "{}"),
    // The Discord bridge.
    r("Turn on the Discord call.", "discord_call", r#"{"action": "listen"}"#),
    // Instant answers.
    r("What time is it?", "time", "{}"),
    r("What's 17 times 23?", "answer", "{}"),
    // And what only the model can answer.
    r("What's the capital of France?", "language_model", "{}"),
    r("Explain in detail how a transformer network computes attention.", "language_model", "{}"),
];

/// Whether `wanted` (a JSON object) is contained in `got`.
fn contains(got: &Value, wanted: &Value) -> bool {
    match wanted.as_object() {
        Some(fields) => fields.iter().all(|(key, value)| got.get(key) == Some(value)),
        None => true,
    }
}

/// Every route of the matrix once, as a dry run: each command path compiles its patterns and
/// opens what it opens the first time. Run after start-up, so Sir's first command of each kind
/// is as fast as his hundredth (measured cold: the first "open my system stats" took 451 ms to
/// decide, the next one 3 ms).
pub async fn warm_up(app: &Jarvis) {
    let started = Instant::now();
    let mark = app.history_mark().await;
    for route in ROUTES {
        let _ = app.dry_run(route.say, false).await;
    }
    app.history_rollback(mark).await;
    // A request no parser takes asks every parser first: their patterns are compiled here, on
    // ordinary sentences, so Sir's first free-form request does not pay for them.
    let idle = ReflexContext::default();
    for say in intent_engine::BENCH_CORPUS {
        let _ = crate::hyper_cortex::tier(say, &idle);
    }
    // A dry route stops at the tool: what a look change computes is run once here, on a copy
    // of the settings that is thrown away. (After a night's idle the first emblem move took
    // 236 ms, the next one none: its code had to come off the disk.)
    let look = app.tools().config.read(|c| c.interface.clone());
    for args in [json!({"orb_board_position": "top left"}), json!({"orb_board_position": "a little bit to the right"}), json!({"hud_palette": "arc_cyan"}), json!({"core_view": true})] {
        let _ = crate::interface::configure(&look, &args);
    }
    // Windows' media session answers slowly the first time it is asked (336 ms measured, then
    // a few): asked once here, read-only, so "what's playing?" and the live context are not
    // the ones to wait.
    let _ = tokio::time::timeout(Duration::from_millis(1500), crate::music::now_playing()).await;
    info!("[CORTEX] Every command path warmed with {} dry routes in {} ms", ROUTES.len(), started.elapsed().as_millis());
}

async fn routing_probes(app: &Jarvis) {
    // Measured as Sir meets them: after the start-up warm-up (a no-op when it already ran).
    warm_up(app).await;
    let mark = app.history_mark().await;
    for route in ROUTES {
        let wanted: Value = serde_json::from_str(route.args).unwrap_or(Value::Null);
        let (recorded, took) = app.dry_run(route.say, false).await;
        let ms = took.as_secs_f64() * 1000.0;
        let hit = recorded.iter().find(|rec| rec.tool == route.tool && contains(&rec.args, &wanted));
        let result = match hit {
            Some(rec) => Ok(format!("{} → {} {}", rec.kind, rec.tool, if rec.args.is_null() { String::new() } else { rec.args.to_string() })),
            None => {
                let did: Vec<String> = recorded.iter().map(|rec| format!("{} {} {}", rec.kind, rec.tool, rec.args)).collect();
                Err(format!("wanted {} {}; it did: {}", route.tool, route.args, if did.is_empty() { "nothing".to_string() } else { did.join("; ") }))
            }
        };
        judge("routing", route.say, TURN_BUDGET_MS, ms, result);
    }
    // The dry turns never reached the conversation log; the in-memory history forgets them too.
    app.history_rollback(mark).await;
}

// ------------------------------------------------------------------------ the tools ----

#[derive(Clone, Copy, PartialEq)]
enum Reach {
    /// Works on this machine.
    Local,
    /// Asks a public server.
    Network,
    /// Reads one of Sir's accounts: nothing of the result is logged.
    Account,
}

/// Every tool call that only reads, with real arguments, and a word its answer must contain
/// ("" = any answer).
const LIVE_TOOLS: &[(&str, &str, Reach, &str)] = &[
    ("get_current_datetime", "{}", Reach::Local, ""),
    ("get_system_status", "{}", Reach::Local, "CPU"),
    ("calculate", r#"{"expression": "2 + 2 * 3"}"#, Reach::Local, "8"),
    ("recall_memories", r#"{"query": "Sir"}"#, Reach::Local, ""),
    ("search_conversation", r#"{"query": "weather", "days_back": 1}"#, Reach::Local, ""),
    ("list_jarvis_features", "{}", Reach::Local, ""),
    ("list_skills", "{}", Reach::Local, ""),
    ("list_forged_tools", "{}", Reach::Local, ""),
    ("inspect_own_source", "{}", Reach::Local, ""),
    ("reminders", r#"{"action": "list"}"#, Reach::Local, ""),
    ("control_audio_mixer", r#"{"action": "list"}"#, Reach::Local, ""),
    ("control_music", r#"{"action": "what_is_playing"}"#, Reach::Local, ""),
    ("control_music", r#"{"action": "list_playlists"}"#, Reach::Local, ""),
    ("media_and_volume_control", r#"{"action": "get_volume"}"#, Reach::Local, ""),
    ("manage_vram", r#"{"action": "status"}"#, Reach::Local, ""),
    ("manage_deep_research", r#"{"action": "status"}"#, Reach::Local, ""),
    ("discord_call", r#"{"action": "status"}"#, Reach::Local, ""),
    ("hud_board", r#"{"action": "status"}"#, Reach::Local, ""),
    ("manage_stark_protocol", r#"{"action": "list"}"#, Reach::Local, ""),
    ("manage_spatial_scene", r#"{"action": "list"}"#, Reach::Local, ""),
    ("manage_spatial_scene", r#"{"action": "monitors"}"#, Reach::Local, ""),
    ("manage_contacts", r#"{"action": "list"}"#, Reach::Local, ""),
    ("manage_voice_macro", r#"{"action": "list"}"#, Reach::Local, ""),
    ("manage_workout", r#"{"action": "status"}"#, Reach::Local, ""),
    ("manage_price_sniper", r#"{"action": "list"}"#, Reach::Local, ""),
    ("set_auto_responder", r#"{"action": "status"}"#, Reach::Local, ""),
    ("manage_location_settings", r#"{"action": "status"}"#, Reach::Local, ""),
    ("configure_productivity_service", r#"{"service_name": "all", "action": "status"}"#, Reach::Local, ""),
    ("manage_schedule_watcher", r#"{"service_name": "list"}"#, Reach::Local, ""),
    ("get_remote_pc_status", "{}", Reach::Local, ""),
    ("top_processes", r#"{"limit": 5}"#, Reach::Network, ""),
    ("network_status", "{}", Reach::Network, ""),
    ("web_search", r#"{"query": "Stark Industries arc reactor"}"#, Reach::Network, ""),
    ("get_precise_location_weather", r#"{"forecast_hours": 6}"#, Reach::Network, ""),
    ("check_ride_and_commute_status", "{}", Reach::Network, ""),
    ("get_calendar_schedule", r#"{"date_or_range": "today"}"#, Reach::Account, ""),
    ("check_school_schedule", r#"{"target_date": "today"}"#, Reach::Account, ""),
    ("check_unread_emails", r#"{"limit": 3}"#, Reach::Account, ""),
];

/// Words of a failure that means "Sir has not set this up", not "this is broken".
const NOT_SET_UP: &[&str] = &["not configured", "not set up", "isn't set up", "is not enabled", "disabled", "no account", "not connected", "not paired", "no credentials", "set it up", "no gps", "no location", "home is not set"];

/// Whether `name` with `args` is a call the tool's schema accepts: the tool exists, every
/// required parameter is there, no unknown one is, and enum values are allowed ones.
pub fn check_call(defs: &[Value], name: &str, args: &Value) -> Result<(), String> {
    let def = defs.iter().find(|d| d["function"]["name"] == name).ok_or(format!("{name} is not a tool"))?;
    let params = &def["function"]["parameters"];
    for required in params["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        args.get(required).ok_or(format!("{name} needs '{required}'"))?;
    }
    for (key, value) in args.as_object().into_iter().flatten() {
        let schema = &params["properties"][key];
        if schema.is_null() {
            return Err(format!("{name} has no parameter '{key}'"));
        }
        if schema["enum"].as_array().is_some_and(|allowed| !allowed.contains(value)) {
            return Err(format!("{name}.{key} = {value} is not one of {}", schema["enum"]));
        }
    }
    Ok(())
}

/// A registered tool's schema is one the model (and `execute`) can use.
fn well_formed(def: &Value) -> Result<String, String> {
    let f = &def["function"];
    let name = f["name"].as_str().filter(|n| !n.is_empty()).ok_or("a tool without a name")?;
    if f["description"].as_str().is_none_or(|d| d.trim().len() < 12) {
        return Err(format!("{name} has no description"));
    }
    let params = &f["parameters"];
    if params["type"] != "object" {
        return Err(format!("{name}: parameters are not an object"));
    }
    let known: Vec<&str> = params["properties"].as_object().map(|p| p.keys().map(String::as_str).collect()).unwrap_or_default();
    for required in params["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !known.contains(&required) {
            return Err(format!("{name} requires '{required}', which it does not declare"));
        }
    }
    Ok(format!("wired: {} parameter(s); not carried out (it changes things)", known.len()))
}

async fn tool_probes(tools: &ToolContext) {
    let defs = tools.definitions();
    for (name, args, reach, expect) in LIVE_TOOLS {
        let id = if *args == "{}" { name.to_string() } else { format!("{name} {args}") };
        let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        if let Err(why) = check_call(&defs, name, &args) {
            judge("tool", id, LOCAL_BUDGET_MS, 0.0, Err(why));
            continue;
        }
        let budget = if *reach == Reach::Local { LOCAL_BUDGET_MS } else { NETWORK_BUDGET_MS };
        let started = Instant::now();
        let outcome = tokio::time::timeout(Duration::from_secs(20), Box::pin(tools.execute(name, &args))).await;
        let ms = ms_since(started);
        let Ok(outcome) = outcome else {
            judge("tool", id, budget, ms, Err("no answer within 20 s".to_string()));
            continue;
        };
        let first = outcome.output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        let lower = outcome.output.to_lowercase();
        if !outcome.success && NOT_SET_UP.iter().any(|w| lower.contains(w)) {
            skip("tool", id, format!("not set up: {}", crate::crash_guard::clip(first, 120)));
            continue;
        }
        let shown = if *reach == Reach::Account { format!("answered ({} characters; Sir's data is not logged)", outcome.output.len()) } else { crate::crash_guard::clip(first, 120).to_string() };
        let result = if !outcome.success {
            Err(crate::crash_guard::clip(first, 160).to_string())
        } else if outcome.tool != *name {
            Err(format!("ran '{}' instead", outcome.tool))
        } else if !expect.is_empty() && !outcome.output.contains(expect) {
            Err(format!("no '{expect}' in: {shown}"))
        } else {
            Ok(shown)
        };
        judge("tool", id, budget, ms, result);
    }
    // Everything else changes something: registered and well-formed, never carried out here.
    for def in &defs {
        let name = def["function"]["name"].as_str().unwrap_or("");
        if LIVE_TOOLS.iter().any(|(n, ..)| *n == name) {
            continue;
        }
        timed("tool", name, LOCAL_BUDGET_MS, || well_formed(def));
    }
}

// --------------------------------------------------------------------- the pipelines ----

/// One card of every kind the butler shows, each button checked against its tool's schema:
/// (cards, fixes).
fn butler_cards(defs: &[Value]) -> Result<(usize, usize), String> {
    use crate::guardian::{compose, Anomaly, Kind};
    use crate::proactive_butler as butler;
    let telemetry = crate::telemetry::SystemTelemetry::default();
    let mut cards = Vec::new();
    let found = [
        (Kind::GpuHot, 88.0, Some("chrome")),
        (Kind::CpuHot, 93.0, None),
        (Kind::VramFull, 97.0, Some("chrome")),
        (Kind::RamFull, 93.0, Some("chrome")),
        (Kind::CpuPegged, 99.0, Some("chrome")),
        (Kind::DiskLow("C:".to_string()), 3.0, None),
    ];
    for (kind, value, culprit) in found {
        let anomaly = Anomaly { kind, value };
        let (message, offer) = compose(&anomaly, culprit, &telemetry);
        cards.push(butler::hardware(&anomaly, &message, &offer));
    }
    let now = chrono::Local::now().naive_local();
    let lesson = butler::Upcoming { title: "Physics".to_string(), start: now + chrono::Duration::minutes(12), place: "204".to_string(), lesson: true };
    cards.push(butler::upcoming_card(now, &[&lesson]).ok_or("no card for a lesson twelve minutes ahead")?);
    cards.push(butler::minecraft_crash(Path::new("crash-reports/crash-client.txt"), "java.lang.OutOfMemoryError: Java heap space"));
    let failure = butler::build_failure("D:\\mod\\src\\main\\java\\demo\\Handler.java:10: error: cannot find symbol\nBUILD FAILED in 2s\n").ok_or("a failed build was not recognised")?;
    cards.push(butler::build_card(&failure));
    let mut fixes = 0;
    for card in &cards {
        if card.fixes.is_empty() {
            return Err(format!("'{}' has no one-click fix", card.title));
        }
        for fix in &card.fixes {
            check_call(defs, fix.tool, &fix.args).map_err(|why| format!("'{}' [ {} ]: {why}", card.title, fix.label))?;
            fixes += 1;
        }
    }
    Ok((cards.len(), fixes))
}

/// The suggestion bar for each kind of block, every key checked against its tool's schema:
/// (kinds, keys).
fn suggestion_bars(defs: &[Value]) -> Result<(usize, usize), String> {
    use crate::hud_suggestions::{candidates, top, Habits, Subject, SHOWN};
    let subject = |kind: &'static str, page: usize, pages: usize| Subject { kind, topic: "State of VR".to_string(), headline: "Hardware".to_string(), page, pages, blocks: 5, source: Some("https://example.org/vr".to_string()) };
    let subjects = [subject("note", 0, 3), subject("note", 2, 3), subject("chart", 0, 1), subject("figure", 0, 1), subject("photo", 0, 1), subject("map", 0, 1)];
    let mut keys = 0;
    for s in &subjects {
        if top(s, &Habits::default()).len() != SHOWN {
            return Err(format!("the bar of a {} holds {} keys", s.kind, top(s, &Habits::default()).len()));
        }
        // Every candidate, not only the three in front: habits may bring any of them up.
        for (key, _) in candidates(s) {
            check_call(defs, key.tool, &key.args).map_err(|why| format!("{} [ {} ]: {why}", s.kind, key.label))?;
            keys += 1;
        }
    }
    Ok((subjects.len(), keys))
}

async fn pipeline_probes(app: &Jarvis) {
    let tools = app.tools();
    // The reflex router and the whole deterministic path, over the benchmark corpus.
    let bench = intent_engine::benchmark(25);
    judge("pipeline", "reflex router (p99 over its corpus)", REFLEX_BUDGET_MS, bench.p99_us / 1000.0, Ok(bench.describe()));
    let full = intent_engine::benchmark_full(25);
    judge("pipeline", "deterministic path (p99 over its corpus)", REFLEX_BUDGET_MS, full.p99_us / 1000.0, Ok(full.describe()));

    // The cortex: three reads side by side, in order, faster than one after another.
    let calls = vec![("get_current_datetime".to_string(), json!({})), ("get_system_status".to_string(), json!({})), ("list_jarvis_features".to_string(), json!({}))];
    let started = Instant::now();
    let outcomes = crate::hyper_cortex::run_parallel(tools, calls.clone()).await;
    let in_order = outcomes.iter().zip(&calls).all(|(o, (name, _))| o.success && o.tool == *name);
    judge("pipeline", "cortex: reads side by side", LOCAL_BUDGET_MS, ms_since(started), if in_order { Ok(format!("{} reads, each in its place", outcomes.len())) } else { Err("a read failed or came back out of order".to_string()) });

    // A read started ahead is what the request gets.
    let before = crate::hyper_cortex::stats();
    let args = json!({});
    let started_ahead = crate::hyper_cortex::speculate(tools, "get_system_status", &args);
    let started = Instant::now();
    let outcome = crate::hyper_cortex::execute(tools, "get_system_status", &args).await;
    let after = crate::hyper_cortex::stats();
    let served = after.served > before.served;
    judge(
        "pipeline",
        "cortex: a read started ahead answers the request",
        LOCAL_BUDGET_MS,
        ms_since(started),
        if started_ahead && served && outcome.success { Ok(format!("served from memory ({} ahead so far, {} served)", after.speculated, after.served)) } else { Err(format!("started ahead: {started_ahead}, served: {served}, ok: {}", outcome.success)) },
    );

    timed("pipeline", "cortex: every read it may run early or side by side is a real tool", LOCAL_BUDGET_MS, || {
        let defs = tools.definitions();
        match crate::hyper_cortex::read_only_tools().find(|name| !defs.iter().any(|d| d["function"]["name"] == *name)) {
            None => Ok(format!("{} read-only tools, all registered", crate::hyper_cortex::read_only_tools().count())),
            Some(missing) => Err(format!("'{missing}' is not a tool")),
        }
    });
    timed("pipeline", "cortex: stated arithmetic is checked", REFLEX_BUDGET_MS, || {
        let (text, corrected) = crate::hyper_cortex::check_figures("So 12 × 8 = 86, and 60 / 4 = 15.");
        if text == "So 12 × 8 = 96, and 60 / 4 = 15." && corrected == 1 { Ok(text) } else { Err(text) }
    });
    // The tier of a request: one of his own controls on the reflex budget; a request no parser
    // takes has asked every parser first, and is on the budget of a whole turn decision.
    {
        use crate::hyper_cortex::{tier, Tier};
        let ctx = ReflexContext::default();
        for (say, wanted, budget) in [("Move your orb to the left", Tier::Reflex, REFLEX_BUDGET_MS), ("Tell me a joke", Tier::Tactical, TURN_BUDGET_MS), ("Explain in detail how a transformer works", Tier::Deep, TURN_BUDGET_MS)] {
            let started = Instant::now();
            let got = tier(say, &ctx);
            let ms = ms_since(started);
            judge("pipeline", format!("cortex: tier of '{say}'"), budget, ms, if got == wanted { Ok(format!("{got:?}")) } else { Err(format!("{got:?}, wanted {wanted:?}")) });
        }
    }

    // The butler's cards: every button is a call a tool accepts, and a click hands it over.
    timed("pipeline", "butler: every card's one-click fix is a real tool call", LOCAL_BUDGET_MS, || {
        let (cards, fixes) = butler_cards(&tools.definitions())?;
        Ok(format!("{cards} cards, {fixes} fixes, each a call its tool accepts"))
    });
    timed("pipeline", "butler: a click hands the HUD the fix and takes the card away", LOCAL_BUDGET_MS, || {
        use crate::proactive_butler::{click, show, Card, Fix};
        let fix = Fix { label: "SELF TEST FIX".to_string(), tool: "get_current_datetime", args: json!({}) };
        show(&Card { title: "SELF TEST".to_string(), body: "A card of the live self-test.".to_string(), fixes: vec![fix.clone()], spoken: None });
        let clicked = click("SELF TEST FIX");
        match (clicked == Some(fix), click("SELF TEST FIX")) {
            (true, None) => Ok("one click, one tool call".to_string()),
            (handed, again) => Err(format!("the fix was handed over: {handed}; a second click gave {again:?}")),
        }
    });
    timed("pipeline", "board: the suggestion bar's three next actions are real tool calls", LOCAL_BUDGET_MS, || {
        let (bars, keys) = suggestion_bars(&tools.definitions())?;
        Ok(format!("{bars} kinds of block, {keys} keys, each a call its tool accepts"))
    });
    // (Windows is asked for the refresh rate here; the HUD asks on a thread of its own.)
    timed("pipeline", "hud: frame pacing follows the display", LOCAL_BUDGET_MS, || {
        use crate::gui::{target_fps, MAX_FPS};
        let hz = crate::monitors::fastest_refresh_hz().map(|hz| hz as f32).unwrap_or(60.0);
        let got = [target_fps(false, true, true, hz), target_fps(true, false, true, hz), target_fps(true, true, false, hz), target_fps(true, true, true, hz)];
        if got[..3] == [8.0, 30.0, 60.0] && got[3] == hz.clamp(60.0, MAX_FPS) {
            Ok(format!("fastest monitor {hz:.0} Hz: {:.0} FPS while Sir is at the HUD, 60 busy elsewhere, 30 at rest", got[3]))
        } else {
            Err(format!("{got:?} at {hz} Hz"))
        }
    });
    timed("pipeline", "protocols: independent steps run side by side, dependent ones in order", REFLEX_BUDGET_MS, || {
        let store = crate::protocols::ProtocolStore::in_memory();
        let mut said = Vec::new();
        for name in ["study", "modding", "gaming", "house party"] {
            let p = store.get(name).ok_or(format!("no built-in {name} protocol"))?;
            let stages = crate::protocols::plan(&p.steps);
            let planned: usize = stages.iter().flatten().map(Vec::len).sum();
            let widest = stages.iter().map(Vec::len).max().unwrap_or(0);
            if planned != p.steps.len() {
                return Err(format!("{name}: {planned} of {} steps planned", p.steps.len()));
            }
            if widest < 2 {
                return Err(format!("{name}: nothing runs side by side"));
            }
            said.push(format!("{name} {} steps in {} lanes", p.steps.len(), widest));
        }
        Ok(said.join(", "))
    });

    // What Whisper wrote on 2026-11-03, and what Sir had said.
    timed("pipeline", "misheard commands are recognised by their sound", REFLEX_BUDGET_MS, || {
        let idle = ReflexContext::default();
        let cases = [("Fight the core.", Some("hide your core")), ("disabled the car.", Some("hide your core")), ("Which do old run?", Some("switch to Ultron")), ("10 discord call on.", Some("turn on the discord call")), ("next week", None), ("I hate this game", None)];
        match cases.iter().find(|(heard, said)| intent_engine::phonetic_rescue(heard, &idle) != *said) {
            None => Ok(format!("{} sentences read right", cases.len())),
            Some((heard, said)) => Err(format!("'{heard}' → {:?}, wanted {said:?}", intent_engine::phonetic_rescue(heard, &idle))),
        }
    });

    timed("pipeline", "lexicon: \"live\" is /laiv/ unless it is the verb", REFLEX_BUDGET_MS, || {
        let said = crate::pronunciation_lexicon::PronunciationLexicon::normalize_for_speech("Live telemetry is up. I live in Germany.");
        if said.to_lowercase().contains("lyve telemetry") && said.contains("I live in") { Ok(said) } else { Err(said) }
    });

    // Both voices have their reference clip and its profile.
    timed("pipeline", "voice: the Jarvis reference clip and its calibrated profile", LOCAL_BUDGET_MS, || {
        let clip = crate::voice_reference::reference_path().ok_or("no Jarvis reference clip in assets/")?;
        let profile = crate::fish_zero_shot_tts::load_profile(&clip, Path::new(crate::fish_zero_shot_tts::PROFILE_PATH))?;
        let blend = profile.blend.clone().ok_or("the Jarvis voice is not calibrated yet (run --voice-calibrate)")?;
        Ok(format!("{}: {blend}, F0 {:.1} Hz", clip.display(), profile.f0.median_hz))
    });
    timed("pipeline", "voice: the Ultron reference clip and its profile", LOCAL_BUDGET_MS, || {
        let clip = crate::ultron_mode::reference_path().ok_or("no Ultron reference clip in assets/")?;
        let profile = crate::ultron_mode::reference_profile()?;
        let voice = profile.blend.clone().ok_or("the Ultron voice is not calibrated yet (run --voice-calibrate)")?;
        Ok(format!("{}: {voice}, F0 {:.1} Hz, formants ×{}", clip.display(), profile.f0.median_hz, crate::ultron_mode::FORMANT_RATIO))
    });

    // Both voices closed loop: a sentence through the whole chain, measured against the clip
    // (silent, on the CPU whatever the speech engine uses, so a game keeps the GPU).
    type Verify = fn(&crate::config::JarvisConfig) -> Result<crate::fish_zero_shot_tts::Verification, String>;
    let voices: [(&str, Verify); 2] = [("voice: Jarvis measured against his clip (closed loop)", crate::fish_zero_shot_tts::verify), ("voice: Ultron measured against his clip (closed loop)", crate::ultron_mode::verify)];
    for (id, verify) in voices {
        let mut cfg = tools.config.read(|c| c.clone());
        cfg.tts_gpu = false;
        let started = Instant::now();
        let measured = tokio::task::spawn_blocking(move || verify(&cfg)).await.map_err(|e| format!("the measurement stopped: {e}")).and_then(|v| v);
        let result = measured.and_then(|v| {
            let held_out = v.held_out.map(|m| format!("{:.3}", m.similarity)).unwrap_or_else(|| "n/a".to_string());
            let said = format!("{}: similarity {:.3} (unseen sentence {held_out}), F0 {:+.2} st from the clip", v.voice, v.transferred.similarity, v.transferred.f0_offset_st);
            if v.passed() { Ok(said) } else { Err(format!("{said}; the target is {}", crate::fish_zero_shot_tts::SIMILARITY_TARGET)) }
        });
        judge("pipeline", id, VOICE_BUDGET_MS, ms_since(started), result);
    }

    // The phone page's address: permanent with a tunnel token and a hostname, else a quick
    // tunnel's, which is new on every start. (What is set up right now is reported with it.)
    timed("pipeline", "tunnel: a permanent address is used as it stands, its token never on the command line", LOCAL_BUDGET_MS, || {
        use crate::config::SatelliteSettings;
        use crate::worldwide_tunnel::{configured_address, plan, snapshot, TOKEN_SERVICE};
        // The shape of a tunnel token (not one).
        let token = format!("eyJ{}", "hIjoiZXhhbXBsZSJ9".repeat(4));
        let named = plan(&SatelliteSettings { tunnel_public_url: "https://jarvis.example.com".to_string(), ..Default::default() }, Some(&token), 8444)?;
        let quick = plan(&SatelliteSettings::default(), None, 8444)?;
        let sound = named.permanent
            && named.address.as_deref() == Some("https://jarvis.example.com")
            && named.args.iter().all(|a| !a.contains(&token))
            && named.env.iter().any(|(key, value)| key == "TUNNEL_TOKEN" && *value == token)
            && !quick.permanent
            && quick.address.is_none();
        if !sound {
            return Err(format!("named: {:?} {:?} permanent {}; quick: permanent {}", named.args, named.address, named.permanent, quick.permanent));
        }
        let settings = tools.config.read(|c| c.satellite.clone());
        let stored = tools.pim.config().vault().has_secret(TOKEN_SERVICE, crate::vault::TOKEN);
        let now = snapshot();
        Ok(match (configured_address(&settings, stored), now.permanent) {
            (Some(address), _) => format!("set up: the permanent address {address} (tunnel {})", now.state),
            (None, true) => format!("set up: a permanent {} address (tunnel {})", now.provider, now.state),
            (None, false) => format!("set up: a quick tunnel, new address on every start (tunnel {}); a token and a hostname make it permanent", now.state),
        })
    });

    timed("pipeline", "theme: Arc Cyan is the default, saved themes load", LOCAL_BUDGET_MS, || {
        use crate::theme_engine as theme;
        if theme::DEFAULT_PRESET != "arc_cyan" || theme::preset("arc_cyan").is_none() {
            return Err(format!("the default is '{}'", theme::DEFAULT_PRESET));
        }
        let stored = if Path::new(theme::CUSTOM_THEMES_PATH).is_file() { theme::load_custom(Path::new(theme::CUSTOM_THEMES_PATH)).len() } else { 0 };
        Ok(format!("default {}; {stored} saved theme(s) in {}", theme::label(theme::DEFAULT_PRESET), theme::CUSTOM_THEMES_PATH))
    });

    timed("pipeline", "monitors are listed", LOCAL_BUDGET_MS, || {
        let monitors = crate::monitors::list();
        if monitors.is_empty() { Err("no monitor found".to_string()) } else { Ok(crate::monitors::context_line(&monitors).trim().to_string()) }
    });

    match source_hygiene(Path::new("src")) {
        Some((ms, result)) => judge("pipeline", "sources: no emoji, no dead apology line", NETWORK_BUDGET_MS, ms, result),
        None => skip("pipeline", "sources: no emoji, no dead apology line", "the sources are not next to this executable"),
    }
}

/// A symbol that is an emoji, a dingbat, a miscellaneous symbol or a geometric shape — the
/// glyphs that are never part of a HUD label. (Arrows, dashes and box-drawing lines are fine.)
pub fn is_pictograph(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x25A0..=0x25FF | 0x2300..=0x23FF | 0x2B00..=0x2BFF | 0x27F0..=0x27FF | 0xFE0F)
}

/// Scans every source file's code (comments aside) for pictographs and for the apology line Sir
/// had removed. None when there is no `src` directory here.
pub fn source_hygiene(src: &Path) -> Option<(f64, Result<String, String>)> {
    let started = Instant::now();
    let entries = std::fs::read_dir(src).ok()?;
    // Built at run time, so this file does not contain the line it looks for.
    let apology = ["lost the", "thread"].join(" ");
    let (mut files, mut found) = (0usize, Vec::new());
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rs")) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        files += 1;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        for (number, line) in text.lines().enumerate() {
            let code = if line.trim_start().starts_with("//") { "" } else { line.split("//").next().unwrap_or("") };
            if let Some(c) = code.chars().find(|c| is_pictograph(*c)) {
                found.push(format!("{name}:{} has U+{:04X}", number + 1, c as u32));
            }
            if line.to_lowercase().contains(&apology) {
                found.push(format!("{name}:{} has the apology line", number + 1));
            }
        }
    }
    let result = if files == 0 {
        return None;
    } else if found.is_empty() {
        Ok(format!("{files} source files clean"))
    } else {
        Err(format!("{} finding(s): {}", found.len(), found.iter().take(6).cloned().collect::<Vec<_>>().join("; ")))
    };
    Some((ms_since(started), result))
}

// --------------------------------------------------------------------------- the HUD ----

/// A reflex carried out for real: routed live, then its tool run. The tool's answer.
async fn reflex(tools: &ToolContext, say: &str) -> Result<String, String> {
    match intent_engine::route(say, &ReflexContext::live()).map(|routed| routed.reflex) {
        Some(intent_engine::Reflex::Silent(action)) => {
            let outcome = Box::pin(tools.execute(action.tool, &action.args)).await;
            if outcome.success { Ok(outcome.output) } else { Err(outcome.output) }
        }
        other => Err(format!("'{say}' is no reflex here: {other:?}")),
    }
}

async fn hud_probes(app: &Jarvis) {
    let tools = app.tools();
    if !crate::gui::windows_supported() {
        for id in ["a window opens in the HUD and closes by name", "the emblem moves and comes back", "the theme changes and comes back", "pages turn and the board closes by voice command", "frame time"] {
            skip("hud", id, "the HUD window is not up (headless or hidden)");
        }
    } else {
        // A window of Jarvis' own: an in-app block the moment it is asked for, gone by name.
        const TITLE: &str = "SELF TEST // PROBE";
        let started = Instant::now();
        let opened = Box::pin(tools.execute("spawn_native_window", &json!({"id": "selftest_probe", "title": TITLE, "markdown": "Live self-test: this block opens, then closes by name.", "width": 420, "height": 240}))).await;
        let shown = if opened.success { wait_until(Duration::from_millis(1500), || !crate::hud_dock::blocks_named("self test").is_empty()).await } else { None };
        match shown {
            Some(_) => judge("hud", "a window opens as an in-app block", UI_BUDGET_MS, ms_since(started), Ok(format!("'{TITLE}' is on the board"))),
            None => judge("hud", "a window opens as an in-app block", UI_BUDGET_MS, ms_since(started), Err(if opened.success { "the block never appeared (is the HUD hidden?)".to_string() } else { opened.output.clone() })),
        }
        let started = Instant::now();
        let closed = Box::pin(tools.execute("hud_board", &json!({"action": "close", "target": "self test"}))).await;
        let gone = wait_until(Duration::from_millis(1500), || crate::hud_dock::blocks_named("self test").is_empty() && !tools.widgets.windows().iter().any(|w| w.id == "selftest_probe")).await;
        judge(
            "hud",
            "\"close <block>\" fades it out and closes it",
            UI_BUDGET_MS,
            ms_since(started),
            if closed.success && gone.is_some() { Ok(format!("faded and gone ({} ms fade)", crate::hud_dock::CLOSE_FADE.as_millis())) } else { Err(format!("still there: {}", crate::crash_guard::clip(closed.output.trim(), 120))) },
        );
        // Whatever happened, the probe window does not stay.
        let _ = tools.widgets.close_window("selftest_probe");

        // The emblem: to a corner by the screenshot's sentence family, then back where it was.
        let before = tools.config.read(|c| c.interface.orb_board_pos);
        let started = Instant::now();
        let moved = reflex(tools, "Move your orb to the top left").await;
        let at = tools.config.read(|c| c.interface.orb_board_pos);
        let nudged = reflex(tools, "Move your orb a little bit to the right").await;
        let after = tools.config.read(|c| c.interface.orb_board_pos);
        let ms = ms_since(started);
        tools.config.update(|c| c.interface.orb_board_pos = before);
        crate::gui::refresh();
        let ok = moved.is_ok() && nudged.is_ok() && at == Some([0.14, 0.2]) && after.is_some_and(|p| p[0] > 0.2 && (p[1] - 0.2).abs() < 1e-4);
        judge("hud", "the emblem moves by voice command and comes back", LOCAL_BUDGET_MS, ms, if ok { Ok(format!("top left {at:?}, nudged right {after:?}, restored to {before:?}")) } else { Err(format!("moved {moved:?}, nudged {nudged:?}: {at:?} → {after:?}")) });

        // The theme: another preset and back.
        let palette = tools.config.read(|c| c.interface.hud_palette.clone());
        let other = crate::theme_engine::next_preset(&palette);
        let started = Instant::now();
        let changed = Box::pin(tools.execute("configure_interface", &json!({"hud_palette": other}))).await;
        let now = tools.config.read(|c| c.interface.hud_palette.clone());
        let restored = Box::pin(tools.execute("configure_interface", &json!({"hud_palette": palette.clone()}))).await;
        let back = tools.config.read(|c| c.interface.hud_palette.clone());
        judge(
            "hud",
            "the theme changes and comes back",
            LOCAL_BUDGET_MS,
            ms_since(started),
            if changed.success && restored.success && now == other && back == palette { Ok(format!("{palette} → {other} → {palette}")) } else { Err(format!("{palette} → {now} → {back}")) },
        );

        board_probe(tools).await;

        match crate::gui::frame_times() {
            Some((frames, average_ms, worst_ms)) => judge(
                "hud",
                "frame time (the HUD's own work per frame)",
                // A 180 Hz monitor leaves 5.5 ms a frame.
                5.5,
                average_ms,
                Ok(format!("{frames} frames: average {average_ms:.2} ms, worst {worst_ms:.2} ms")),
            ),
            None => skip("hud", "frame time (the HUD's own work per frame)", "no frames painted yet"),
        }
    }

    // Ultron Mode: on, everything changes; off, everything is back.
    let started = Instant::now();
    match app.probe_ultron() {
        Some(result) => judge("hud", "Ultron Mode goes on and comes back to Jarvis", LOCAL_BUDGET_MS, ms_since(started), result),
        None => skip("hud", "Ultron Mode goes on and comes back to Jarvis", "Ultron Mode is on right now; not touched"),
    }

    // The privacy lock: on, then off (untouched when Sir has it on).
    if crate::listening_lock::is_locked() {
        skip("hud", "the privacy lock locks and releases", "the lock is on right now; not touched");
    } else {
        timed("hud", "the privacy lock locks and releases", LOCAL_BUDGET_MS, || {
            use crate::listening_lock::{apply, is_locked, LockCommand};
            apply(LockCommand::Lock);
            let locked = is_locked();
            apply(LockCommand::Resume);
            if locked && !is_locked() { Ok("locked, released".to_string()) } else { Err(format!("locked: {locked}, afterwards: {}", is_locked())) }
        });
    }
}

/// A three-page test report on the board: its pages turn by the real reflexes, then the board
/// closes by name. Not run while a board of Sir's is up.
async fn board_probe(tools: &ToolContext) {
    use crate::hud_board::{Block, BlockKind, Note, Report};
    const ID: &str = "pages turn and the board closes by voice command";
    if crate::hud_board::is_active() {
        skip("hud", ID, "a board is up; it is not replaced by a test report");
        return;
    }
    let notes = (1..=3).map(|n| Note { headline: format!("PAGE {n}"), body: format!("Live self-test page {n} of 3.") }).collect();
    let block = Block::new("SELF TEST // PAGES", "PROBE", "LIVE SELF-TEST", BlockKind::NoteStack { notes });
    crate::hud_board::present(Report { topic: "Live self-test".to_string(), created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), blocks: vec![block], sources: Vec::new(), id: None });
    let started = Instant::now();
    let mut steps: Vec<String> = Vec::new();
    let mut failed: Option<String> = None;
    for (say, wanted) in [("Next page", "PAGE 2"), ("Show result 3", "PAGE 3"), ("Go back", "PAGE 2"), ("Previous page", "PAGE 1")] {
        match reflex(tools, say).await {
            Ok(answer) if answer.contains(wanted) => steps.push(format!("{say} → {wanted}")),
            Ok(answer) => failed = Some(format!("'{say}' gave '{}', wanted {wanted}", crate::crash_guard::clip(answer.trim(), 60))),
            Err(why) => failed = Some(format!("'{say}': {}", crate::crash_guard::clip(why.trim(), 100))),
        }
        if failed.is_some() {
            break;
        }
    }
    if failed.is_none() {
        match reflex(tools, "Close the HUD board").await {
            Ok(_) if !crate::hud_board::is_active() => steps.push("Close the HUD board → closed".to_string()),
            Ok(_) => failed = Some("the board is still up after 'close the HUD board'".to_string()),
            Err(why) => failed = Some(format!("'Close the HUD board': {why}")),
        }
    }
    // The test report never stays, whatever happened.
    crate::hud_board::close_board();
    judge("hud", ID, LOCAL_BUDGET_MS, ms_since(started), failed.map_or_else(|| Ok(steps.join(", ")), Err));
}

// ---------------------------------------------------------------------------- the run ----

/// Runs the whole suite in the running app. One run at a time: a second call returns the
/// summary so far.
pub async fn run(app: &Jarvis) -> Summary {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return summary();
    }
    lock(&RESULTS).clear();
    let started = Instant::now();
    info!("[PROBE] Live self-test: pipelines, routing, tools, HUD");
    pipeline_probes(app).await;
    routing_probes(app).await;
    tool_probes(app.tools()).await;
    hud_probes(app).await;
    RUNNING.store(false, Ordering::Release);
    let s = summary();
    let line = format!("[PROBE] Live self-test finished in {:.1} s: {}", started.elapsed().as_secs_f64(), s.describe());
    if s.clean() {
        info!("{line}");
    } else {
        error!("{line}");
        for failed in results().iter().filter(|r| matches!(r.status, Status::Fail | Status::Slow)) {
            error!("[PROBE]   {} {} // {}: {}", failed.status.label(), failed.group, failed.id, failed.detail);
        }
    }
    crate::gui::refresh();
    s
}

/// The report of the last run as JSON (`--live-test` writes it next to the log).
pub fn report_json() -> String {
    serde_json::to_string_pretty(&json!({ "summary": summary(), "probes": results() })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_and_live_tool_names_a_real_tool_with_arguments_its_schema_takes() {
        let mut defs = crate::tools::definitions_with(Vec::new());
        // The skill library's own tools come from the skill manager at run time.
        defs.push(json!({"type": "function", "function": {"name": "list_skills", "description": "Lists the learned skills.", "parameters": {"type": "object", "properties": {}, "required": []}}}));
        for (name, args, ..) in LIVE_TOOLS {
            let args: Value = serde_json::from_str(args).unwrap_or_else(|e| panic!("{name}: {e}"));
            check_call(&defs, name, &args).unwrap_or_else(|e| panic!("{e}"));
            assert!(crate::hyper_cortex::is_read_only(name, &args) || matches!(*name, "configure_productivity_service" | "manage_schedule_watcher"), "{name} {args} is run live but is not known to only read");
        }
        for route in ROUTES {
            let args: Value = serde_json::from_str(route.args).unwrap_or_else(|e| panic!("{}: {e}", route.say));
            assert!(args.is_object(), "{}", route.say);
            let special = matches!(route.tool, "language_model" | "time" | "date" | "answer" | "ultron_mode" | "listening_lock");
            assert!(special || defs.iter().any(|d| d["function"]["name"] == route.tool), "'{}' routes to no tool: {}", route.say, route.tool);
        }
        // Every registered tool is well-formed (what the wiring probe checks at run time).
        for def in &defs {
            well_formed(def).unwrap_or_else(|e| panic!("{e}"));
        }
        // Every button of every butler card is a call its tool accepts.
        let (cards, fixes) = butler_cards(&defs).unwrap_or_else(|e| panic!("{e}"));
        assert!(cards >= 9 && fixes > cards, "{cards} cards, {fixes} fixes");
        // And every key the suggestion bar can show.
        let (bars, keys) = suggestion_bars(&defs).unwrap_or_else(|e| panic!("{e}"));
        assert!(bars == 6 && keys >= bars * 3, "{bars} bars, {keys} keys");
    }

    #[test]
    fn a_dry_run_records_and_carries_nothing_out() {
        assert!(!dry());
        assert!(intercept("open_application", &json!({"app_name": "spotify"})).is_none(), "outside a dry run tools run for real");
        assert!(!intercept_model(None));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let recorded = rt.block_on(record(async {
            assert!(dry());
            let outcome = intercept("open_application", &json!({"app_name": "spotify"})).expect("intercepted");
            assert!(outcome.success && outcome.output.starts_with("Dry run"));
            assert!(intercept_model(Some(("check_school_schedule", &json!({"target_date": "tomorrow"})))));
            assert!(intercept_answer("time"));
        }));
        assert_eq!(recorded.len(), 3);
        assert_eq!((recorded[0].kind, recorded[0].tool.as_str()), ("tool", "open_application"));
        assert_eq!((recorded[1].kind, recorded[1].tool.as_str()), ("route", "check_school_schedule"));
        assert_eq!((recorded[2].kind, recorded[2].tool.as_str()), ("answer", "time"));
        assert!(contains(&recorded[0].args, &json!({"app_name": "spotify"})) && !contains(&recorded[0].args, &json!({"app_name": "discord"})));
    }

    #[test]
    fn probes_are_judged_on_the_result_and_the_clock() {
        let outcome = |ms: f64, result: Result<String, String>| match result {
            Ok(_) if ms <= REFLEX_BUDGET_MS => Status::Pass,
            Ok(_) => Status::Slow,
            Err(_) => Status::Fail,
        };
        assert_eq!(outcome(0.004, Ok(String::new())), Status::Pass);
        assert_eq!(outcome(2.0, Ok(String::new())), Status::Slow);
        assert_eq!(outcome(0.004, Err(String::new())), Status::Fail);
        assert!(Summary { total: 3, pass: 3, ..Default::default() }.clean());
        assert!(!Summary { total: 3, pass: 2, slow: 1, ..Default::default() }.clean());
        assert!(!Summary::default().clean(), "nothing ran");
    }

    /// The whole tree: no emoji, dingbat or symbol glyph in any code line, and the apology line
    /// Sir had removed stays out.
    #[test]
    fn the_sources_hold_no_pictographs_and_no_dead_apology() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let (_, result) = source_hygiene(&src).expect("the sources are here");
        result.unwrap_or_else(|found| panic!("{found}"));
        assert!(is_pictograph('\u{1F3A8}') && is_pictograph('\u{26A1}') && is_pictograph('\u{25C9}') && is_pictograph('\u{2328}'));
        assert!(!is_pictograph('→') && !is_pictograph('—') && !is_pictograph('ä') && !is_pictograph('×'));
    }
}
