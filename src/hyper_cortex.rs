//! The hyper cortex: the part of JAI that works ahead of the request and beside the language
//! model.
//!
//! * **Tiers** (`tier`): Reflex (his own controls and exact commands, decided in Rust in
//!   microseconds), Tactical (an everyday question: the model without hidden reasoning) and Deep
//!   (maths, code, comparisons: full reasoning, and the figures it states are checked).
//! * **Speculation** (`anticipate`, `speculate`, `execute`): a transcript is routed the moment
//!   it exists — also the transcript of a pause, heard before Sir has finished. When it asks for
//!   something that only reads (the timetable, the calendar, the weather, unread mail), the fetch
//!   starts at once, and the request that follows takes the result from memory.
//! * **Side by side** (`run_parallel`): tool calls that only read run concurrently in a
//!   `JoinSet` and come back in the order they were asked for.
//! * **Context ahead of time** (`prefetch_context`, `take_prefetched`): the desktop and the media
//!   session are read when Sir starts speaking, not after he has finished.
//! * **Self-healing** (`execute`): a read that failed on the network is tried once more; a
//!   window or program action aimed at one of JAI's own blocks goes to the block instead of
//!   Windows ("Hud Board doesn't appear to be running" never reaches Sir).
//! * **Checked figures** (`FigureCheck`): arithmetic the model states in a reply
//!   ("12 × 8 = 86") is recomputed while it streams, and a wrong result is corrected before Sir
//!   hears it.

use crate::crash_guard::lock;
use crate::intent_engine::{self, ReasoningGear, ReflexContext};
use crate::tools::{ToolContext, ToolOutcome};
use regex::Regex;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------- tiers ----

/// How much of JAI a request needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// One of his own controls or an exact command: decided in Rust, no language model.
    Reflex,
    /// An everyday question or request: the model, no hidden reasoning, a short reply.
    Tactical,
    /// Maths, physics, code, a comparison, long writing: full reasoning, checked figures.
    Deep,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reflex => "reflex",
            Self::Tactical => "tactical",
            Self::Deep => "deep",
        }
    }
}

/// The tier of a request, decided without the model: a reflex when any deterministic parser
/// takes it, else by the reasoning gear.
pub fn tier(text: &str, ctx: &ReflexContext) -> Tier {
    if intent_engine::route(text, ctx).is_some() || crate::intents::silent_quick_command(text).is_some() || crate::intents::parse(text).is_some() {
        return Tier::Reflex;
    }
    match intent_engine::reasoning_gear(text) {
        ReasoningGear::Reflex => Tier::Tactical,
        ReasoningGear::Deep => Tier::Deep,
    }
}

// ------------------------------------------------------------------- what only reads ----

/// Tools that only read, with the actions that do (empty: every call). Nothing here changes a
/// file, a setting, the volume, a window or anybody's inbox, so such a call may run ahead of
/// time, beside another one, or twice.
const READ_ONLY: &[(&str, &[&str])] = &[
    ("recall_memories", &[]),
    ("search_conversation", &[]),
    ("get_system_status", &[]),
    ("get_current_datetime", &[]),
    ("web_search", &[]),
    ("read_webpage", &[]),
    ("calculate", &[]),
    ("search_inside_files", &[]),
    ("read_text_file", &[]),
    ("network_status", &[]),
    ("top_processes", &[]),
    ("check_unread_emails", &[]),
    ("search_emails", &[]),
    ("get_calendar_schedule", &[]),
    ("check_school_schedule", &[]),
    ("query_connected_website", &[]),
    ("get_precise_location_weather", &[]),
    ("check_ride_and_commute_status", &[]),
    ("list_jai_features", &[]),
    ("recent_actions", &[]),
    ("whats_new", &[]),
    ("whats_open", &[]),
    ("timer_left", &[]),
    ("is_app_running", &[]),
    ("list_skills", &[]),
    ("list_forged_tools", &[]),
    ("inspect_own_source", &[]),
    ("get_remote_pc_status", &[]),
    ("media_and_volume_control", &["get_volume"]),
    ("control_audio_mixer", &["list", "list_mixes", "get_master", "get_app", "muted", "device", "devices", "switch_device"]),
    ("control_orb_mode", &["status"]),
    ("control_music", &["what_is_playing", "list_playlists", "queue_show"]),
    ("reminders", &["list"]),
    ("manage_vram", &["status", "game_status"]),
    ("manage_deep_research", &["status"]),
    ("discord_call", &["status", "recap"]),
    ("discord_bot", &["status", "read", "recap", "summary", "who"]),
    ("away_mode", &["status", "digest"]),
    ("watch_and_tell", &["list"]),
    ("hud_board", &["status"]),
    ("manage_stark_protocol", &["list"]),
    ("sync_study_materials_and_deadlines", &["list"]),
    ("manage_spatial_scene", &["list", "monitors"]),
    ("manage_contacts", &["list"]),
    ("manage_voice_macro", &["list"]),
    ("manage_workout", &["status"]),
    ("manage_price_sniper", &["list"]),
    ("set_auto_responder", &["status"]),
    ("manage_location_settings", &["status"]),
    ("show_3d_model", &["measure", "parts"]),
    // live layers (C1): view "live"'s answers (passes, traffic, weather_on, rain_soon, alerts).
    ("open_jai_view", &crate::live_layers::READ_ACTIONS),
];

/// Reads whose answer is the same a moment later and that put nothing on screen: these may be
/// fetched before Sir has finished asking.
const SPECULABLE: &[&str] = &[
    "get_calendar_schedule", "check_school_schedule", "get_precise_location_weather", "check_ride_and_commute_status", "check_unread_emails", "search_emails", "web_search",
    "get_system_status", "network_status", "top_processes", "recall_memories",
];

/// Whether this call only reads.
pub fn is_read_only(name: &str, args: &Value) -> bool {
    READ_ONLY.iter().find(|(tool, _)| *tool == name).is_some_and(|(_, actions)| actions.is_empty() || args["action"].as_str().is_some_and(|a| actions.contains(&a)))
}

/// Every tool name of the read-only table (the self-test checks each one exists).
pub fn read_only_tools() -> impl Iterator<Item = &'static str> {
    READ_ONLY.iter().map(|(tool, _)| *tool)
}

fn speculable(name: &str, args: &Value) -> bool {
    SPECULABLE.contains(&name) && is_read_only(name, args)
}

// ------------------------------------------------------------------------ the tally ----

static SPECULATED: AtomicU64 = AtomicU64::new(0);
static SERVED: AtomicU64 = AtomicU64::new(0);
static HEALED: AtomicU64 = AtomicU64::new(0);
static BATCHES: AtomicU64 = AtomicU64::new(0);
static FIGURES: AtomicU64 = AtomicU64::new(0);

/// What the cortex has done since JAI started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CortexStats {
    /// Reads started before they were asked for.
    pub speculated: u64,
    /// Requests answered from such a read.
    pub served: u64,
    /// Failed calls that succeeded on a repaired second try.
    pub healed: u64,
    /// Groups of reads run side by side.
    pub parallel_batches: u64,
    /// Arithmetic results corrected in a reply.
    pub figures_corrected: u64,
}

pub fn stats() -> CortexStats {
    CortexStats {
        speculated: SPECULATED.load(Ordering::Relaxed),
        served: SERVED.load(Ordering::Relaxed),
        healed: HEALED.load(Ordering::Relaxed),
        parallel_batches: BATCHES.load(Ordering::Relaxed),
        figures_corrected: FIGURES.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------- speculation ----

/// A read started ahead of its request.
struct Pending {
    key: String,
    started: Instant,
    result: watch::Receiver<Option<ToolOutcome>>,
}

static PENDING: LazyLock<Mutex<Vec<Pending>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// A speculative read nobody asked for within this long is forgotten.
const SPECULATION_TTL: Duration = Duration::from_secs(10);
/// The longest a request waits for its speculative read before fetching itself.
const SPECULATION_WAIT: Duration = Duration::from_secs(25);

fn call_key(name: &str, args: &Value) -> String {
    // serde_json keeps object keys sorted, so equal arguments give equal keys.
    format!("{name} {args}")
}

/// Starts a read ahead of the request that will ask for it. False when the call is not one
/// that may run early, or the same call is already under way. Needs a tokio runtime.
pub fn speculate(tools: &ToolContext, name: &str, args: &Value) -> bool {
    if !speculable(name, args) {
        return false;
    }
    let key = call_key(name, args);
    let (tx, rx) = watch::channel(None);
    {
        let mut pending = lock(&PENDING);
        pending.retain(|p| p.started.elapsed() < SPECULATION_TTL);
        if pending.iter().any(|p| p.key == key) {
            return false;
        }
        pending.push(Pending { key, started: Instant::now(), result: rx });
    }
    let (tools, name, args) = (tools.clone(), name.to_string(), args.clone());
    tokio::spawn(async move {
        let outcome = Box::pin(tools.execute(&name, &args)).await;
        let _ = tx.send(Some(outcome));
    });
    SPECULATED.fetch_add(1, Ordering::Relaxed);
    true
}

/// The result of a speculative read of exactly this call, if one is under way or fresh (waits
/// for it to finish). A failed read is not served: the request fetches again.
async fn speculated(name: &str, args: &Value) -> Option<ToolOutcome> {
    let key = call_key(name, args);
    let mut result = {
        let mut pending = lock(&PENDING);
        pending.retain(|p| p.started.elapsed() < SPECULATION_TTL);
        let at = pending.iter().position(|p| p.key == key)?;
        pending.remove(at).result
    };
    let outcome = match tokio::time::timeout(SPECULATION_WAIT, result.wait_for(|o| o.is_some())).await {
        Ok(Ok(found)) => (*found).clone(),
        _ => None,
    }?;
    outcome.success.then_some(outcome)
}

/// What a transcript lets JAI do before the request reaches the brain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anticipation {
    /// The vocabularies it touches (`intent_engine::domain`).
    pub domains: u32,
    pub tier: Tier,
    /// The read that was started ahead, if any.
    pub fetching: Option<&'static str>,
}

/// Routes a transcript the moment it exists (also one that is not final yet) and starts the
/// read it asks for. Nothing with an effect ever runs here.
pub fn anticipate(tools: &ToolContext, heard: &str) -> Anticipation {
    let started = Instant::now();
    let domains = intent_engine::domains(&intent_engine::canonical(heard));
    let tier = tier(heard, &ReflexContext::live());
    let route = crate::features::route_text(heard).or_else(|| crate::integration_router::route_text(heard));
    let fetching = route.and_then(|r| speculate(tools, r.tool, &r.args).then_some(r.tool));
    let anticipation = Anticipation { domains, tier, fetching };
    debug!(
        "[CORTEX] '{}' → {} ({}){} in {} µs",
        crate::crash_guard::clip(heard, 80),
        tier.label(),
        intent_engine::domain::describe(domains),
        fetching.map(|t| format!(", fetching {t} ahead")).unwrap_or_default(),
        started.elapsed().as_micros()
    );
    anticipation
}

// ------------------------------------------------------------------- self-healing ----

/// What to try when a call failed.
#[derive(Debug, Clone, PartialEq)]
enum Repair {
    /// The same call once more, after a short wait.
    Again(Duration),
    /// Another call that does what was meant.
    Instead(&'static str, Value),
}

/// Words of an error that passes by itself.
const TRANSIENT: &[&str] = &["timed out", "timeout", "connection", "dns error", "temporarily", " 502", " 503", " 504", "reset by peer", "unreachable", "network error", "error sending request"];

/// The repair for a failed call, if there is an obvious one.
fn repair(name: &str, args: &Value, error: &str) -> Option<Repair> {
    // One of JAI's own blocks named as a window or a program ("close HUD Board"): the block.
    let closing = match name {
        "close_application" | "close_native_window" => true,
        "window_management" => args["action"] == "close",
        _ => false,
    };
    if closing {
        let target = ["app_name", "target_window", "window_id_or_title", "target", "id"].iter().find_map(|k| args[*k].as_str()).unwrap_or("");
        if let Some(action) = intent_engine::close_command(&format!("close {target}")) {
            return Some(Repair::Instead(action.tool, action.args));
        }
    }
    let lower = error.to_lowercase();
    (is_read_only(name, args) && TRANSIENT.iter().any(|t| lower.contains(t))).then_some(Repair::Again(Duration::from_millis(400)))
}

/// Runs a tool the way the brain wants it run: from a speculative read when one is there, and
/// with one repaired second try when the first fails for a reason that has an obvious cure.
pub async fn execute(tools: &ToolContext, name: &str, args: &Value) -> ToolOutcome {
    if let Some(hit) = speculated(name, args).await {
        SERVED.fetch_add(1, Ordering::Relaxed);
        info!("[CORTEX] {name}: answered from the read started ahead ({} ms of work already done)", hit.duration_ms);
        return hit;
    }
    let first = Box::pin(tools.execute(name, args)).await;
    if first.success {
        return first;
    }
    let second = match repair(name, args, &first.output) {
        Some(Repair::Again(wait)) => {
            tokio::time::sleep(wait).await;
            Box::pin(tools.execute(name, args)).await
        }
        Some(Repair::Instead(tool, new_args)) => Box::pin(tools.execute(tool, &new_args)).await,
        None => return first,
    };
    if second.success {
        HEALED.fetch_add(1, Ordering::Relaxed);
        info!("[CORTEX] {name} failed ({}); healed with {} {}", crate::crash_guard::clip(first.output.trim(), 100), second.tool, second.args);
        return second;
    }
    first
}

// ----------------------------------------------------------------------- side by side ----

/// Whether these calls may run concurrently: more than one, and every one only reads.
pub fn parallel_safe(calls: &[(String, Value)]) -> bool {
    calls.len() > 1 && calls.iter().all(|(name, args)| is_read_only(name, args))
}

/// Runs the calls concurrently (each through `execute`) and returns their outcomes in the
/// order of the calls.
pub async fn run_parallel(tools: &ToolContext, calls: Vec<(String, Value)>) -> Vec<ToolOutcome> {
    let started = Instant::now();
    let mut set = JoinSet::new();
    for (index, (name, args)) in calls.iter().cloned().enumerate() {
        let tools = tools.clone();
        set.spawn(crate::live_self_tester::carry(async move { (index, execute(&tools, &name, &args).await) }));
    }
    let mut outcomes: Vec<Option<ToolOutcome>> = calls.iter().map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, outcome)) => outcomes[index] = Some(outcome),
            Err(e) => warn!("[CORTEX] A parallel tool task stopped: {e}"),
        }
    }
    BATCHES.fetch_add(1, Ordering::Relaxed);
    let slowest = outcomes.iter().flatten().map(|o| o.duration_ms).max().unwrap_or(0);
    let total: u64 = outcomes.iter().flatten().map(|o| o.duration_ms).sum();
    info!("[CORTEX] {} reads side by side in {} ms (slowest {slowest} ms; one after another: {total} ms)", calls.len(), started.elapsed().as_millis());
    outcomes
        .into_iter()
        .zip(calls)
        .map(|(outcome, (tool, args))| {
            outcome.unwrap_or(ToolOutcome {
                tool,
                args,
                output: "Error: the tool stopped unexpectedly.".to_string(),
                success: false,
                duration_ms: 0,
                memories_changed: false,
                skills_changed: false,
                shutdown_requested: false,
                modality_changed: false,
                state_changed: false,
            })
        })
        .collect()
}

/// One lane of a protocol: steps that keep their order, as (step index, tool, arguments).
pub type Lane = Vec<(usize, String, Value)>;

/// Runs lanes of tool calls side by side, each lane's own calls in order, and returns the
/// outcomes with their step index (sorted by it). Boxed, because the caller is itself a tool
/// (a protocol running its steps): its future would otherwise have to contain itself.
pub fn run_lanes(tools: ToolContext, lanes: Vec<Lane>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<(usize, ToolOutcome)>> + Send>> {
    Box::pin(async move {
        let mut set = JoinSet::new();
        for lane in lanes {
            let tools = tools.clone();
            set.spawn(crate::live_self_tester::carry(async move {
                let mut done = Vec::with_capacity(lane.len());
                for (index, name, args) in lane {
                    done.push((index, tools.execute(&name, &args).await));
                }
                done
            }));
        }
        let mut outcomes = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(done) => outcomes.extend(done),
                Err(e) => warn!("[CORTEX] A protocol lane stopped: {e}"),
            }
        }
        BATCHES.fetch_add(1, Ordering::Relaxed);
        outcomes.sort_by_key(|(index, _)| *index);
        outcomes
    })
}

// ----------------------------------------------------------------- context ahead of time ----

/// The slow parts of the live context, read while Sir is still speaking.
pub struct Prefetched {
    pub desktop: crate::context::Desktop,
    /// The media session, described ("Back in Black — AC/DC (Sonora, playing)").
    pub music: Option<String>,
}

static CONTEXT: Mutex<Option<(Instant, Prefetched)>> = Mutex::new(None);
static LAST_PREFETCH_MS: AtomicU64 = AtomicU64::new(0);
/// A read-ahead older than this is stale (a long sentence plus its transcription fits inside).
const CONTEXT_TTL: Duration = Duration::from_secs(12);

/// Sir started speaking: read the desktop and the media session now (at most once a second).
pub fn prefetch_context() {
    let now = crate::ears::now_ms();
    if now.saturating_sub(LAST_PREFETCH_MS.swap(now, Ordering::AcqRel)) < 1000 {
        return;
    }
    tokio::spawn(async {
        let started = Instant::now();
        let (desktop, playing) = tokio::join!(crate::context::desktop(), tokio::time::timeout(Duration::from_millis(600), crate::music::now_playing()));
        let music = playing.ok().flatten().map(|np| np.describe());
        *lock(&CONTEXT) = Some((Instant::now(), Prefetched { desktop, music }));
        debug!("[CORTEX] Context read ahead in {} ms", started.elapsed().as_millis());
    });
}

/// The read-ahead of this turn, if there is a fresh one (taken: the next turn reads again).
pub fn take_prefetched() -> Option<Prefetched> {
    lock(&CONTEXT).take().filter(|(at, _)| at.elapsed() < CONTEXT_TTL).map(|(_, prefetched)| prefetched)
}

// ------------------------------------------------------------------- checked figures ----

/// "12 × 8 = 86", "3.5 + 1.25 = 4.75", "100 / 8 = 12.5".
static EQUATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(-?\d+(?:\.\d+)?)\s*([+\-−–*×·/÷])\s*(-?\d+(?:\.\d+)?)\s*=\s*(-?\d+(?:\.\d+)?)").expect("valid equation pattern"));
/// The end of a text that may still become an equation: a number, then perhaps an operator,
/// more numbers and "=" (group 1 starts at the first number).
static OPEN_EQUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w.,])(-?\d+(?:\.\d*)?(?:\s*[+\-−–*×·/÷]\s*(?:-?\d+(?:\.\d*)?)?)*(?:\s*=\s*(?:-?\d*(?:\.\d*)?)?)?\s*)$").expect("valid open-equation pattern"));

/// A number as the model would write it: whole when it is, else up to `decimals` places without
/// trailing zeros.
fn written(value: f64, decimals: usize) -> String {
    if (value - value.round()).abs() < 1e-9 {
        return format!("{}", value.round() as i64);
    }
    let text = format!("{value:.*}", decimals.clamp(2, 6));
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Recomputes every plain equation in `text` ("a op b = c") and corrects a wrong result.
/// Returns the text and how many results were corrected. Longer expressions, percentages and
/// numbers with thousands separators are left alone.
pub fn check_figures(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut corrected) = (0usize, 0usize);
    for found in EQUATION.captures_iter(text) {
        let (Some(whole), Some(stated)) = (found.get(0), found.get(4)) else { continue };
        // Part of a longer number ("1,000 + 1") or of a longer expression ("2 + 3 × 4 = 14"): not
        // judged.
        let direct = text[..whole.start()].chars().next_back();
        let spaced = text[..whole.start()].trim_end().chars().next_back();
        let mut after = text[whole.end()..].chars();
        let (next, then) = (after.next(), after.next());
        let joined_before = direct.is_some_and(|c| c.is_ascii_digit() || c == '.' || c == ',') || spaced.is_some_and(|c| "+-−–*×·/÷^(=".contains(c));
        let joined_after = match next {
            Some(c) if c.is_ascii_digit() || c == '%' => true,
            Some('.' | ',') => then.is_some_and(|c| c.is_ascii_digit()),
            _ => text[whole.end()..].trim_start().starts_with(['+', '-', '−', '–', '*', '×', '·', '/', '÷', '^', '=']),
        };
        if joined_before || joined_after {
            continue;
        }
        let (Ok(a), Ok(b), Ok(c)) = (found[1].parse::<f64>(), found[3].parse::<f64>(), found[4].parse::<f64>()) else { continue };
        let expected = match &found[2] {
            "+" => a + b,
            "-" | "−" | "–" => a - b,
            "*" | "×" | "·" => a * b,
            _ if b == 0.0 => continue,
            _ => a / b,
        };
        // Right to the places the model wrote ("10 / 3 = 3.33" is fine).
        let decimals = stated.as_str().split_once('.').map_or(0, |(_, d)| d.len());
        if (expected - c).abs() <= 0.5 * 10f64.powi(-(decimals as i32)) + 1e-9 {
            continue;
        }
        let fixed = written(expected, decimals);
        info!("[VERIFY] '{}' is wrong: corrected to {fixed}", whole.as_str());
        out.push_str(&text[copied..stated.start()]);
        out.push_str(&fixed);
        copied = stated.end();
        corrected += 1;
    }
    out.push_str(&text[copied..]);
    (out, corrected)
}

/// The figure check on a streaming reply: text passes straight through, except a trailing
/// number that may be the start of an equation, which waits until the equation is complete
/// (or turns out to be none).
#[derive(Debug, Default)]
pub struct FigureCheck {
    held: String,
}

impl FigureCheck {
    pub fn new() -> Self {
        Self::default()
    }

    fn release(&mut self, upto: usize) -> String {
        let ready: String = self.held.drain(..upto).collect();
        let (checked, corrected) = check_figures(&ready);
        FIGURES.fetch_add(corrected as u64, Ordering::Relaxed);
        checked
    }

    pub fn push(&mut self, text: &str) -> String {
        self.held.push_str(text);
        let open = OPEN_EQUATION.captures(&self.held).and_then(|c| c.get(1)).map_or(self.held.len(), |m| m.start());
        // A long run of figures (a table row, a serial number) is not held back.
        let upto = if self.held.len() - open > 96 { self.held.len() } else { open };
        self.release(upto)
    }

    /// The end of the reply: whatever was still waiting.
    pub fn flush(&mut self) -> String {
        let all = self.held.len();
        self.release(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_reads_may_run_early_or_side_by_side() {
        assert!(is_read_only("check_school_schedule", &json!({"target_date": "tomorrow"})));
        assert!(is_read_only("control_music", &json!({"action": "what_is_playing"})));
        assert!(!is_read_only("control_music", &json!({"action": "play", "song_name": "Thunderstruck"})));
        assert!(!is_read_only("control_music", &json!({})), "no action named: not known to be a read");
        assert!(!is_read_only("send_whatsapp_message", &json!({})));
        assert!(!is_read_only("close_application", &json!({"app_name": "spotify"})));
        assert!(speculable("get_calendar_schedule", &json!({"date_or_range": "today"})));
        assert!(!speculable("read_webpage", &json!({"url": "https://example.com"})), "a page is only fetched when asked");
        assert!(!speculable("control_music", &json!({"action": "what_is_playing"})));
        let reads = |calls: &[(&str, Value)]| parallel_safe(&calls.iter().map(|(n, a)| (n.to_string(), a.clone())).collect::<Vec<_>>());
        assert!(reads(&[("check_school_schedule", json!({})), ("check_unread_emails", json!({})), ("get_precise_location_weather", json!({}))]));
        assert!(!reads(&[("check_school_schedule", json!({}))]), "one call needs no JoinSet");
        assert!(!reads(&[("check_school_schedule", json!({})), ("open_application", json!({"app_name": "spotify"}))]), "anything with an effect keeps its order");
        // Every tool in the tables is a real tool (the skill library's own tools come from the
        // skill manager at run time).
        let mut known: Vec<String> = crate::tools::definitions_with(Vec::new()).iter().filter_map(|d| d["function"]["name"].as_str().map(String::from)).collect();
        known.push("list_skills".to_string());
        for tool in read_only_tools().chain(SPECULABLE.iter().copied()) {
            assert!(known.iter().any(|k| k == tool), "'{tool}' is not a tool");
        }
        assert_eq!(call_key("t", &json!({"b": 1, "a": 2})), call_key("t", &json!({"a": 2, "b": 1})), "argument order does not matter");
    }

    #[test]
    fn failures_with_an_obvious_cure_are_repaired() {
        // jai.log 2026-11-03 23:05:30: "Hud Board doesn't appear to be running."
        assert_eq!(
            repair("close_application", &json!({"app_name": "hud board", "force": true}), "Hud Board doesn't appear to be running."),
            Some(Repair::Instead("hud_board", json!({"action": "close"})))
        );
        assert_eq!(
            repair("close_application", &json!({"app_name": "to web search", "force": true}), "To Web Search doesn't appear to be running."),
            Some(Repair::Instead("hud_board", json!({"action": "close"})))
        );
        assert_eq!(
            repair("window_management", &json!({"action": "close", "target_window": "feature lab"}), "No open window matches 'Feature Lab'."),
            Some(Repair::Instead("hud_board", json!({"action": "close", "target": "feature lab"})))
        );
        // A real program that is not running stays an honest failure.
        assert_eq!(repair("close_application", &json!({"app_name": "spotify"}), "Spotify doesn't appear to be running."), None);
        assert_eq!(repair("window_management", &json!({"action": "snap_left", "target_window": "feature lab"}), "No open window matches 'Feature Lab'."), None);
        // A read that failed on the network: once more. A write is never repeated.
        assert_eq!(repair("check_school_schedule", &json!({}), "Error: WebUntis: error sending request (connection reset)"), Some(Repair::Again(Duration::from_millis(400))));
        assert_eq!(repair("web_search", &json!({"query": "x"}), "Error: operation timed out"), Some(Repair::Again(Duration::from_millis(400))));
        assert_eq!(repair("send_whatsapp_message", &json!({}), "Error: operation timed out"), None);
        assert_eq!(repair("check_school_schedule", &json!({}), "Error: wrong password"), None);
    }

    #[test]
    fn stated_arithmetic_is_recomputed_and_corrected() {
        assert_eq!(check_figures("So 12 × 8 = 86 in total."), ("So 12 × 8 = 96 in total.".to_string(), 1));
        assert_eq!(check_figures("That is 100 / 8 = 14, Sir."), ("That is 100 / 8 = 12.5, Sir.".to_string(), 1));
        assert_eq!(check_figures("3.5 + 1.25 = 5.75"), ("3.5 + 1.25 = 4.75".to_string(), 1));
        assert_eq!(check_figures("7 - 10 = 3 and 6 * 7 = 42."), ("7 - 10 = -3 and 6 * 7 = 42.".to_string(), 1));
        assert_eq!(check_figures("First, 15 * 4 = 70 metres."), ("First, 15 * 4 = 60 metres.".to_string(), 1), "a comma before it is no thousands separator");
        // Right, or right to the places written: untouched.
        for fine in ["12 × 8 = 96", "10 / 3 = 3.33", "2 + 2 = 4.", "1 / 3 = 0.3", "0.1 + 0.2 = 0.3", "3.5 + 1.25 = 4.8", "2 / 3 = 0.67"] {
            assert_eq!(check_figures(fine), (fine.to_string(), 0), "{fine}");
        }
        // Not a plain equation: longer expressions, percentages, thousands, dates, versions.
        for other in ["2 + 3 × 4 = 14", "(2 + 3) × 4 = 20", "50 / 2 = 25%", "1,000 + 1 = 1,001", "The 12 - 8 = 20 rule", "5 / 0 = 0", "on 2026-11-05 at 15:00", "v1.2 - 3 = ?"] {
            let (text, fixed) = check_figures(other);
            assert!(fixed == 0 || other.starts_with("The"), "{other} → {text}");
        }
        assert_eq!(check_figures("The 12 - 8 = 20 rule").0, "The 12 - 8 = 4 rule");
    }

    #[test]
    fn the_stream_filter_holds_only_an_unfinished_equation() {
        let mut f = FigureCheck::new();
        // Plain text passes at once.
        assert_eq!(f.push("The answer"), "The answer");
        assert_eq!(f.push(" is"), " is");
        // A number waits one token to see whether an equation follows.
        assert_eq!(f.push(" 12"), " ");
        assert_eq!(f.push(" ×"), "");
        assert_eq!(f.push(" 8 ="), "");
        assert_eq!(f.push(" 86"), "");
        assert_eq!(f.push(", Sir."), "12 × 8 = 96, Sir.");
        // A number that is no equation is released with the next word.
        assert_eq!(f.push(" It is 21"), " It is ");
        assert_eq!(f.push(" degrees"), "21 degrees");
        // The end of the reply releases what waited, checked.
        assert_eq!(f.push(" and 2 + 2 = 5"), " and ");
        assert_eq!(f.flush(), "2 + 2 = 4");
        assert_eq!(f.flush(), "");
        // Token by token, the result is the same as checking the whole text.
        let reply = "First, 15 * 4 = 70 metres. Then 60 / 4 = 15 seconds, so 3 x 3 stays as written and 9 - 4 = 5.";
        let mut f = FigureCheck::new();
        let mut streamed = String::new();
        for piece in reply.split_inclusive(' ') {
            streamed.push_str(&f.push(piece));
        }
        streamed.push_str(&f.flush());
        assert_eq!(streamed, check_figures(reply).0);
        assert!(streamed.contains("15 * 4 = 60 metres") && streamed.contains("60 / 4 = 15") && streamed.contains("9 - 4 = 5"));
    }

    #[test]
    fn requests_are_sorted_into_tiers_without_the_model() {
        let ctx = ReflexContext::default();
        for reflex in ["Move your orb to the left", "Close the HUD board", "Pause", "Set the volume to 40 percent", "Open Spotify"] {
            assert_eq!(tier(reflex, &ctx), Tier::Reflex, "{reflex}");
        }
        for tactical in ["What's the capital of France?", "Tell me a joke"] {
            assert_eq!(tier(tactical, &ctx), Tier::Tactical, "{tactical}");
        }
        for deep in ["Explain in detail how a transformer works", "Think carefully about whether I should buy a new GPU"] {
            assert_eq!(tier(deep, &ctx), Tier::Deep, "{deep}");
        }
        assert_eq!(Tier::Deep.label(), "deep");
    }
}
