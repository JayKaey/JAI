//! The Feature Lab (`[ FEATURES ]` in the header): every system JAI has, grouped as in the
//! capability manifest, each with one button. `[ TEST ALL PROBES ]` runs the live self-test
//! (`live_self_tester`): every pipeline, every route through the real brain as a dry run, every
//! tool (a read is run for real, anything else is checked to be wired) and the HUD itself, each
//! against its latency budget; the tally and every row's badge fill in while it runs. Read-only
//! probes (`SAFE_PROBES`) also run one at a time with `[ TEST ]` and report PASS / FAIL, their
//! time and the first line of their result; every other feature is `[ TRY ]`, which does
//! something real (`try_plan`): a
//! feature without input runs and its result opens as a board block, a feature with input runs a
//! safe demo or opens the HUD control that drives it (the mixer, the SCREENS mover, the Theme
//! Studio), and only what would send, delete, close or rewrite something waits in the text console
//! as an example request for Sir to send. The badge shows PASS / FAIL (or IN CONSOLE).

use super::{rgba, stroke, strip_font, strip_ink, strip_muted};
use crate::tools::{ToolContext, JAI_CAPABILITY_MANIFEST};
use eframe::egui::{self, Color32, Frame, Margin, Response, RichText, Rounding, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::runtime::Handle;

/// Tools that only read, and the arguments the lab probes them with.
pub(super) const SAFE_PROBES: &[(&str, &str)] = &[
    ("get_current_datetime", "{}"),
    ("get_system_status", "{}"),
    ("top_processes", "{}"),
    ("network_status", "{}"),
    ("diagnose_network_and_ping", r#"{"action":"ping"}"#),
    ("diagnose_integrations", "{}"),
    ("calculate", r#"{"expression": "2 + 2 * 3"}"#),
    ("get_calendar_schedule", r#"{"date_or_range": "today"}"#),
    ("check_school_schedule", "{}"),
    ("get_precise_location_weather", "{}"),
    ("check_ride_and_commute_status", "{}"),
    ("list_jai_features", "{}"),
    ("list_skills", "{}"),
    ("list_forged_tools", "{}"),
    ("reminders", r#"{"action": "list"}"#),
    ("control_audio_mixer", r#"{"action": "list"}"#),
    ("manage_deep_research", r#"{"action": "status"}"#),
    ("discord_call", r#"{"action": "status"}"#),
    ("get_remote_pc_status", "{}"),
    ("recall_memories", r#"{"query": "Sir"}"#),
    // The workbench's reads.
    ("lookup_formula", r#"{"query": "kinetic energy"}"#),
    ("chemistry_helper", r#"{"action": "molar_mass", "formula": "H2SO4"}"#),
    ("show_app_shortcuts", r#"{"app": "Windows"}"#),
    ("screen_time_report", r#"{"action": "today"}"#),
    ("suggest_routine", r#"{"action": "now"}"#),
    ("thermal_advisor", r#"{"action": "status"}"#),
    ("clipboard_history", r#"{"action": "list"}"#),
    ("manage_flashcards", r#"{"action": "due"}"#),
    ("suggest_commands", r#"{"prefix": "open"}"#),
];

/// Values of a required enum that only read ("action": "status" …), most telling first.
const READ_ONLY: &[&str] = &["status", "list", "summary", "get_volume", "what_is_playing", "list_playlists", "recent"];

/// Safe demos for features that need input (valid calls: the lab's test checks each one).
const DEMOS: &[(&str, &str)] = &[
    ("web_search", r#"{"query": "Stark Industries arc reactor"}"#),
    ("read_webpage", r#"{"url": "https://example.com"}"#),
    ("open_jai_view", r#"{"view": "system_stats"}"#),
    ("launch_interactive_simulation", r#"{"sim_type": "pendulum"}"#),
    ("find_files", r#"{"query": "jai"}"#),
    ("find_nearby_places_or_route", r#"{"query": "coffee"}"#),
    ("pop_up_ui_card", r#"{"title": "Feature Lab", "content": "Pop-up cards are online, Sir."}"#),
    ("spawn_native_window", r#"{"title": "Feature Lab", "markdown": "Native windows are online, Sir."}"#),
    ("create_or_update_widget", r#"{"title": "Feature Lab", "type": "MetricGauge", "value": 72, "unit": "%"}"#),
    ("hud_board", r#"{"action": "status"}"#),
    // (Opens the telemetry block if it is not up, then glides it to the middle: inside the HUD.)
    ("arrange_jai_window", r#"{"target": "telemetry", "action": "move", "position": "centre"}"#),
    // Without arguments these two would sort Sir's school files / need a host: a read instead.
    ("sync_study_materials_and_deadlines", r#"{"action": "list"}"#),
    ("server_deck", r#"{"hosts": ["mc.hypixel.net"]}"#),
    ("open_application", r#"{"app_name": "notepad"}"#),
    ("open_website", r#"{"platform_or_url": "https://example.com"}"#),
    ("control_spatial_window", r#"{"action": "retile"}"#),
];

/// Features a HUD control drives: `[ TRY ]` opens the control (or runs the call behind it).
const CONTROLS: &[(&str, Control)] = &[
    ("move_window_to_monitor", Control::Open("screens")),
    ("configure_interface", Control::Open("theme_studio")),
    ("customize_hud_and_windows", Control::Open("theme_studio")),
    ("control_gui_aspect", Control::Open("theme_studio")),
    ("control_audio_mixer", Control::Open("mixer")),
    ("media_and_volume_control", Control::Run("control_audio_mixer", r#"{"action": "list"}"#)),
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Control {
    /// A HUD control by id (`super::open_control`).
    Open(&'static str),
    /// The call that shows the control's state as a board block.
    Run(&'static str, &'static str),
}

/// What `[ TRY ]` does for a feature.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TryPlan {
    /// Runs `tool` with `args` and shows the result as a board block.
    Run { tool: String, args: Value },
    /// Opens a HUD control.
    Open(&'static str),
    /// Sends, deletes, closes or rewrites something, or needs Sir's words: an example request in
    /// the text console.
    Console(String),
}

/// Words in a tool's name that mean it changes something outside the lab's demo.
const SIDE_EFFECTS: &[&str] = &[
    "delete", "remove", "close", "send", "reply", "patch", "restore", "recompile", "shell", "overwrite", "edit", "cleanup", "undo", "forge_tool", "remote_pc", "lifecycle",
    "teleport", "beam", "modify", "create_", "remember", "sandbox", "focus_mode", "window_management", "listening_mode", "modality", "reboot", "spawn_multi", "spawn_coding", "orb_mode",
    "save_",
];

/// The block's id on the board.
const ID: &str = "features";

/// A probe's outcome: (passed, milliseconds, first line of the result); `None` while it runs.
type Outcome = Option<(bool, u64, String)>;

static OPEN: AtomicBool = AtomicBool::new(false);
static RESULTS: Mutex<BTreeMap<String, Outcome>> = Mutex::new(BTreeMap::new());
static FILTER: Mutex<String> = Mutex::new(String::new());
/// Features whose example request waits in the text console.
static STAGED: Mutex<std::collections::BTreeSet<String>> = Mutex::new(std::collections::BTreeSet::new());

fn results() -> MutexGuard<'static, BTreeMap<String, Outcome>> {
    RESULTS.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

pub(super) fn toggle() {
    OPEN.fetch_xor(true, Ordering::AcqRel);
}

pub(super) fn set_open(open: bool) {
    OPEN.store(open, Ordering::Release);
}

fn probe_args(name: &str) -> Option<Value> {
    SAFE_PROBES.iter().find(|(n, _)| *n == name).and_then(|(_, a)| serde_json::from_str(a).ok())
}

/// What `[ TRY ]` does for the tool `def`: its read-only probe, its HUD control, its safe demo, a
/// read-only action, a run without input — else an example request in the console.
pub(super) fn try_plan(def: &Value) -> TryPlan {
    let f = &def["function"];
    let name = f["name"].as_str().unwrap_or("");
    let run = |tool: &str, args: &str| TryPlan::Run { tool: tool.to_string(), args: serde_json::from_str(args).unwrap_or_default() };
    if let Some((_, args)) = SAFE_PROBES.iter().chain(DEMOS).find(|(n, _)| *n == name) {
        return run(name, args);
    }
    match CONTROLS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c) {
        Some(Control::Open(id)) => return TryPlan::Open(id),
        Some(Control::Run(tool, args)) => return run(tool, args),
        None => {}
    }
    let params = &f["parameters"];
    let required: Vec<&str> = params["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    // Every required parameter an enum with a read-only value: "status", "list" ….
    let mut args = serde_json::Map::new();
    for key in &required {
        let allowed = params["properties"][key]["enum"].as_array().cloned().unwrap_or_default();
        match READ_ONLY.iter().find(|v| allowed.iter().any(|a| a == *v)) {
            Some(v) => args.insert(key.to_string(), Value::from(*v)),
            None => return TryPlan::Console(format!("Use {} to ", name.replace('_', " "))),
        };
    }
    if required.is_empty() && SIDE_EFFECTS.iter().any(|w| name.contains(w)) {
        return TryPlan::Console(format!("Use {} to ", name.replace('_', " ")));
    }
    TryPlan::Run { tool: name.to_string(), args: Value::Object(args) }
}

/// Runs a `[ TRY ]` in the background: the badge shows the outcome and the result opens as a
/// board block (unless the feature put up a board of its own).
fn run_try(tools: &ToolContext, rt: &Handle, name: &str, tool: String, args: Value) {
    results().insert(name.to_string(), None);
    let (tools, name) = (tools.clone(), name.to_string());
    rt.spawn(async move {
        let before = crate::hud_board::screen_state();
        let o = tools.execute(&tool, &args).await;
        let lines: Vec<&str> = o.output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if crate::hud_board::screen_state() == before {
            use crate::hud_board::{Block, BlockKind, Note, Report};
            let notes = lines.iter().take(6).map(|l| Note { headline: tool.replace('_', " ").to_uppercase(), body: l.to_string() }).collect::<Vec<_>>();
            let notes = if notes.is_empty() { vec![Note { headline: tool.to_uppercase(), body: "Done.".into() }] } else { notes };
            let block = Block::new(format!("FEATURE LAB // {}", name.replace('_', " ")), if o.success { "PASS" } else { "FAIL" }, format!("TOOL: {tool}"), BlockKind::NoteStack { notes });
            crate::hud_board::present(Report { topic: format!("Feature Lab: {name}"), created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), blocks: vec![block], sources: Vec::new(), id: None });
        }
        let line = lines.first().copied().unwrap_or("");
        results().insert(name, Some((o.success, o.duration_ms, crate::crash_guard::clip(line, 160).to_string())));
        super::request_repaint();
    });
}

/// Every tool once, under its manifest category ("Everything else" for the rest), with its
/// description.
pub(super) fn catalogue(defs: &[Value]) -> Vec<(&'static str, Vec<(String, String)>)> {
    let entry = |d: &Value| Some((d["function"]["name"].as_str()?.to_string(), d["function"]["description"].as_str().unwrap_or("").to_string()));
    let mut groups: Vec<(&'static str, Vec<(String, String)>)> = JAI_CAPABILITY_MANIFEST
        .iter()
        .map(|c| (c.name, defs.iter().filter_map(entry).filter(|(n, _)| c.tools.contains(&n.as_str())).collect()))
        .collect();
    let rest = defs.iter().filter_map(entry).filter(|(n, _)| !JAI_CAPABILITY_MANIFEST.iter().any(|c| c.tools.contains(&n.as_str())));
    groups.push(("Everything else", rest.collect()));
    // A tool listed in two categories shows under the first.
    let mut seen = std::collections::HashSet::new();
    for (_, tools) in &mut groups {
        tools.retain(|(n, _)| seen.insert(n.clone()));
    }
    groups.retain(|(_, tools)| !tools.is_empty());
    groups
}

/// Runs one probe in the background; its row shows the outcome.
fn run_probe(tools: &ToolContext, rt: &Handle, name: &str) {
    let Some(args) = probe_args(name) else { return };
    results().insert(name.to_string(), None);
    let (tools, name) = (tools.clone(), name.to_string());
    rt.spawn(async move {
        let o = tools.execute(&name, &args).await;
        let line = o.output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
        results().insert(name, Some((o.success, o.duration_ms, crate::crash_guard::clip(&line, 160).to_string())));
        super::request_repaint();
    });
}

fn chip(ui: &mut Ui, text: &str) -> Response {
    crate::holo_ui::key(ui, text)
}

/// The tool definitions, their grouping and each tool's `[ TRY ]` plan: built once and again
/// only when a skill or a forged tool changed the tool list (never per frame: listing the
/// tools reads two SQLite tables and builds every schema).
struct Catalogue {
    /// When it was built (rebuilt after `CATALOGUE_TTL`, so a new skill shows up).
    built: std::time::Instant,
    groups: Vec<(&'static str, Vec<(String, String)>)>,
    plans: BTreeMap<String, TryPlan>,
}

static CATALOGUE: Mutex<Option<Arc<Catalogue>>> = Mutex::new(None);
const CATALOGUE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn cached_catalogue(tools: &ToolContext) -> Arc<Catalogue> {
    let mut cached = CATALOGUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cached.as_ref().filter(|c| c.built.elapsed() < CATALOGUE_TTL) {
        return Arc::clone(c);
    }
    let defs = tools.definitions();
    let plans = defs.iter().filter_map(|d| Some((d["function"]["name"].as_str()?.to_string(), try_plan(d)))).collect();
    let fresh = Arc::new(Catalogue { built: std::time::Instant::now(), groups: catalogue(&defs), plans });
    *cached = Some(Arc::clone(&fresh));
    fresh
}

/// The lab's content: the filter, `[ TEST ALL PROBES ]` with the live suite's tally, what failed,
/// then every category.
fn body(ui: &mut Ui, app: &crate::app::Jai, rt: &Handle) {
    use crate::live_self_tester as suite;
    let tools = app.tools();
    let catalogue = cached_catalogue(tools);
    let groups = &catalogue.groups;
    let total: usize = groups.iter().map(|(_, t)| t.len()).sum();
    let mut filter = FILTER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (running, tally) = (suite::running(), suite::summary());
    ui.horizontal(|ui| {
        crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut filter).hint_text("Filter features…").desired_width(240.0));
        if chip(ui, if running { "TESTING…" } else { "TEST ALL PROBES" })
            .on_hover_text("The live self-test: every pipeline, every route through the real brain (dry run), every tool and the HUD, each against its latency budget. Nothing is sent, said or changed.")
            .clicked()
            && !running
        {
            let app = app.clone();
            rt.spawn(async move {
                suite::run(&app).await;
            });
        }
        let line = if tally.total == 0 {
            format!("{total} FEATURES · LIVE SUITE NOT RUN YET")
        } else {
            format!("{total} FEATURES · {} PROBES · {} PASS · {} FAIL · {} SLOW · {} SKIP{}", tally.total, tally.pass, tally.fail, tally.slow, tally.skip, if running { " · RUNNING" } else { "" })
        };
        let colour = if tally.total > 0 && !running && tally.clean() { crate::holo::theme().ink() } else if tally.fail + tally.slow > 0 { crate::holo::theme().alert() } else { strip_muted() };
        ui.label(RichText::new(line).font(strip_font()).color(colour));
    });
    *FILTER.lock().unwrap_or_else(|e| e.into_inner()) = filter.clone();
    let needle = filter.trim().to_lowercase();
    ui.add_space(4.0);
    crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
        // What the live suite found wrong or slow, first; then its pipeline and HUD probes.
        let outcomes = suite::results();
        for (heading, wanted) in [("LIVE SUITE // FAILED OR SLOW", true), ("LIVE SUITE // PIPELINES · ROUTING · HUD", false)] {
            let shown: Vec<&suite::ProbeOutcome> = outcomes
                .iter()
                .filter(|o| matches!(o.status, suite::Status::Fail | suite::Status::Slow) == wanted && (wanted || o.group != "tool"))
                .filter(|o| needle.is_empty() || o.id.to_lowercase().contains(&needle) || o.group.contains(needle.as_str()))
                .collect();
            if shown.is_empty() {
                continue;
            }
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{heading} // {}", shown.len())).font(strip_font()).color(crate::hud_board::active_colour()));
            for o in shown {
                let colour = match o.status {
                    suite::Status::Pass => crate::holo::theme().ink(),
                    suite::Status::Skip => strip_muted(),
                    _ => crate::holo::theme().alert(),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} · {:.2} MS", o.status.label(), o.ms)).font(strip_font()).color(colour));
                    ui.label(RichText::new(format!("{} // {}", o.group.to_uppercase(), o.id)).font(strip_font()).color(strip_ink()));
                });
                ui.add(egui::Label::new(RichText::new(format!("→ {}", o.detail)).size(11.0).color(strip_muted())).wrap());
            }
        }
        for (group, features) in groups {
            let shown: Vec<&(String, String)> =
                features.iter().filter(|(n, d)| needle.is_empty() || n.contains(&needle) || d.to_lowercase().contains(&needle) || group.to_lowercase().contains(&needle)).collect();
            if shown.is_empty() {
                continue;
            }
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} // {}", group.to_uppercase(), shown.len())).font(strip_font()).color(crate::hud_board::active_colour()));
            for (name, description) in shown {
                let plan = catalogue.plans.get(name).cloned().unwrap_or(TryPlan::Console(format!("Use {} to ", name.replace('_', " "))));
                row(ui, tools, rt, name, description, plan);
            }
        }
    });
}

fn row(ui: &mut Ui, tools: &ToolContext, rt: &Handle, name: &str, description: &str, plan: TryPlan) {
    let probe = probe_args(name).is_some();
    let outcome = results().get(name).cloned();
    // What the live suite found for this tool (shown until the row is tested by hand).
    let suite = crate::live_self_tester::tool_outcome(name);
    ui.horizontal(|ui| {
        let hint = match &plan {
            _ if probe => "Run this read-only check now",
            TryPlan::Run { .. } => "Run it now; the result opens on the board",
            TryPlan::Open(_) => "Open the HUD control that drives it",
            TryPlan::Console(_) => "Changes or sends something: an example request goes into the text console for you to send",
        };
        if chip(ui, if probe { "TEST" } else { "TRY" }).on_hover_text(hint).clicked() {
            match plan.clone() {
                _ if probe => run_probe(tools, rt, name),
                TryPlan::Run { tool, args } => run_try(tools, rt, name, tool, args),
                TryPlan::Open(id) => {
                    super::open_control(id);
                    results().insert(name.to_string(), Some((true, 0, format!("opened the {} control", id.replace('_', " ").to_uppercase()))));
                }
                TryPlan::Console(text) => {
                    super::prefill_console(text);
                    STAGED.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string());
                }
            }
        }
        ui.label(RichText::new(name.to_uppercase()).font(strip_font()).color(strip_ink()));
        match &outcome {
            Some(None) => {
                ui.label(RichText::new("RUNNING…").font(strip_font()).color(strip_muted()));
            }
            Some(Some((ok, ms, _))) => {
                let (text, colour) = if *ok { ("PASS", crate::holo::theme().ink()) } else { ("FAIL", crate::holo::theme().alert()) };
                ui.label(RichText::new(format!("{text} · {ms} MS")).font(strip_font()).color(colour));
            }
            None if STAGED.lock().unwrap_or_else(|e| e.into_inner()).contains(name) => {
                ui.label(RichText::new("IN CONSOLE · SEND TO RUN").font(strip_font()).color(strip_muted()));
            }
            None => {
                if let Some(o) = &suite {
                    use crate::live_self_tester::Status;
                    let (text, colour) = match o.status {
                        Status::Pass if o.detail.starts_with("wired") => ("WIRED".to_string(), crate::holo::theme().ink()),
                        Status::Pass => (format!("PASS · {:.0} MS", o.ms), crate::holo::theme().ink()),
                        Status::Skip => ("SKIP".to_string(), strip_muted()),
                        Status::Slow => (format!("SLOW · {:.0} MS", o.ms), crate::holo::theme().alert()),
                        Status::Fail => ("FAIL".to_string(), crate::holo::theme().alert()),
                    };
                    ui.label(RichText::new(text).font(strip_font()).color(colour));
                }
            }
        }
    });
    let text = match (&outcome, &suite) {
        (Some(Some((_, _, line))), _) if !line.is_empty() => format!("→ {line}"),
        (None, Some(o)) if !o.detail.is_empty() => format!("→ {}", o.detail),
        _ => description.split_inclusive(['.', '!', '?']).next().unwrap_or(description).trim().to_string(),
    };
    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(strip_muted())).wrap());
}

/// Registers the lab with egui while it is open (each HUD frame): a block on the board, or its
/// own window once detached.
pub(super) fn show(ctx: &egui::Context, app: &crate::app::Jai, rt: &Handle) {
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, ViewportId::from_hash_of("jai_feature_lab"), is_open()) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, "Feature Lab // every system", [820.0, 680.0], |ui| body(ui, app, rt));
        if action.close {
            OPEN.store(false, Ordering::Release);
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Feature Lab — J.A.I.").with_app_id("jai").with_inner_size([820.0, 720.0]).with_min_inner_size([520.0, 400.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let (app, rt) = (app.clone(), rt.clone());
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jai_feature_lab"), builder, move |ctx, class| {
        crate::crash_guard::guard("feature lab", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!is_open() && !super::detached_leaving(ctx)) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Feature Lab").default_size([820.0, 680.0]).show(ctx, |ui| body(ui, &app, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, "Feature Lab // every system") {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui, &app, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

/// Whether `name` with the JSON `args` is a call the tool's schema accepts (the live suite's
/// check, on arguments written as text).
#[cfg(test)]
pub(super) fn check_call(defs: &[Value], name: &str, args: &str) -> Result<(), String> {
    let args: Value = serde_json::from_str(args).map_err(|e| format!("{name}: {e}"))?;
    crate::live_self_tester::check_call(defs, name, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_listed_once_and_every_probe_is_a_valid_call() {
        // As the app builds them: the skill registry adds list_skills and the other skill tools.
        let root = std::env::temp_dir().join(format!("jai_lab_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let skills = crate::skills::SkillManager::open(root.join("db.sqlite"), root.join("skills")).unwrap();
        let defs = crate::tools::definitions_with(skills.tool_definitions());
        let groups = catalogue(&defs);
        let listed: Vec<&str> = groups.iter().flat_map(|(_, t)| t.iter().map(|(n, _)| n.as_str())).collect();
        let unique: std::collections::HashSet<&str> = defs.iter().filter_map(|d| d["function"]["name"].as_str()).collect();
        assert_eq!(listed.len(), unique.len(), "each tool exactly once");
        assert!(unique.iter().all(|n| listed.contains(n)));
        for (name, args) in SAFE_PROBES {
            check_call(&defs, name, args).unwrap();
        }
    }

    /// Repro 2026-11-03 ("[ TRY ] does nothing"): every TRY only typed "Use <tool> to " into the
    /// console. Now each one runs a valid call, opens a real HUD control, or (only for what sends,
    /// deletes, closes or rewrites) stages an example request.
    #[test]
    fn every_try_does_something_real() {
        let root = std::env::temp_dir().join(format!("jai_lab_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let skills = crate::skills::SkillManager::open(root.join("db.sqlite"), root.join("skills")).unwrap();
        let defs = crate::tools::definitions_with(skills.tool_definitions());
        let (mut run, mut console) = (0, Vec::new());
        for def in &defs {
            let name = def["function"]["name"].as_str().unwrap();
            match try_plan(def) {
                TryPlan::Run { tool, args } => {
                    check_call(&defs, &tool, &args.to_string()).unwrap_or_else(|e| panic!("{name}: {e}"));
                    run += 1;
                }
                TryPlan::Open(id) => assert!(super::super::CONTROL_IDS.contains(&id), "{name} opens unknown control {id}"),
                TryPlan::Console(_) => console.push(name),
            }
        }
        for (name, control) in CONTROLS {
            assert!(defs.iter().any(|d| d["function"]["name"] == *name), "{name} is not a tool");
            if let Control::Run(tool, args) = control {
                check_call(&defs, tool, args).unwrap();
            }
        }
        for (name, args) in DEMOS {
            check_call(&defs, name, args).unwrap();
        }
        // Read-only checks and no-input features run; what changes the world waits for Sir.
        assert_eq!(try_plan(defs.iter().find(|d| d["function"]["name"] == "manage_vram").unwrap()), TryPlan::Run { tool: "manage_vram".into(), args: serde_json::json!({"action": "status"}) });
        assert_eq!(try_plan(defs.iter().find(|d| d["function"]["name"] == "media_and_volume_control").unwrap()), TryPlan::Run { tool: "control_audio_mixer".into(), args: serde_json::json!({"action": "list"}) });
        assert_eq!(try_plan(defs.iter().find(|d| d["function"]["name"] == "move_window_to_monitor").unwrap()), TryPlan::Open("screens"));
        for side_effect in ["send_whatsapp_message", "recompile_and_restart", "close_all_secondary_windows", "run_shell_command", "control_jai_lifecycle", "delete_memory", "save_instant_replay"] {
            assert!(console.contains(&side_effect), "{side_effect} must wait in the console");
        }
        // Found by the tool audit: without arguments this one sorts Sir's school files and adds
        // calendar entries. TRY only lists.
        assert_eq!(
            try_plan(defs.iter().find(|d| d["function"]["name"] == "sync_study_materials_and_deadlines").unwrap()),
            TryPlan::Run { tool: "sync_study_materials_and_deadlines".into(), args: serde_json::json!({"action": "list"}) }
        );
        assert!(run >= defs.len() / 2, "{run} of {} features run on TRY", defs.len());
    }
}

