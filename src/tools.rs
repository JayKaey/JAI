//! Tool registry: JSON schemas for Ollama native tool calling + one dispatcher used by the LLM,
//! the instant-command fast path and the UI.

use crate::config::ConfigStore;
use crate::file_ops::{self, EditOutcome};
use crate::focus::{self, FocusTracker, OwnThing};
use crate::lifecycle::{self, Restart};
use crate::media::{self, MediaAction};
use crate::memory::{DeleteOutcome, MemoryManager};
use crate::modality::{ModalityStore, VoiceResponseMode};
use crate::protocols::{Protocol, ProtocolStore};
use crate::reminders::ReminderStore;
use crate::skills::{JaiSkill, SkillManager, SkillParameter};
use crate::telemetry::SystemMonitor;
use crate::widgets::WidgetEngine;
use crate::winctl::{self, WindowAction, WindowTarget};
use crate::music::MusicEngine;
use crate::pim::{self, Pim};
use crate::{
    apps, calc, clipboard, diagnostics, gui, interface, memory, modality, monitors, music, protocols, reminders, screen, self_improve, shell,
    sites, websearch, widgets,
};
use std::path::PathBuf;
use std::time::Duration;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

#[derive(Debug, Serialize)]
pub struct CapabilityCategory {
    pub name: &'static str,
    pub summary: &'static str,
    pub tools: &'static [&'static str],
}

/// Human-readable capability taxonomy. The help response joins this with the live tool
/// schemas, saved skills, protocols and voice macros so no installed tool is omitted.
pub const JAI_CAPABILITY_MANIFEST: &[CapabilityCategory] = &[
    CapabilityCategory { name: "Music and welcome", summary: "Sonora/Spotify player selection; named playlists, songs and tracks within playlists; local-library indexing; playback, skip, volume and now playing. Say 'Play JaysRock', 'Play Back in Black in JaysRock', or 'Wake up, Daddy's home'. Playback depends on available local files, installed players and streaming access.", tools: &["control_music", "media_and_volume_control"] },
    CapabilityCategory { name: "Windows, monitors and desktop", summary: "Launch and close applications; focus, resize, snap, maximize and move real windows between monitors; inspect the screen or selected text; clipboard and system telemetry; \"tell me when\" watches (a download finishing, an app closing or starting, a website coming back); J.A.I.OS beside Windows (\"is my PC ready for J.A.I.OS?\", \"make room for J.A.I.OS\", \"make a J.A.I.OS stick\").", tools: &["jaios_setup", "open_application", "close_application", "window_management", "move_window_to_monitor", "inspect_screen_or_selection", "clipboard", "get_system_status", "top_processes", "network_status", "get_current_datetime", "focus_mode", "is_app_running", "watch_and_tell"] },
    CapabilityCategory { name: "Calendar, school and study", summary: "Apple/iCloud CalDAV and local calendar reads, creation, edits, conflicts and offline synchronization; WebUntis classes and cancellations; itslearning tasks and materials; deadline syncing, watchers, briefings and oral exam practice. Remote integrations require configured credentials.", tools: &["get_calendar_schedule", "create_calendar_event", "modify_or_delete_calendar_event", "check_school_schedule", "check_course_updates", "sync_study_materials_and_deadlines", "manage_schedule_watcher", "configure_productivity_service", "run_executive_briefing", "start_oral_exam_session", "answer_oral_exam", "set_smart_morning_alarm"] },
    CapabilityCategory { name: "Workout coach", summary: "Detect calendar workout entries such as Arme, record sets/reps/weights, estimate one-repetition personal records, display strength progression and share persistent rest timers between desktop and phone.", tools: &["manage_workout"] },
    CapabilityCategory { name: "Worldwide phone and remote PC", summary: "Token-protected mobile satellite with local and public HTTPS pairing URLs and QR codes; live CPU/GPU/process telemetry; remote locking, sleep, protocol execution and configured Wake-on-LAN. Internet access requires an active tunnel; waking a sleeping host requires another reachable device or relay.", tools: &["get_remote_pc_status", "control_remote_pc", "manage_remote_node", "manage_location_settings", "beam_to_device", "scan_with_phone", "teleport_content"] },
    CapabilityCategory { name: "Communication and contacts", summary: "Configured email and WhatsApp inboxes, summaries, search, contacts, explicit messages/replies, unified inbox and auto-responder controls; JAI's own Discord account (Sir's DMs are orders, friends get answers, he joins voice calls as himself, posts, reads and summarises); Away Mode (answering machine, alerts to Sir's phone, welcome-back digest).", tools: &["check_unread_emails", "summarize_email", "search_emails", "send_whatsapp_message", "check_whatsapp_messages", "reply_to_last_whatsapp", "manage_contacts", "check_all_communications", "set_auto_responder", "discord_bot", "away_mode"] },
    CapabilityCategory { name: "Stark protocols, macros and reminders", summary: "Create and run saved app/music/window protocols, capture workspaces, teach multi-step voice macros, set reminders and timers, select spoken/text response modes, control the assistant lifecycle, take back the last change of his own ('Undo that'), tell what he just did and what is new in this build.", tools: &["manage_stark_protocol", "manage_voice_macro", "reminders", "set_response_modality", "control_jai_lifecycle", "shutdown_jai", "undo_last_action", "recent_actions", "whats_new", "whats_open", "timer_left"] },
    CapabilityCategory { name: "Memory and learned skills", summary: "Store and recall facts, search conversation history, manage persistent skills and execute installed skill tools.", tools: &["remember_fact", "recall_memories", "delete_memory", "search_conversation", "create_skill", "execute_skill", "create_persistent_skill", "run_custom_skill", "list_skills", "delete_skill"] },
    CapabilityCategory { name: "Files, code and automation", summary: "Locate/read/write/edit files, undo edits, search source contents, run commands, generate projects, inspect source, apply self-improvements and perform reversible folder cleanup.", tools: &["find_files", "read_text_file", "create_or_overwrite_file", "edit_file_surgical", "undo_last_file_edit", "search_inside_files", "run_shell_command", "spawn_coding_project", "patch_own_source", "smart_folder_cleanup", "undo_last_cleanup", "run_sandbox_calculation_or_script", "diagnose_minecraft_or_server"] },
    CapabilityCategory { name: "Web, research and locations", summary: "Web search and page reading, connected websites, background research, location-aware weather, nearby places, routes and commute checks, network diagnosis and price watches.", tools: &["web_search", "read_webpage", "open_website", "query_connected_website", "start_background_research", "get_precise_location_weather", "find_nearby_places_or_route", "check_ride_and_commute_status", "diagnose_network_and_ping", "manage_price_sniper"] },
    CapabilityCategory { name: "Visual tools, media and analysis", summary: "Native widgets and windows, a self-configuring HUD (themes, two cores, a resizable orb, text and window size) and live real-file project graph, interactive simulations, calculation, code/idea analysis, supported media extraction, instant replay and local finance/goal tracking.", tools: &["create_or_update_widget", "remove_widget", "spawn_native_window", "close_native_window", "customize_hud_and_windows", "configure_interface", "control_jai_interface", "launch_interactive_simulation", "adjust_simulation", "calculate", "stress_test_idea_or_code", "download_or_extract_media", "save_instant_replay", "manage_finances_and_goals", "list_jai_features"] },
    CapabilityCategory { name: "Workbench", summary: "Spaced-repetition flashcards, the maths/physics/chemistry formula sheet, molar masses and mole conversion, keyboard-shortcut sheets for the program in front, screen time with a break coach, routines learned from what Sir asks for, an hour of temperatures with a fan-curve verdict, clipboard history (in memory only), tiling program windows on a monitor and predictive command completion in the console.", tools: &["manage_flashcards", "lookup_formula", "chemistry_helper", "show_app_shortcuts", "screen_time_report", "suggest_routine", "thermal_advisor", "clipboard_history", "tile_windows", "suggest_commands"] },
];

/// Tools few requests need, behind the `more_tools` door: the prompt is ≈7k tokens lighter (room
/// for the conversation, quicker loads). Each is still carried out when called by name.
pub const BEHIND_DOOR: &[&str] = &[
    "discord_bot", "teleport_content", "spawn_multi_window_workspace", "manage_remote_node", "manage_stark_protocol", "forge_tool",
    "edit_file_surgical", "download_or_extract_media", "manage_voice_macro", "set_smart_morning_alarm", "manage_location_settings",
    "manage_workout", "show_mobile_satellite_instructions", "patch_own_source", "watch_and_tell", "chemistry_helper", "spawn_coding_project",
    "manage_finances_and_goals", "sync_study_materials_and_deadlines", "diagnose_minecraft_or_server", "manage_contacts", "adjust_simulation",
    "control_spatial_window", "shadow_memory", "run_sandbox_calculation_or_script", "manage_spatial_scene", "smart_folder_cleanup", "start_oral_exam_session",
    "lookup_formula", "configure_productivity_service", "show_app_shortcuts", "manage_schedule_watcher", "spawn_topic_knowledge_workspace",
    "diagnose_network_and_ping", "clipboard_history", "beam_to_device", "control_remote_pc", "suggest_routine", "scan_with_phone",
    "thermal_advisor", "inspect_own_source", "stress_test_idea_or_code", "diagnose_integrations", "show_live_camera", "server_deck",
];

fn door_definition() -> Value {
    json!({"type":"function","function":{
        "name":"more_tools",
        "description":format!("Tools not listed above, for less common requests: {}. Call this with the names a request needs; it answers with their parameters, then call those tools by name.", BEHIND_DOOR.join(", ")),
        "parameters":{"type":"object","properties":{"names":{"type":"array","items":{"type":"string"},"description":"Tool names from the list"}},"required":["names"]}
    }})
}

pub fn prompt_definitions_from(all: Vec<Value>) -> Vec<Value> {
    let mut shown: Vec<Value> = all.into_iter().filter(|t| !BEHIND_DOOR.contains(&t["function"]["name"].as_str().unwrap_or(""))).collect();
    shown.push(door_definition());
    shown
}

/// The door's answer: the parameters of the tools asked for.
fn more_tools(all: &[Value], args: &Value) -> Result<String, String> {
    let door: Vec<String> = BEHIND_DOOR.iter().map(|n| n.to_string()).collect();
    let asked: Vec<String> = args["names"].as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(|n| canonical_tool_name(n, &door)).collect();
    let shown: Vec<&Value> = all.iter().map(|t| &t["function"]).filter(|f| asked.iter().any(|n| f["name"] == n.as_str())).collect();
    if shown.is_empty() {
        return Err(format!("Name the tools you need; behind the door are: {}.", BEHIND_DOOR.join(", ")));
    }
    Ok(format!("Call these by name: {}", serde_json::to_string(&shown).unwrap_or_default()))
}

/// Door tools that failed this turn: only the first failure carries the parameters (a second one
/// has another cause, such as the other PC being off, and the model read them as "try again").
// shortcut: one list for the whole process, so a background mission failing the same door tool during Sir's turn uses up his first failure; keep it per turn if missions call door tools.
static DOOR_FAILED: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// A new turn: each door tool's first failure carries its parameters again.
pub fn new_turn() {
    DOOR_FAILED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn first_failure(name: &str) -> bool {
    DOOR_FAILED.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string())
}

fn capability_tool_definition() -> Value {
    json!({"type":"function","function":{
        "name":"list_jai_features",
        "description":"Report actual JAI capabilities, registered tools, saved protocols, voice macros and installed skills. Use this for 'What can you do?', feature lists and capability questions such as 'Can you play a specific playlist?' without executing those actions.",
        "parameters":{"type":"object","properties":{"topic":{"type":"string","description":"Optional capability topic, for example music, calendar, phone or workout"}},"required":[]}
    }})
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolOutcome {
    pub tool: String,
    pub args: Value,
    pub output: String,
    pub success: bool,
    pub duration_ms: u64,
    #[serde(skip)]
    pub memories_changed: bool,
    #[serde(skip)]
    pub skills_changed: bool,
    /// Sir told JAI to quit; the app exits once the reply has been spoken.
    #[serde(skip)]
    pub shutdown_requested: bool,
    /// The voice response mode changed (the rest of the turn follows the new one).
    #[serde(skip)]
    pub modality_changed: bool,
    /// Sleep mode or the voice mute changed.
    #[serde(skip)]
    pub state_changed: bool,
}

tokio::task_local! {
    /// Inside a call an account was allowed: what it does in turn is allowed with it.
    static VOUCHED: ();
}

#[derive(Clone)]
pub struct ToolContext {
    pub memory: MemoryManager,
    pub skills: SkillManager,
    pub monitor: SystemMonitor,
    /// JAI's own voice loudness (0-200 %), separate from the system volume.
    pub voice_volume: Arc<AtomicU8>,
    /// Live settings; `configure_interface` restyles the HUD through them.
    pub config: ConfigStore,
    /// When JAI answers out loud.
    pub modality: ModalityStore,
    /// Dynamic widgets and extra native windows.
    pub widgets: WidgetEngine,
    pub reminders: ReminderStore,
    pub protocols: ProtocolStore,
    /// JAI's speaking rate (f32 bits, 0.5-2.0), applied live by the TTS.
    pub voice_speed: Arc<AtomicU32>,
    /// What "it", "that", "full screen" and "close it" refer to.
    pub focus: FocusTracker,
    /// Playlists, songs and the media session.
    pub music: MusicEngine,
    /// The personal manager: e-mail, calendar, WebUntis, itslearning, web connectors, watchers.
    pub pim: Pim,
    pub workout: crate::workout_coach::WorkoutCoach,
    /// Context graph, contacts, WhatsApp, inbox, macros, hivemind, location and the Simulation Lab.
    pub stark: crate::stark::StarkSystems,
    /// Whose turn these tools serve: Sir (everything on his main PC), his order from there, or an
    /// account (what Sir allowed it).
    pub caller: crate::accounts::Caller,
}

/// Names small models invent for real tools ("open_app", "search_web", "set_timer" …).
fn canonical_tool_name(name: &str, known: &[String]) -> Option<String> {
    let n = name.trim().to_lowercase().replace(['-', ' ', '.'], "_");
    if known.contains(&n) {
        return Some(n);
    }
    let alias = match n.as_str() {
        "open_app" | "launch_app" | "launch_application" | "start_application" | "open" | "launch" | "start_app" | "run_app" => "open_application",
        "close_app" | "kill_app" | "kill_application" | "quit_application" | "close" | "terminate_application" => "close_application",
        "search" | "search_web" | "websearch" | "google" | "internet_search" | "web" => "web_search",
        "read_url" | "fetch_url" | "fetch_webpage" | "read_page" | "get_webpage" => "read_webpage",
        "open_url" | "open_site" | "open_web" | "browse" | "browse_to" | "navigate" | "go_to_website" | "open_channel" | "open_profile"
        | "youtube_search" | "search_youtube" | "site_search" | "website" => "open_website",
        "read_file" | "open_text_file" | "view_file" | "cat" | "show_file" => "read_text_file",
        "write_file" | "create_file" | "save_file" | "overwrite_file" | "new_file" => "create_or_overwrite_file",
        "edit_file" | "modify_file" | "patch_file" | "replace_in_file" | "change_file" | "edit_code" => "edit_file_surgical",
        "undo_edit" | "revert_file" | "undo_file_edit" | "undo" | "restore_file" => "undo_last_file_edit",
        "grep" | "search_in_files" | "find_in_files" | "search_file_contents" | "search_code" | "grep_files" => "search_inside_files",
        "fullscreen" | "maximize_window" | "minimize_window" | "window" | "snap_window" => "window_management",
        "customize_hud" | "customize_window" | "style_window" | "restyle_window" | "theme_window" => "customize_hud_and_windows",
        "control_interface" | "interface_layout"
        | "control_ui" | "ui_layout" | "orb_size" | "set_orb_size" | "control_jai_ui" => "control_jai_interface",
        "orb_mode" | "pure_orb_mode" | "orb_only" | "set_orb_mode" | "toggle_orb_mode" => "control_orb_mode",
        "mixer" | "audio_mixer" | "volume_mixer" | "app_volume" | "set_app_volume" | "mute_app" => "control_audio_mixer",
        "listening_mode" | "hands_free" | "wake_word_mode" | "set_wake_word" | "handsfree_mode" => "set_listening_mode",
        "shutdown" | "quit" | "exit" | "exit_jai" | "quit_jai" | "shutdown_self" | "close_jai" | "stop_jai" => "shutdown_jai",
        "remember" | "save_memory" | "store_memory" | "save_fact" | "add_memory" | "memorize" => "remember_fact",
        "recall" | "search_memory" | "get_memories" | "memory_search" => "recall_memories",
        "create_widget" | "add_widget" | "update_widget" | "widget" | "make_widget" => "create_or_update_widget",
        "delete_widget" | "remove_widgets" => "remove_widget",
        "open_window" | "new_window" | "create_window" | "spawn_window" | "show_window" => "spawn_native_window",
        "close_window" | "close_jai_window" => "close_native_window",
        "set_reminder" | "remind" | "remind_me" | "create_reminder" | "add_reminder" | "list_reminders" | "cancel_reminder" => "reminders",
        "set_timer" | "timer" | "start_timer" => "create_or_update_widget",
        "calculator" | "calc" | "math" | "compute" | "convert" | "convert_units" | "unit_conversion" => "calculate",
        "briefing" | "morning_briefing" | "daily_brief" | "status_report" | "daily_briefing" | "executive_briefing" | "stark_briefing" | "day_briefing" => {
            "run_executive_briefing"
        }
        "check_email" | "check_emails" | "read_email" | "read_emails" | "emails" | "inbox" | "check_inbox" | "unread_emails" | "get_emails" => "check_unread_emails",
        "email_summary" | "summarise_email" | "read_mail" => "summarize_email",
        "find_email" | "find_emails" | "search_mail" | "search_inbox" => "search_emails",
        "calendar" | "get_calendar" | "schedule" | "get_schedule" | "calendar_events" | "my_calendar" | "agenda" => "get_calendar_schedule",
        "add_event" | "create_event" | "add_calendar_event" | "schedule_event" | "new_event" | "add_appointment" => "create_calendar_event",
        "delete_event" | "move_event" | "reschedule_event" | "update_event" | "cancel_event" | "edit_event" | "delete_calendar_event" => {
            "modify_or_delete_calendar_event"
        }
        "timetable" | "school_schedule" | "webuntis" | "stundenplan" | "check_timetable" | "school_timetable" | "get_timetable" => "check_school_schedule",
        "itslearning" | "course_updates" | "check_courses" | "lms" | "get_course_updates" => "check_course_updates",
        "query_website" | "connected_website" | "web_connector" | "check_website" | "read_connected_site" => "query_connected_website",
        "study_sync" | "sync_deadlines" | "organize_study_files" | "summarize_worksheet" | "study_assistant" | "sync_study_materials" => {
            "sync_study_materials_and_deadlines"
        }
        "watch_schedule" | "schedule_watcher" | "change_notifier" | "watcher" | "watch_service" => "manage_schedule_watcher",
        "productivity_settings" | "integrations" | "configure_service" | "integration_settings" | "productivity_hub" => "configure_productivity_service",
        "run_protocol" | "engage_protocol" | "create_protocol" | "protocol" | "protocols" | "stark_protocol" | "execute_protocol" | "save_workspace"
        | "capture_workspace" => "manage_stark_protocol",
        "focus" | "start_focus" | "focus_session" => "focus_mode",
        "get_clipboard" | "read_clipboard" | "copy" | "copy_to_clipboard" | "set_clipboard" => "clipboard",
        "screen" | "ocr" | "read_window" | "screenshot_text" | "look_at_screen" | "read_screen" | "inspect_screen" | "read_selection" | "vision"
        | "screen_vision" => "inspect_screen_or_selection",
        "play_music" | "music" | "play_song" | "play_playlist" | "spotify" | "play" | "music_control" | "pause_music" | "now_playing" => "control_music",
        "move_window" | "move_to_monitor" | "move_app_to_monitor" | "monitor_move" | "move_to_screen" => "move_window_to_monitor",
        "find_file" | "search_files" | "locate_file" | "file_search" | "open_file" => "find_files",
        "conversation_history" | "search_history" | "recall_conversation" => "search_conversation",
        "network" | "ping" | "internet_status" | "check_internet" | "ip_address" => "network_status",
        "processes" | "list_processes" | "task_manager" | "system_processes" => "top_processes",
        "volume" | "set_volume" | "media" | "media_control" => "media_and_volume_control",
        "system_status" | "status" | "system_info" | "telemetry" => "get_system_status",
        "shell" | "run_command" | "powershell" | "bash" | "terminal" | "execute_command" => "run_shell_command",
        "lifecycle" | "restart" | "sleep" | "mute" => "control_jai_lifecycle",
        "set_mode" | "response_mode" | "modality" | "set_modality" => "set_response_modality",
        "whatsapp" | "send_whatsapp" | "whatsapp_message" | "send_message" | "message" | "text_message" | "send_text" => "send_whatsapp_message",
        "read_whatsapp" | "whatsapp_messages" | "check_whatsapp" | "get_whatsapp" | "whatsapp_inbox" => "check_whatsapp_messages",
        "reply_whatsapp" | "reply" | "reply_to_message" | "whatsapp_reply" => "reply_to_last_whatsapp",
        "contacts" | "add_contact" | "save_contact" | "contact" | "find_contact" => "manage_contacts",
        "check_messages" | "all_messages" | "omni_inbox" | "check_communications" | "messages" => "check_all_communications",
        "auto_reply" | "autoresponder" | "auto_responder" | "away_message" | "do_not_disturb" => "set_auto_responder",
        "teleport" | "send_content" | "share" | "forward" => "teleport_content",
        "macro" | "create_macro" | "voice_macro" | "routine" | "run_macro" => "manage_voice_macro",
        "remote_node" | "ssh" | "server" | "node" | "remote" | "pi" | "raspberry_pi" | "minecraft_server" | "hivemind" => "manage_remote_node",
        "weather_here" | "local_weather" | "gps_weather" | "precise_weather" | "location_weather" | "forecast" => "get_precise_location_weather",
        "nearby" | "find_nearby" | "places" | "route" | "directions" | "where_am_i" | "nearby_places" | "distance" => "find_nearby_places_or_route",
        "commute" | "ride_check" | "ride" | "commute_status" | "when_to_leave" | "departure" => "check_ride_and_commute_status",
        "location_settings" | "set_home" | "geofence" | "satellite" => "manage_location_settings",
        "simulate" | "simulation" | "plot" | "plot_function" | "graph_function" | "physics_simulation" | "simulation_lab" => "launch_interactive_simulation",
        "update_simulation" | "change_simulation" | "set_simulation" | "simulation_parameters" => "adjust_simulation",
        "features" | "capabilities" | "help" | "what_can_you_do" | "list_features" => "list_jai_features",
        "workout" | "gym" | "log_workout" | "personal_records" | "rest_timer" => "manage_workout",
        "open_view" | "jai_view" | "open_subwindow" | "spawn_jai_subwindow" | "open_system_stats" | "system_monitor" | "open_panel_window" => {
            "open_jai_view"
        }
        _ => "",
    };
    if !alias.is_empty() {
        return Some(alias.to_string());
    }
    known
        .iter()
        .map(|k| (k, strsim::jaro_winkler(&n, k)))
        .filter(|(_, s)| *s >= 0.92)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k.clone())
}

/// Tools whose execution takes long enough that JAI should acknowledge first.
pub fn is_slow(tool: &str) -> bool {
    matches!(
        tool,
        "web_search" | "read_webpage" | "run_shell_command" | "create_skill" | "execute_skill" | "create_persistent_skill" | "run_custom_skill"
            | "patch_own_source" | "search_inside_files"
    ) || tool.starts_with("skill_")
        || pim::is_slow(tool)
        || crate::stark::is_slow(tool)
}

/// This run is a silent scripted session (`--silent`): tools act inside JAI only.
static SILENT_SESSION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_silent_session(on: bool) {
    SILENT_SESSION.store(on, Ordering::Release);
}

pub fn silent_session() -> bool {
    SILENT_SESSION.load(Ordering::Acquire)
}

/// What a silent scripted session may really carry out: everything that only reads, and what
/// stays inside JAI (his HUD, his windows and widgets, his board, his own voice and modes).
/// The volume is his too there: the mixer works on a copy of the real one (`audio_mixer`'s
/// bench). Anything that reaches the PC around him (an application, the music, a window of
/// Sir's, the keyboard, a message, a file, the power) is noted and left undone. (A script once
/// held "mute everything except Discord"; it was stopped three requests before that one.)
pub fn stays_inside(name: &str, args: &Value) -> bool {
    if crate::hyper_cortex::is_read_only(name, args) {
        return true;
    }
    if crate::kit::is_tool(name) {
        return crate::kit::stays_inside(name, args);
    }
    // (Whether he starts with the PC is the system's: the registry or the autostart folder.)
    if name == "configure_interface" && args.get("start_with_system").is_some() {
        return false;
    }
    let action = args["action"].as_str().unwrap_or("");
    match name {
        "configure_interface" | "control_jai_interface" | "customize_hud_and_windows" | "control_orb_mode" | "spawn_native_window" | "update_native_window"
        | "close_native_window" | "close_all_secondary_windows" | "create_or_update_widget" | "remove_widget" | "hud_board" | "open_jai_view" | "control_popups"
        | "set_response_modality" | "set_listening_mode" | "listening_lock" | "control_audio_mixer" | "thermal_advisor" | "undo_last_action" | "lookup_formula" | "chemistry_helper"
        | "more_tools" => true,
        // His own views moved and sized (with "it" an app, it hands over to the window tools,
        // which a silent session stops in their turn).
        "arrange_jai_window" => true,
        // The volume (on the copy) and his own voice, never the media keys.
        "media_and_volume_control" => matches!(action, "set_volume" | "volume" | "volume_up" | "volume_down" | "mute" | "unmute" | "get_volume") || action.starts_with("jai_") || action.contains("jai"),
        // His own guide (the phone link itself would restart the tunnel).
        "show_mobile_satellite_instructions" => args["topic"] == "permanent_address",
        // "Get rid of it" says so when there is nothing of his; it never reaches a program.
        "close_application" => bool_arg(args, "own_only") == Some(true),
        // Pictures, live cameras and street view are his blocks; saving writes into Pictures.
        "show_live_camera" | "show_street_view" => true,
        "show_image" => action != "save",
        // Himself or a window of his own to another monitor, nobody else's window.
        "move_window_to_monitor" => args["app_or_window_name"].as_str().is_some_and(|n| names_jai(n) || matches!(n.to_lowercase().as_str(), "hud" | "the hud" | "your hud" | "your window" | "your interface") || (crate::apps::find_spec(n).is_none() && crate::own_windows::open(n))),
        // A simulation is a window of his own.
        "launch_interactive_simulation" => true,
        // The 3D studio is his own block; saving a model writes a file of Sir's.
        "show_3d_model" => action != "export",
        _ => false,
    }
}

/// What the model is told when it reached for an action on a plain question.
/// What a silent scripted session says of a call it leaves undone (outside JAI).
pub const NOT_CARRIED_OUT: &str = "Noted, not carried out: this is a silent scripted session, which acts inside JAI only.";

pub const ONLY_ASKED: &str = "Nothing was done: Sir asked a question and gave no order. Answer the question itself in one or two sentences, and if doing it would help him, end with \"Shall I?\".";

/// What a plain question may use ("should I close Firefox?" once closed it): what only reads,
/// works something out or shows it. A tool with actions goes by the action.
pub fn only_looks(name: &str, args: &Value) -> bool {
    const BY_NAME: &[&str] = &["check_", "diagnose_", "find_", "get_", "inspect_", "list_", "lookup_", "read_", "recall_", "search_", "show_", "suggest_", "summarize_"];
    const LOOKING: &[&str] = &["more_tools", "chemistry_helper", "pop_up_ui_card", "run_executive_briefing", "run_sandbox_calculation_or_script", "screen_time_report", "spawn_topic_knowledge_workspace", "stress_test_idea_or_code", "thermal_advisor"];
    const READ_ACTIONS: &[&str] = &["check", "due", "find", "get", "history", "info", "list", "overview", "query", "read", "recap", "recent", "recommend", "report", "search", "show", "stats", "status", "summary", "view"];
    crate::hyper_cortex::is_read_only(name, args)
        || BY_NAME.iter().any(|prefix| name.starts_with(prefix))
        || LOOKING.contains(&name)
        || crate::kit::only_looks(name, args)
        // ("Where was I?" is asked, and bringing it back is the answer.)
        || name == "where_was_i"
        // ("Are you there?" wakes the board: that is the answer.)
        || (name == "hud_board" && args["action"] == "wake")
        // ("What's next?", "what did I miss?": he shows it, and that is the answer.)
        || (name == "configure_interface" && args.as_object().is_some_and(|asked| !asked.is_empty() && asked.iter().all(|(key, on)| matches!(key.as_str(), "next_now" | "missed") && on == true)))
        || args["action"].as_str().is_some_and(|action| READ_ACTIONS.iter().any(|read| action == *read || action.strip_prefix(read).is_some_and(|rest| rest.starts_with('_'))))
}

/// The outcome of a call a plain question does not warrant: nothing ran.
pub fn only_asked(name: &str, args: &Value) -> ToolOutcome {
    ToolOutcome {
        tool: name.to_string(),
        args: args.clone(),
        output: ONLY_ASKED.to_string(),
        success: true,
        duration_ms: 0,
        memories_changed: false,
        skills_changed: false,
        shutdown_requested: false,
        modality_changed: false,
        state_changed: false,
    }
}

/// "4 minutes and 30 seconds", "58 seconds", "1 hour and 2 minutes": a span of time as it is said.
fn spoken_span(ms: i64) -> String {
    let unit = |n: i64, name: &str| format!("{n} {name}{}", if n == 1 { "" } else { "s" });
    let seconds = (ms + 999) / 1000;
    match (seconds / 3600, seconds % 3600 / 60, seconds % 60) {
        (0, 0, s) => unit(s, "second"),
        (0, m, 0) => unit(m, "minute"),
        (0, m, s) => format!("{} and {}", unit(m, "minute"), unit(s, "second")),
        (h, 0, _) => unit(h, "hour"),
        (h, m, _) => format!("{} and {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// "Close JAI" / "close yourself" means quitting this app, not killing a process.
pub(crate) fn names_jai(app: &str) -> bool {
    let n: String = app.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect();
    let n = n.trim();
    let n = n.strip_prefix("the ").unwrap_or(n);
    matches!(
        n,
        "jai" | "jay" | "jaiexe" | "jai app" | "yourself" | "you" | "your app" | "your application" | "your program"
            | "your process" | "your window" | "assistant" | "jai assistant" | "dich" | "dich selbst" | "urself" | "ur self"
            | "your self" | "yourselfe" | "yo self" | "self"
    )
}

/// A `close_application` call that names no program: only fillers ("up", "off", "Earth, you"
/// without the Earth) close something of his that is up, never the program Sir works in; one of
/// his own views by name ("earth you") closes as his own. None: the call stands as it is.
fn own_close(name: &str, args: &Value) -> Option<(&'static str, Value)> {
    if name != "close_application" {
        return None;
    }
    let raw = str_arg(args, &["app_name", "name", "app", "application"])?;
    if focus::is_pronoun(raw) || names_jai(raw) {
        return None;
    }
    let lower: String = raw.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' }).collect();
    let cleaned = crate::intent_engine::strip_close_fillers(&lower);
    let cleaned = cleaned.trim().trim_start_matches("the ").trim_start_matches("my ").trim();
    if cleaned.is_empty() {
        warn!("[TOOL] close_application '{raw}' names no program: something of his, if anything");
        return Some(("close_application", json!({ "app_name": "it", "force": true, "own_only": true })));
    }
    if crate::intent_engine::is_own_view(cleaned) {
        return Some(("hud_board", json!({ "action": "close", "target": crate::intent_engine::own_view_name(cleaned) })));
    }
    // Something that is up on his own board under that name ("the briefing", "the timetable")
    // is his before it is a program of Windows'. (Seen live 2026-11-12: "close the briefing"
    // looked for a program called Briefing.)
    if crate::hud_board::names_block(cleaned) || !crate::hud_dock::blocks_named(cleaned).is_empty() {
        return Some(("hud_board", json!({ "action": "close", "target": cleaned })));
    }
    None
}

/// The first '…'-quoted part of a tool result ("Opened native window 'window_2' …" → window_2).
fn quoted(s: &str) -> &str {
    s.split('\'').nth(1).unwrap_or("")
}

pub(crate) fn str_arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub(crate) fn int_arg(args: &Value, key: &str) -> Option<i64> {
    match args.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.round() as i64)),
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
}

pub(crate) fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    match args.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Every tool definition, Sir's learned `skills` in their stable slot.
pub fn definitions_with(skills: Vec<Value>) -> Vec<Value> {
    let mut tools = MemoryManager::tool_definitions();
    tools.push(memory::conversation_tool_definition());
    tools.extend(pc_tool_definitions());
    tools.push(monitors::tool_definition());
    tools.push(music::tool_definition());
    tools.extend(assistant_tool_definitions());
    tools.extend(pim::tool_definitions());
    tools.extend(crate::stark::tool_definitions());
    tools.extend(widgets::tool_definitions());
    tools.extend(protocols::tool_definitions());
    tools.extend(self_improve::tool_definitions());
    tools.extend(skills);
    tools.push(capability_tool_definition());
    tools.push(crate::workout_coach::tool_definition());
    tools.push(crate::jai_views::tool_definition());
    tools.extend(crate::remote_guardian::definitions());
    // Spatial computing: VRAM guard, deep research, the workspace windows and their profiles.
    tools.extend(crate::vram_guard::definitions());
    tools.extend(crate::deep_research_engine::definitions());
    tools.extend(crate::workspace_canvas::definitions());
    tools.extend(crate::spatial_scenes::definitions());
    tools.extend(crate::intent_engine::definitions());
    tools.extend(crate::dynamic_popups::definitions());
    tools.extend(crate::tool_forge::definitions());
    tools.extend(crate::hud_board::definitions());
    tools.extend(crate::venue_guide::definitions());
    tools.extend(crate::discord_bridge::definitions());
    tools.extend(crate::discord_bot::definitions());
    tools.extend(crate::away_mode::definitions());
    tools.extend(crate::watch_and_tell::definitions());
    tools.extend(crate::shadow_memory::definitions());
    tools.extend(crate::aegis_server_deck::definitions());
    tools.extend(crate::llm_reboot::definitions());
    tools.extend(crate::window_commands::definitions());
    // Pictures, live public cameras and street view.
    tools.extend(crate::image_viewer::definitions());
    tools.extend(crate::webcams::definitions());
    tools.extend(crate::street_view::definitions());
    tools.extend(crate::model3d::definitions());
    tools
}

impl ToolContext {
    pub fn feature_manifest(&self, topic: Option<&str>) -> String {
        let topic = topic.unwrap_or("").trim().to_lowercase();
        if ["music", "playlist", "song", "sonora", "spotify"].iter().any(|word| topic.contains(word)) {
            return "Certainly, Sir. I can play a specific playlist or track using Sonora or Spotify, including a song within a named playlist. Tell me the name, for example 'Play JaysRock', 'Play Back in Black in JaysRock', or 'Play Highway to Hell'. I search the local library and known playlists first, and can open a Spotify search when needed. I can also pause, resume, skip, adjust volume and report what is playing. Playback requires an available player and a matching local file or accessible streaming result.".into();
        }
        let definitions = self.definitions();
        let mut shown = std::collections::HashSet::new();
        let mut output = String::from("Certainly, Sir. These are my implemented capabilities. Connected services need their corresponding account/configuration.\n");
        for category in JAI_CAPABILITY_MANIFEST {
            if !topic.is_empty() && !category.name.to_lowercase().contains(&topic) && !category.summary.to_lowercase().contains(&topic) { continue; }
            // Sir reads (and hears) this: the area and its summary, never raw tool identifiers or
            // their model-facing descriptions.
            output.push_str(&format!("\n{}: {}\n", category.name, category.summary));
            for def in &definitions {
                let Some(name) = def["function"]["name"].as_str() else { continue };
                if category.tools.contains(&name) {
                    shown.insert(name.to_string());
                }
            }
        }
        if topic.is_empty() {
            let other: Vec<_> = definitions.iter().filter_map(|def| {
                let name = def["function"]["name"].as_str()?;
                (!shown.contains(name)).then(|| name.trim_start_matches("skill_").replace('_', " "))
            }).collect();
            if !other.is_empty() { output.push_str(&format!("\nAlso installed: {}.\n", other.join(", "))); }
        }
        if topic.is_empty() || ["protocol", "macro", "voice", "skill"].iter().any(|word| topic.contains(word)) {
            let protocols = self.protocols.list();
            output.push_str(&format!("\nSaved Stark protocols: {}.\n", if protocols.is_empty() { "none".into() } else { protocols.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ") }));
            for item in self.stark.macros.list() {
                output.push_str(&format!("Voice macro '{}': triggers [{}]; steps: {}.\n", item.name, item.trigger_phrases.join(", "), item.steps.join(" → ")));
            }
            output.push_str("Welcome protocol triggers: wake up daddy's home; daddy's home; jai wake up; i'm home jai; i am back.\n");
        }
        if !topic.is_empty() && shown.is_empty() {
            output.push_str("No registered capability matched that topic. Ask for all features to see the complete registry.\n");
        }
        output
    }

    /// Stable order: memory, PC control, widgets/windows, self-improvement, skills. Stability
    /// keeps Ollama's prompt cache warm.
    pub fn definitions(&self) -> Vec<Value> {
        definitions_with(self.skills.tool_definitions())
    }

    /// What the model is shown: every tool but those behind the door, and the door.
    pub fn prompt_definitions(&self) -> Vec<Value> {
        prompt_definitions_from(self.definitions())
    }

    pub async fn execute(&self, name: &str, raw_args: &Value) -> ToolOutcome {
        // Some models send arguments as a JSON string.
        let mut args = match raw_args {
            Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            Value::Null => json!({}),
            other => other.clone(),
        };
        // A misspelled or invented tool name still reaches the right tool.
        let known: Vec<String> = self
            .definitions()
            .iter()
            .filter_map(|d| d["function"]["name"].as_str().map(String::from))
            .chain(["shutdown_jai", "create_skill", "execute_skill", "place_application_window"].map(String::from))
            .collect();
        let repaired = canonical_tool_name(name, &known);
        if let Some(fixed) = repaired.as_deref().filter(|f| *f != name) {
            warn!("[TOOL] '{name}' is not a tool; using '{fixed}'");
            if matches!(name.to_lowercase().as_str(), "set_timer" | "timer" | "start_timer") {
                let minutes = args.get("minutes").or_else(|| args.get("duration_minutes")).cloned();
                if let Some(obj) = args.as_object_mut() {
                    obj.entry("type").or_insert(json!("CountdownOrStopwatch"));
                    obj.entry("title").or_insert(json!("Timer"));
                    if let Some(m) = minutes {
                        obj.entry("minutes").or_insert(m);
                    }
                }
            }
        }
        let name = repaired.as_deref().unwrap_or(name);
        // Inside the live self-test's dry run the call is recorded, not carried out.
        if let Some(recorded) = crate::live_self_tester::intercept(name, &args) {
            return recorded;
        }
        // "Close up", "close the Earth, you": a filler names no program (only something of his),
        // and a view of his own closes as his own — whichever parser or model made the call.
        let (name, args) = match own_close(name, &args) {
            Some((tool, call)) => (tool, call),
            None => (name, args),
        };
        // "Close it" right after something of his own was opened or changed means that, not the
        // application Sir happens to be working in (2026-11-06: "open a window with my drive
        // space", "make it smaller", "close it" closed the program in front; the Theme Studio, a
        // timer or a formula sheet would have fallen through the same way).
        // (2026-11-09: "close that window" after a 3D model closed the Claude window in front.)
        let unnamed_window = name == "close_native_window" && str_arg(&args, &["id", "window_id", "title"]).is_none_or(|id| id == "last" || focus::is_pronoun(id));
        let own_thing = ((name == "close_application" && str_arg(&args, &["app_name", "name", "app", "application"]).is_some_and(focus::is_pronoun)) || unnamed_window)
            .then(|| self.own_thing_to_close())
            .flatten();
        let (name, args) = match own_thing {
            Some((tool, call)) => (tool, call),
            None => (name, args),
        };
        // "Make it full screen", "minimise it", "move it to the left" right after one of his own
        // views was opened, moved or arranged mean that view, not the app in front; "exit full
        // screen" means the view of his filling a screen (`window_commands::redirect`).
        let (name, args) = match crate::window_commands::redirect_for(self, name, &args) {
            Some(call) => {
                info!("[TOOL] {name} {args} → arrange_jai_window {call} (it is a view of his)");
                ("arrange_jai_window", call)
            }
            None => (name, args),
        };
        // Someone other than Sir (on his PC: what Sir allowed that account here), or whoever sits
        // at another PC of his (what its account may there). What an allowed call does in turn
        // (a view's reads, a protocol's steps) comes with it.
        if VOUCHED.try_with(|_| ()).is_err() && self.caller != crate::accounts::Caller::Remote {
            let why = match &self.caller {
                crate::accounts::Caller::Guest(guest) => crate::admin::guest_refusal(&self.config.read(|c| c.memory_db_path.clone()), guest, name, &args),
                _ => crate::admin::refusal(&self.config, name, &args),
            };
            if let Some(why) = why {
                info!("[TOOL] {name} {args} refused for {}: not allowed", self.caller);
                return ToolOutcome { tool: name.to_string(), args, output: format!("Error: {why}"), success: false, duration_ms: 0, memories_changed: false, skills_changed: false, shutdown_requested: false, modality_changed: false, state_changed: false };
            }
            if self.caller.guest().is_some() || crate::fleet::is_client(&self.config.read(|c| c.main_pc.clone())) {
                return VOUCHED.scope((), self.carry_out(name, args)).await;
            }
        }
        // (Sir's order from his main PC: a window it opens here is his while it is open.)
        if self.caller == crate::accounts::Caller::Remote {
            crate::admin::sirs_call(name, &args);
        }
        self.carry_out(name, args).await
    }

    /// A call that may be made, carried out.
    async fn carry_out(&self, name: &str, mut args: Value) -> ToolOutcome {
        // A silent scripted session acts inside JAI only.
        if silent_session() && !stays_inside(name, &args) {
            info!("[TOOL] {name} {args} (noted, not carried out: a silent session leaves the PC around JAI alone)");
            return ToolOutcome {
                tool: name.to_string(),
                args,
                output: NOT_CARRIED_OUT.to_string(),
                success: true,
                duration_ms: 0,
                memories_changed: false,
                skills_changed: false,
                shutdown_requested: false,
                modality_changed: false,
                state_changed: false,
            };
        }
        crate::mind_activity::pulse("tools.rs");
        if let Some(module) = crate::mind_activity::module_for_tool(name) {
            crate::mind_activity::pulse(module);
        }
        // What is his before the call: the way back from what the call changes ("undo that").
        let before = self.snapshot();
        let started = Instant::now();
        info!("[TOOL] {name} {args}");
        crate::hud_lines::step(name);
        let mut memories_changed = false;
        let mut skills_changed = false;
        let mut shutdown_requested = false;
        let mut modality_changed = false;
        let mut state_changed = false;

        let result: Result<String, String> = match name {
            "more_tools" => more_tools(&self.definitions(), &args),
            "list_jai_features" => Ok(self.feature_manifest(str_arg(&args, &["topic"]))),
            "open_jai_view" => crate::jai_views::run(self, &args).await,
            "manage_workout" => crate::workout_coach::run_tool(self, &args),
            name if crate::remote_guardian::is_tool(name) => crate::remote_guardian::run_tool(self, name, &args).await,
            name if crate::vram_guard::is_tool(name) => crate::vram_guard::run_tool(&args),
            name if crate::deep_research_engine::is_tool(name) => crate::deep_research_engine::run_tool(self, &args),
            name if crate::workspace_canvas::is_tool(name) => crate::workspace_canvas::run_tool(self, name, &args),
            name if crate::spatial_scenes::is_tool(name) => crate::spatial_scenes::run_tool(self, &args),
            name if crate::intent_engine::is_tool(name) => crate::intent_engine::run_tool(self, name, &args).await,
            name if crate::dynamic_popups::is_tool(name) => crate::dynamic_popups::run_tool(name, &args),
            name if crate::tool_forge::is_tool(name) => Box::pin(crate::tool_forge::run_tool(self, name, &args)).await,
            name if crate::hud_board::is_tool(name) => crate::hud_board::run_tool(self, &args).await,
            name if crate::window_commands::is_tool(name) => Box::pin(crate::window_commands::run_tool(self, &args)).await,
            name if crate::venue_guide::is_tool(name) => Box::pin(crate::venue_guide::run_tool(self, &args)).await,
            name if crate::discord_bridge::is_tool(name) => Box::pin(crate::discord_bridge::run_tool(self, &args)).await,
            name if crate::discord_bot::is_tool(name) => Box::pin(crate::discord_bot::run_tool(self, &args)).await,
            name if crate::away_mode::is_tool(name) => Box::pin(crate::away_mode::run_tool(self, &args)).await,
            name if crate::watch_and_tell::is_tool(name) => Box::pin(crate::watch_and_tell::run_tool(self, &args)).await,
            name if crate::kit::is_tool(name) => Box::pin(crate::kit::run_tool(self, name, &args)).await,
            name if crate::shadow_memory::is_tool(name) => Box::pin(crate::shadow_memory::run_tool(self, &args)).await,
            name if crate::aegis_server_deck::is_tool(name) => Box::pin(crate::aegis_server_deck::run_tool(self, &args)).await,
            name if crate::llm_reboot::is_tool(name) => crate::llm_reboot::run_tool().await,
            name if crate::image_viewer::is_tool(name) => Box::pin(crate::image_viewer::run_tool(self, &args)).await,
            name if crate::webcams::is_tool(name) => Box::pin(crate::webcams::run_tool(self, &args)).await,
            name if crate::street_view::is_tool(name) => Box::pin(crate::street_view::run_tool(self, &args)).await,
            name if crate::model3d::is_tool(name) => Box::pin(crate::model3d::run_tool(self, &args)).await,
            "remember_fact" => match (str_arg(&args, &["key", "name"]), str_arg(&args, &["value", "fact"])) {
                (key, Some(value)) => self
                    .memory
                    .remember_fact(key.unwrap_or(""), value, str_arg(&args, &["category"]), int_arg(&args, "importance").map(|i| i as i32))
                    .map(|o| {
                        memories_changed = true;
                        // "Call me Max": the title he uses on every line from now on.
                        if crate::memory::is_address_key(&o.entry.key) && o.entry.value.split_whitespace().count() <= 4 {
                            self.config.update(|c| c.interface.address = Some(o.entry.value.clone()));
                        }
                        // (What it was, with its kind, is what "undo that" puts back: `undo::way_back`.)
                        match (o.previous_value, o.previous_kind) {
                            (Some(prev), Some((category, importance))) => format!("Updated memory '{}' (was: \"{prev}\" [{category}, {importance}]): {}", o.entry.key, o.entry.value),
                            (Some(prev), None) => format!("Updated memory '{}' (was: \"{prev}\"): {}", o.entry.key, o.entry.value),
                            _ if o.existed => format!("Kept memory '{}' [{}]: {}", o.entry.key, o.entry.category, o.entry.value),
                            _ => format!("Stored memory '{}' [{}]: {}", o.entry.key, o.entry.category, o.entry.value),
                        }
                    })
                    .map_err(|e| e.to_string()),
                _ => Err("remember_fact needs a 'value'.".into()),
            },
            "recall_memories" => self
                .memory
                .recall(str_arg(&args, &["query", "q"]), str_arg(&args, &["category"]), 12)
                .map(|items| {
                    if items.is_empty() {
                        "No matching memories.".to_string()
                    } else {
                        items
                            .iter()
                            .map(|m| format!("- {} [{}]: {}", m.key, m.category, m.value))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                })
                .map_err(|e| e.to_string()),
            "delete_memory" => match str_arg(&args, &["id_or_key", "key", "id"]) {
                Some(target) => match self.memory.delete(target) {
                    Ok(DeleteOutcome::Deleted(v)) => {
                        memories_changed = true;
                        Ok(format!(
                            "Deleted memory: {}",
                            v.iter().map(|m| format!("{} ({})", m.key, m.value)).collect::<Vec<_>>().join(", ")
                        ))
                    }
                    Ok(DeleteOutcome::Ambiguous(v)) => Err(format!(
                        "Several memories match '{target}' ({}); ask Sir which one to forget.",
                        v.iter().map(|m| m.key.as_str()).collect::<Vec<_>>().join(", ")
                    )),
                    Ok(DeleteOutcome::NotFound) => Err(format!("No memory matches '{target}'.")),
                    Err(e) => Err(e.to_string()),
                },
                None => Err("delete_memory needs 'id_or_key'.".into()),
            },

            "create_skill" => self.create_skill(&args).await.inspect(|_| skills_changed = true),
            "create_persistent_skill" => self.create_persistent_skill(&args).await.inspect(|_| skills_changed = true),
            "execute_skill" | "run_custom_skill" => match str_arg(&args, &["name", "skill"]) {
                Some(n) => {
                    let skill_args = args.get("arguments").or_else(|| args.get("args")).cloned().unwrap_or_else(|| json!({}));
                    skills_changed = true;
                    self.skills.execute(n, &skill_args).await.map_err(|e| e.to_string())
                }
                None => Err(format!("{name} needs 'name'.")),
            },
            "list_skills" => self
                .skills
                .list()
                .map(|list| {
                    if list.is_empty() {
                        "No skills learned yet.".to_string()
                    } else {
                        list.iter()
                            .map(|s| format!("- {} ({}): {} [used {}x]", s.name, s.title, s.description, s.usage_count))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                })
                .map_err(|e| e.to_string()),
            "delete_skill" => match str_arg(&args, &["name"]) {
                Some(n) => self
                    .skills
                    .delete(n)
                    .map_err(|e| e.to_string())
                    .and_then(|count| {
                        if count > 0 {
                            skills_changed = true;
                            Ok(format!("Deleted skill '{n}'."))
                        } else {
                            Err(format!("No skill named '{n}'."))
                        }
                    }),
                None => Err("delete_skill needs 'name'.".into()),
            },

            // "Open that file (in VS Code)": the file the conversation is about.
            "open_application" if bool_arg(&args, "open_last_file") == Some(true) => match self.focus.last_file() {
                Some(path) => {
                    let with = str_arg(&args, &["with", "open_with"]);
                    file_ops::open_with(&path, with).await.inspect(|_| {
                        if let Some(w) = with.filter(|w| !w.eq_ignore_ascii_case("default")) {
                            self.focus.note_app(w);
                        }
                    })
                }
                None => Err("I haven't found or edited a file in this conversation yet.".into()),
            },
            "open_application" => match str_arg(&args, &["app_name", "name", "app", "application"]) {
                Some(app) => {
                    let extra: Vec<String> = args
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    apps::open(app, &extra).await.inspect(|_| self.focus.note_opened(app))
                }
                None => Err("open_application needs 'app_name'.".into()),
            },
            "open_website" => sites::open(&args).await.map(|o| {
                self.focus.note_site(&o.url);
                o.summary
            }),
            "close_application" => match str_arg(&args, &["app_name", "name", "app", "application"]) {
                Some(app) if names_jai(app) => {
                    shutdown_requested = true;
                    Ok("That is J.A.I. himself: he shuts down as soon as this reply has been spoken.".into())
                }
                // "Get rid of it" with nothing of his up: no program pays for that.
                Some(app) if focus::is_pronoun(app) && bool_arg(&args, "own_only") == Some(true) => Err("There is nothing of mine up to take away.".into()),
                // "Close it": the app (or the website's tab) the conversation is about.
                Some(app) if focus::is_pronoun(app) => self.close_focused(false).await,
                Some(app) => match apps::close(app, bool_arg(&args, "force").unwrap_or(true)).await {
                    Ok(out) => {
                        self.focus.forget_app(app);
                        Ok(out)
                    }
                    // No program by that name: a block or window of his own by that title is
                    // what Sir meant ("close the feature lab", a title read off the HUD).
                    Err(e) => self.close_own_named(app).ok_or(e),
                },
                None => Err("close_application needs 'app_name'.".into()),
            },
            "shutdown_jai" => {
                shutdown_requested = true;
                Ok("J.A.I. shuts down as soon as this reply has been spoken.".into())
            }
            "control_jai_lifecycle" => {
                let action = str_arg(&args, &["action", "command"]).unwrap_or("").to_lowercase().replace([' ', '-'], "_");
                match action.as_str() {
                    "shutdown_jai" | "shutdown" | "shut_down" | "quit" | "exit" | "close" | "power_off" => {
                        shutdown_requested = true;
                        Ok("J.A.I. shuts down as soon as this reply has been spoken.".into())
                    }
                    "restart_jai" | "restart" | "reboot" | "reload" => {
                        lifecycle::request_restart(Restart::Same);
                        shutdown_requested = true;
                        Ok("J.A.I. restarts as soon as this reply has been spoken.".into())
                    }
                    "mute_tts" | "mute" | "mute_voice" => {
                        self.modality.set_voice_muted(true);
                        state_changed = true;
                        Ok("Voice muted for this session: replies appear in the chat only.".into())
                    }
                    "unmute_tts" | "unmute" | "unmute_voice" => {
                        self.modality.set_voice_muted(false);
                        state_changed = true;
                        Ok("Voice restored.".into())
                    }
                    "sleep_mode" | "sleep" | "standby" => {
                        lifecycle::set_sleeping(true);
                        // "Go to sleep": the GPU goes back to Sir at once.
                        crate::vram_guard::flush_with_reason("sleep mode");
                        state_changed = true;
                        Ok("Sleep mode: J.A.I. ignores everything except \"wake up\".".into())
                    }
                    "wake_up" | "wake" | "awake" => {
                        lifecycle::set_sleeping(false);
                        state_changed = true;
                        Ok("Awake again.".into())
                    }
                    other => Err(format!(
                        "Unknown lifecycle action '{other}' (shutdown_jai, restart_jai, mute_tts, unmute_tts, sleep_mode, wake_up)."
                    )),
                }
            }
            "set_response_modality" => {
                let mode = str_arg(&args, &["mode", "modality"]).unwrap_or("mirror_input");
                let condition = str_arg(&args, &["custom_condition", "condition", "rule"]);
                match VoiceResponseMode::from_parts(mode, condition) {
                    Some(m) => {
                        self.modality.set(m.clone());
                        modality_changed = true;
                        Ok(format!("Voice response mode is now {}.", m.display()))
                    }
                    None => Err(format!("Unknown response mode '{mode}' (mirror_input, always_speak, text_only, custom_condition).")),
                }
            }
            "undo_last_action" => self.undo_last(int_arg(&args, "within_s").filter(|s| *s > 0).map(|s| Duration::from_secs(s as u64))).await,
            "recent_actions" => Ok(match str_arg(&args, &["about"]).filter(|about| !about.trim().is_empty()) {
                Some(about) => crate::undo::report_about(about),
                None => crate::undo::report_done(),
            }),
            "whats_new" => Ok(crate::release_notes::spoken()),
            "whats_open" => Ok(self.whats_open()),
            "timer_left" => Ok(self.timer_left()),
            "is_app_running" => match str_arg(&args, &["app_name", "name", "app"]) {
                Some(app) => {
                    let name = apps::display_name(app);
                    Ok(if apps::is_running(app).await { format!("{name} is running, Sir.") } else { format!("{name} isn't running, Sir.") })
                }
                None => Err("is_app_running needs app_name.".to_string()),
            },
            "create_or_update_widget" => self.widgets.create_or_update(&args).inspect(|_| gui::refresh()),
            "remove_widget" => self.widgets.remove_tool(&args).inspect(|_| gui::refresh()),
            "spawn_native_window" => {
                // A content-less "Earth Image" / "Webcam" window: the picture, camera or street view.
                if let Some((tool, routed)) = crate::image_viewer::reroute_window(&args) {
                    return Box::pin(self.execute(tool, &routed)).await;
                }
                if gui::windows_supported() {
                    self.widgets.spawn_window(&args).inspect(|out| {
                        self.focus.note_jai_window(quoted(out));
                        gui::sync_windows();
                    })
                } else {
                    Err("JAI is running without a display (headless), so he cannot open windows.".into())
                }
            }
            "update_native_window" => {
                // "That window", "it": the JAI window the conversation is about, else the newest.
                let raw = str_arg(&args, &["id", "window_id"]).unwrap_or("last").to_string();
                let pronoun = raw == "focus" || focus::is_pronoun(&raw);
                // "Make it bigger" right after moving Discord: Discord, not one of JAI's windows.
                let size = str_arg(&args, &["size"]).map(str::to_lowercase);
                if let (true, Some(size), None, Some(_)) = (pronoun, size.as_deref(), self.focus.jai_window_is_latest(), self.focus.focused_app()) {
                    let action = if size.contains("small") || size.contains("klein") || size == "half" { WindowAction::Smaller } else { WindowAction::Bigger };
                    return Box::pin(self.execute("window_management", &json!({ "action": if action == WindowAction::Bigger { "bigger" } else { "smaller" } }))).await;
                }
                if pronoun {
                    let id = self
                        .focus
                        .snapshot()
                        .last_active_jai_window_id
                        .filter(|id| self.widgets.find_window_named(id).is_some())
                        .unwrap_or_else(|| "last".into());
                    if let Some(o) = args.as_object_mut() {
                        o.insert("id".into(), json!(id));
                    }
                }
                self.widgets.update_window(&args).inspect(|out| {
                    self.focus.note_jai_window(quoted(out));
                    gui::sync_windows();
                })
            }
            "close_native_window" => {
                let id = str_arg(&args, &["id", "window_id", "title"]).unwrap_or("last");
                let fallback = bool_arg(&args, "fallback_to_active_window").unwrap_or(false);
                // "Close that window" right after "open Discord" means Discord's window; JAI's
                // own window only when it was the latest thing in focus.
                if fallback && id == "last" && self.focus.jai_window_is_latest().is_none() && self.focus.focused_app().is_some() {
                    self.close_focused(true).await
                } else {
                    match self.widgets.close_window(id) {
                        Ok(out) => {
                            self.focus.forget_jai_window(id);
                            gui::refresh();
                            Ok(out)
                        }
                        // No JAI window open: the window Sir is working in (never the HUD).
                        Err(_) if fallback => self.close_focused(true).await,
                        Err(e) => Err(e),
                    }
                }
            }
            "customize_hud_and_windows" => self.customize(&args),
            "inspect_own_source" => self_improve::inspect(&args),
            "patch_own_source" => self_improve::patch(&args).await,
            "restore_source_backup" => self_improve::restore_backup(&args),
            "recompile_and_restart" => self_improve::recompile_and_restart(&args),
            "configure_interface" => self.configure_interface(&args),
            "control_jai_interface" => self.control_interface(&args),
            "control_orb_mode" => self.orb_mode(&args),
            "control_audio_mixer" => crate::audio_mixer::tool(&args).await,
            "set_listening_mode" => {
                let mode = str_arg(&args, &["mode", "listening_mode"]).unwrap_or("").to_lowercase().replace([' ', '-'], "_");
                let required = match mode.as_str() {
                    "hands_free" | "handsfree" | "always" | "open" | "freisprech" | "freisprechmodus" => Some(false),
                    "wake_word" | "wakeword" | "wake" | "keyword" | "only_jai" | "aktivierungswort" => Some(true),
                    _ => bool_arg(&args, "wake_word_required"),
                };
                match required {
                    Some(required) => {
                        self.config.update(|c| c.wake_word_required = required);
                        state_changed = true;
                        Ok(if required {
                            "Listening mode: WAKE-WORD. JAI only answers when Sir says \"JAI\" (anywhere in the sentence).".to_string()
                        } else {
                            "Listening mode: HANDS-FREE. JAI answers relevant speech without the wake word; background talk, humming and fragments are ignored.".to_string()
                        })
                    }
                    None => Err("set_listening_mode needs mode: hands_free or wake_word.".into()),
                }
            }
            "window_management" => match str_arg(&args, &["action"]).and_then(WindowAction::parse) {
                Some(action) => {
                    let named = str_arg(&args, &["target_window", "target", "app_name"]);
                    let unnamed = named.is_none_or(focus::is_pronoun);
                    if named.is_some_and(names_jai) {
                        // "Maximize JAI", "fullscreen yourself": only when Sir says so.
                        self.hud_window(action)
                    } else if action == WindowAction::ExitFullscreen && unnamed && self.config.read(|c| c.interface.fullscreen) {
                        // JAI is the one in full screen.
                        self.hud_window(action)
                    } else {
                        self.window_action(action, named).await
                    }
                }
                None => Err("window_management needs a valid 'action'.".into()),
            },
            "media_and_volume_control" => self.media(&args).await,
            "get_system_status" => Ok(self.monitor.latest().summary()),
            "get_current_datetime" => {
                let now = chrono::Local::now();
                Ok(format!("Local time {} on {}.", now.format("%H:%M"), now.format("%A, %-d %B %Y")))
            }
            "run_shell_command" => match str_arg(&args, &["command", "cmd"]) {
                Some(c) => shell::run(c).await.map(|r| match r.exit_code {
                    Some(0) => r.output,
                    code => format!("(exit code {}) {}", code.unwrap_or(-1), r.output),
                }),
                None => Err("run_shell_command needs 'command'.".into()),
            },
            "web_search" => match str_arg(&args, &["query", "q", "search"]) {
                Some(q) => {
                    // Weather without a place means Sir's home town, when memory knows it.
                    websearch::set_home_location(websearch::place_from_memories(&self.memory.list(200).unwrap_or_default()));
                    // In a game "how do I beat this boss?" is about that game: its name goes
                    // with the search, and the sources stay on the board for later.
                    match crate::kit_games::with_game(q) {
                        Some((query, game)) => {
                            let out = websearch::search(&query).await;
                            if let Ok(found) = &out {
                                crate::kit::show_text("games_wiki", &format!("GAME WIKI // {}", game.to_uppercase()), &found.chars().take(1800).collect::<String>(), [460.0, 340.0]);
                            }
                            out
                        }
                        None => websearch::search(q).await,
                    }
                }
                None => Err("web_search needs 'query'.".into()),
            },
            "reminders" => {
                let out = self.reminders.tool(&args);
                gui::refresh();
                out
            }
            "calculate" => match str_arg(&args, &["expression", "query", "input", "math"]) {
                Some(e) => calc::tool(e),
                None => Err("calculate needs 'expression'.".into()),
            },
            "manage_stark_protocol" | "protocols" => {
                let (out, effects) = self.protocols_tool(&args).await;
                // (A workspace just captured is a desk layout by name at once.)
                crate::kit_screen::sync_layouts(self);
                modality_changed |= effects.0;
                state_changed |= effects.1;
                shutdown_requested |= effects.2;
                out
            }
            "focus_mode" => {
                let action = str_arg(&args, &["action"]).unwrap_or("start").to_lowercase();
                modality_changed = true;
                if matches!(action.as_str(), "stop" | "end" | "cancel") {
                    self.protocols.end_focus(&self.modality, &self.widgets).ok_or_else(|| "Focus mode is not active.".to_string())
                } else {
                    let minutes = args.get("minutes").and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok())).unwrap_or(25.0);
                    self.protocols.start_focus(minutes, &self.modality, &self.widgets).inspect(|_| gui::refresh())
                }
            }
            "clipboard" => clipboard::tool(&args).await,
            "read_screen" => screen::read(screen::Scope::parse(str_arg(&args, &["scope", "target"]).unwrap_or("active_window"))).await,
            "inspect_screen_or_selection" => screen::inspect(&args, self.focus.foreground().map(|w| w.handle).filter(|h| *h != 0)).await,
            "control_music" => self.music.control(&args).await.inspect(|_| {
                if matches!(str_arg(&args, &["action"]).unwrap_or("play"), "play" | "start" | "resume") {
                    self.focus.note_app("spotify");
                }
            }),
            "move_window_to_monitor" => self.move_to_monitor(&args).await,
            "place_application_window" => self.place_app(&args).await,
            "find_files" => file_ops::find_tool(&args).await.map(|o| {
                if let Some(p) = &o.path {
                    self.focus.note_file(p);
                }
                o.text
            }),
            "search_inside_files" => file_ops::grep_tool(&args, self.focus_dir()).await,
            "read_text_file" => file_ops::read_tool(&args, self.focus_dir().as_deref()).await.map(|o| {
                if let Some(p) = &o.path {
                    self.focus.note_file(p);
                }
                o.text
            }),
            "create_or_overwrite_file" => self.after_file_write(file_ops::write_tool(&args, self.focus_dir().as_deref()).await, &args),
            "edit_file_surgical" => self.after_file_write(file_ops::edit_tool(&args, self.focus_dir().as_deref()).await, &args),
            "undo_last_file_edit" => self.after_file_write(file_ops::undo_tool(&args, self.focus.last_file()).await, &args),
            "search_conversation" => memory::conversation_tool(&self.memory, &args),
            "network_status" => Ok(diagnostics::network_status(&self.monitor.latest()).await),
            "top_processes" => {
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5).clamp(1, 15) as usize;
                tokio::task::spawn_blocking(diagnostics::top_processes)
                    .await
                    .map(|list| diagnostics::processes_report(&list, limit))
                    .map_err(|e| e.to_string())
            }
            "read_webpage" => match str_arg(&args, &["url", "link", "address"]) {
                Some(u) => websearch::read_webpage(u).await,
                None => Err("read_webpage needs 'url'.".into()),
            },
            pim_tool if pim::is_pim_tool(pim_tool) => pim::run_tool(self, pim_tool, &args).await,
            stark_tool if crate::stark::is_stark_tool(stark_tool) => crate::stark::run_tool(self, stark_tool, &args).await,
            other => {
                let skill_name = other.strip_prefix("skill_").unwrap_or(other);
                match self.skills.get(skill_name) {
                    Ok(Some(_)) => {
                        skills_changed = true;
                        self.skills.execute(skill_name, &args).await.map_err(|e| e.to_string())
                    }
                    // (Said so that the model's next move is the right one: it invented
                    // `translate_text` for "translate good morning into French" and, told only
                    // "unknown tool", wished Sir a good morning instead.)
                    _ => Err(format!("Unknown tool '{other}': there is no such tool. Do not call a tool for this. Answer Sir's request yourself, directly, from your own knowledge (a translation, a definition, a fact, an explanation).")),
                }
            }
        };

        // (An online read keeps its answer; without internet the last one stands in, with its time.)
        let result = crate::diagnostics::kept_answer(name, &args, result);
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(output) => {
                info!("[TOOL] {name} succeeded in {duration_ms} ms");
                let outcome = ToolOutcome {
                    tool: name.to_string(),
                    args,
                    output,
                    success: true,
                    duration_ms,
                    memories_changed,
                    skills_changed,
                    shutdown_requested,
                    modality_changed,
                    state_changed,
                };
                // What "that", "him" and "the day after" refer to from now on.
                self.stark.graph.observe_tool(&outcome, self.focus.focused_app());
                // … what "close it" means, what "undo that" takes back, and what "what did you
                // just do?" tells.
                let after = self.snapshot();
                self.note_what_came_up(name, &outcome.args, &outcome.output, &before, &after);
                if let Some(step) = crate::undo::way_back(name, &outcome.args, &outcome.output, &before, &after) {
                    crate::undo::keep(step);
                }
                if !crate::hyper_cortex::is_read_only(name, &outcome.args) {
                    crate::undo::note_done(&outcome.output);
                    // … and what "it" means for a moment: "throw it on all monitors" right
                    // after the live background was switched on (`just_did`).
                    crate::just_did::note(name, &outcome.args);
                }
                outcome
            }
            Err(err) => {
                warn!("[TOOL] {name} failed in {duration_ms} ms: {err}");
                crate::hud_lines::fault();
                // (A tool behind the door was called unseen: its parameters, for the next try.)
                let door = if BEHIND_DOOR.contains(&name) && first_failure(name) { more_tools(&self.definitions(), &json!({"names": [name]})).map(|p| format!(" {p}")).unwrap_or_default() } else { String::new() };
                ToolOutcome {
                    tool: name.to_string(),
                    args,
                    output: format!("Error: {err}{door}"),
                    success: false,
                    duration_ms,
                    memories_changed: false,
                    skills_changed: false,
                    shutdown_requested: false,
                    modality_changed: false,
                    state_changed: false,
                }
            }
        }
    }

    /// What this call put up of his own is what "close it" means from now on: a window, whichever
    /// tool opened it (a formula sheet, a report), a widget, a view of the HUD.
    fn note_what_came_up(&self, name: &str, args: &Value, said: &str, before: &crate::undo::Snapshot, after: &crate::undo::Snapshot) {
        if let Some(window) = after.windows.iter().rev().find(|w| !before.windows.contains(w)) {
            self.focus.note_jai_window(&window.id);
        }
        if name == "create_or_update_widget" && (said.starts_with("Created widget") || said.starts_with("Updated widget")) {
            self.focus.note_own_thing(OwnThing::Widget(quoted(said).to_string()));
        }
        if name == "configure_interface" {
            for key in ["core_view", "feature_lab", "theme_studio", "mixer_view", "text_console"] {
                match args.get(key).and_then(Value::as_bool) {
                    Some(true) => self.focus.note_own_thing(OwnThing::View(key.to_string())),
                    Some(false) => self.focus.forget_own_thing(&OwnThing::View(key.to_string())),
                    None => {}
                }
            }
        }
        // The Earth, the radar, the disk map: "close it" / "close up" means that panel now.
        if name == "open_jai_view" {
            let view = str_arg(args, &["view", "window_kind", "name", "target"]).and_then(crate::jai_views::JaiSubWindow::parse);
            if let Some(id) = view.map(|v| v.id()).filter(|id| crate::jai_views::is_panel(id)) {
                self.focus.note_own_thing(OwnThing::Panel(id.to_string()));
            }
        }
        // So do the 3D studio, a picture, a live camera and street view.
        let panel = match name {
            "show_3d_model" => Some("model3d"),
            "show_image" => Some("image_viewer"),
            "show_live_camera" => Some("webcams"),
            "show_street_view" => Some("street_view"),
            _ => None,
        };
        if let Some(id) = panel.filter(|id| crate::jai_views::panel_is_open(id)) {
            self.focus.note_own_thing(OwnThing::Panel(id.to_string()));
        }
    }

    /// A block, panel or window of his own called `name`, closed; what closed (None: nothing of
    /// his is called that).
    fn close_own_named(&self, name: &str) -> Option<String> {
        let name = crate::intent_engine::own_view_name(&crate::intent_engine::strip_close_fillers(&name.to_lowercase()));
        if name.trim().len() < 3 {
            return None;
        }
        let mut closed = crate::hud_dock::close_named(&name);
        if closed.is_empty() {
            closed.extend(crate::jai_views::close_panel(&name).map(str::to_string));
        }
        if closed.is_empty() {
            if let Ok(out) = self.widgets.close_window(&name) {
                self.focus.forget_jai_window(&name);
                closed.push(out);
            }
        }
        (!closed.is_empty()).then(|| format!("Closed: {}", closed.join(", ")))
    }

    /// The call that closes what "it" means, when that is something of his still up.
    fn own_thing_to_close(&self) -> Option<(&'static str, Value)> {
        let thing = self.focus.own_thing_is_latest().and_then(|thing| match thing {
            OwnThing::View(key) => gui::view_is_up(&key).then(|| ("configure_interface", json!({ key: false }))),
            OwnThing::Widget(id) => self.widgets.widgets().iter().any(|w| w.id == id).then(|| ("remove_widget", json!({ "id": id }))),
            // A panel opened within the last ten minutes and still up.
            OwnThing::Panel(id) => (crate::jai_views::panel_is_open(&id) && self.focus.own_thing_age().is_some_and(|age| age < Duration::from_secs(10 * 60)))
                .then(|| ("hud_board", json!({ "action": "close", "target": id.replace('_', " ") }))),
        });
        thing.or_else(|| {
            let id = self.focus.jai_window_is_latest().filter(|id| self.widgets.find_window_named(id).is_some())?;
            Some(("close_native_window", json!({ "id": id })))
        })
    }

    /// "What's open?": what of his is up right now, by name, each thing once.
    fn whats_open(&self) -> String {
        let mut seen: Vec<String> = Vec::new();
        let mut named = |names: Vec<String>| -> Vec<String> {
            let fresh: Vec<String> = names.into_iter().filter(|n| !n.trim().is_empty() && !seen.contains(n)).collect();
            seen.extend(fresh.iter().cloned());
            fresh
        };
        let mut lines: Vec<String> = Vec::new();
        if let crate::hud_board::ScreenState::BoardActive { topic, block_count, .. } = crate::hud_board::screen_state() {
            lines.push(format!("The board is up on {topic}, with {block_count} block{}.", if block_count == 1 { "" } else { "s" }));
        }
        for (what, names) in [
            ("Blocks", named(crate::hud_dock::open_titles())),
            ("Windows", named(self.widgets.windows().into_iter().map(|w| w.title).collect())),
            ("On the widgets panel", named(self.widgets.widgets().into_iter().map(|w| w.title).collect())),
        ] {
            if !names.is_empty() {
                lines.push(format!("{what}: {}.", names.join(", ")));
            }
        }
        if lines.is_empty() {
            return "Nothing of mine is open, Sir: no board, no blocks, no windows and no widgets.".to_string();
        }
        lines.join(" ")
    }

    /// "How long is left on the timer?": the latest timer, or else the latest stopwatch, off the
    /// widgets panel.
    fn timer_left(&self) -> String {
        let now = crate::widgets::now_ms();
        let clocks: Vec<(i64, Option<i64>)> = self.widgets.widgets().iter().filter_map(|w| crate::widgets::timer_state(&w.kind, now)).collect();
        match clocks.iter().rev().find(|(_, left)| left.is_some()).or(clocks.last()) {
            None => "There's no timer on the panel, Sir.".to_string(),
            Some((_, Some(left))) if *left <= 0 => "The timer has finished, Sir.".to_string(),
            Some((_, Some(left))) => format!("{} left on the timer, Sir.", spoken_span(*left)),
            Some((elapsed, None)) => format!("The stopwatch is at {}, Sir.", spoken_span(*elapsed)),
        }
    }

    /// What is his right now: the interface, his windows and widgets, the mixer's change count.
    fn snapshot(&self) -> crate::undo::Snapshot {
        crate::undo::Snapshot {
            interface: self.config.read(|c| c.interface.clone()),
            windows: self.widgets.windows(),
            widgets: self.widgets.widgets(),
            volume_changes: crate::audio_mixer::changes(),
            volume_undoable: crate::audio_mixer::can_undo(),
        }
    }

    /// "Undo that": the last change of his own, taken back (`undo`). With nothing noted, an
    /// edit of the file the conversation is about is still undone the old way.
    /// `within`: only a change that recent ("that's not what I said" means the request just
    /// misheard, never an older one; nor a file edit).
    async fn undo_last(&self, within: Option<Duration>) -> Result<String, String> {
        let step = match within {
            Some(max) => crate::undo::take_within(max),
            None => crate::undo::take(),
        };
        let Some(step) = step else {
            if within.is_some() {
                return Err("Nothing of mine to take back: nothing changed on that request.".to_string());
            }
            if self.focus.last_file().is_some() {
                let quietly = json!({ "preview": "none" });
                return self.after_file_write(file_ops::undo_tool(&quietly, self.focus.last_file()).await, &quietly).map(|out| format!("Undone: that edit is undone. {out}"));
            }
            return Err("Nothing of mine to take back: the last change is too long ago, or there was none.".to_string());
        };
        crate::undo::set_undoing(true);
        let mut said: Vec<String> = Vec::new();
        if let Some(settings) = step.interface {
            self.config.update(|c| c.interface = settings);
            gui::show();
            gui::refresh();
        }
        let back = self.widgets.put_back(step.windows, step.widgets);
        if !back.is_empty() {
            gui::sync_windows();
            gui::refresh();
            said.push(format!("Back: {}.", back.join(", ")));
        }
        let mut failed = None;
        for (tool, args) in step.calls {
            let outcome = Box::pin(self.execute(tool, &args)).await;
            if !outcome.success {
                failed = Some(outcome.output.trim_start_matches("Error: ").to_string());
                break;
            }
            said.push(outcome.output);
        }
        crate::undo::set_undoing(false);
        match failed {
            Some(why) => Err(format!("I could not take all of it back: {why}")),
            None => Ok(format!("Undone: {}. {}", step.done, said.join(" ")).trim().to_string()),
        }
    }

    fn configure_interface(&self, args: &Value) -> Result<String, String> {
        let current = self.config.read(|c| c.interface.clone());
        interface::configure(&current, args).and_then(|done| {
            if done.settings != current {
                self.config.update(|c| c.interface = done.settings.clone());
            }
            if bool_arg(args, "open_settings") == Some(true) {
                gui::open_interface_settings();
            }
            crate::hud_feel::apply(args);
            // "Subtitles on", "show my words" while the privacy lock is on: Sir's explicit word
            // that he wants to see what is heard (it stays hidden under the lock otherwise).
            if bool_arg(args, "subtitles") == Some(true) || bool_arg(args, "live_words") == Some(true) {
                crate::listening_lock::show_asked();
            }
            if let Some(open) = bool_arg(args, "text_console") {
                gui::set_text_console(open);
            }
            if let Some(open) = bool_arg(args, "core_view") {
                gui::core_command(crate::holo_mind::CoreCommand::Open(open));
            }
            if let Some(open) = bool_arg(args, "feature_lab") {
                gui::show_features(open);
            }
            if let Some(open) = bool_arg(args, "theme_studio") {
                gui::show_theme_studio(open);
            }
            if let Some(open) = bool_arg(args, "mixer_view") {
                gui::show_mixer(open);
            }
            if let Some(cmd) = interface::core_command(args) {
                gui::core_command(cmd);
            }
            if let Some(mode) = str_arg(args, &["hud_view"]).and_then(crate::hud_dock::HudViewMode::parse) {
                crate::hud_dock::request_view(mode);
            }
            match done.window {
                Some(command) => gui::window_command(command)?,
                // Restyling brings the HUD up so Sir sees the result.
                None => gui::show(),
            }
            Ok(done.summary)
        })
    }

    /// `control_jai_interface`: the orb, the text and the window, each resized live.
    /// The window picks the new settings up on its next frame.
    fn control_interface(&self, args: &Value) -> Result<String, String> {
        let current = self.config.read(|c| c.interface.clone());
        let done = interface::control(&current, args)?;
        if done.settings != current {
            self.config.update(|c| c.interface = done.settings.clone());
        }
        if let Some((width, height)) = done.window_size {
            gui::set_window_size(width, height)?;
        }
        // Sir sees the result: the HUD comes up (a no-op when it is already in front).
        gui::show();
        gui::refresh();
        Ok(done.summary)
    }

    /// `control_orb_mode`: pure orb mode (only the 3D orb, floating), its size, corner, monitor
    /// and opacity. The HUD window re-shapes itself on its next frame.
    fn orb_mode(&self, args: &Value) -> Result<String, String> {
        let current = self.config.read(|c| c.interface.clone());
        let done = interface::control_orb_mode(&current, args)?;
        if done.settings != current {
            self.config.update(|c| c.interface = done.settings.clone());
        }
        gui::show();
        gui::refresh();
        Ok(done.summary)
    }

    /// A window action on the HUD itself: only when Sir named JAI ("maximize JAI",
    /// "fullscreen yourself"), or to leave a full screen JAI himself is in.
    fn hud_window(&self, action: WindowAction) -> Result<String, String> {
        let args = match action {
            WindowAction::Fullscreen => json!({ "fullscreen": true }),
            WindowAction::ExitFullscreen => json!({ "fullscreen": false }),
            WindowAction::Minimize => json!({ "window": "minimize" }),
            WindowAction::Maximize => json!({ "window": "maximize" }),
            WindowAction::Restore => json!({ "window": "restore" }),
            WindowAction::Focus => json!({ "window": "show" }),
            WindowAction::Close | WindowAction::CloseTab => json!({ "window": "hide" }),
            WindowAction::SnapLeft => json!({ "window": "left" }),
            WindowAction::SnapRight => json!({ "window": "right" }),
            WindowAction::SnapTop | WindowAction::SnapBottom => json!({ "window": "center" }),
            WindowAction::Bigger => json!({ "window": "bigger" }),
            WindowAction::Smaller => json!({ "window": "smaller" }),
            WindowAction::MinimizeAll | WindowAction::ShowDesktop => return Err("That acts on the whole desktop, not on me.".into()),
        };
        self.configure_interface(&args)
    }

    /// A window action on an app. A name Sir said wins; otherwise the conversation focus decides:
    /// the app it is about, then the window Sir was working in before this request, then the one
    /// he is in now. Never JAI himself.
    async fn window_action(&self, action: WindowAction, named: Option<&str>) -> Result<String, String> {
        if action.is_global() {
            return winctl::manage(action, WindowTarget::Active).await;
        }
        if let Some(name) = named.filter(|n| !focus::is_pronoun(n)) {
            let out = winctl::manage(action, WindowTarget::Named(name.to_string())).await;
            if out.is_ok() {
                if action == WindowAction::Close {
                    self.focus.forget_app(name);
                } else {
                    self.focus.note_app(name);
                }
            }
            return out;
        }
        let mut last_err = "There is no window to act on.".to_string();
        for target in self.focus.window_targets() {
            info!("[FOCUS] {action:?} → {}", target.describe());
            match self.manage_waiting(action, &target).await {
                Ok(out) => {
                    if let (WindowAction::Close, WindowTarget::Focus(app)) = (action, &target) {
                        self.focus.forget_app(app);
                    }
                    return Ok(out);
                }
                // The conversation is about this app: never act on some other window instead.
                Err(e) if matches!(target, WindowTarget::Focus(_)) => return Err(e),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// Right after "open Discord" its window may still be on its way: wait a few seconds for it.
    async fn manage_waiting(&self, action: WindowAction, target: &WindowTarget) -> Result<String, String> {
        let just_opened = matches!(target, WindowTarget::Focus(_)) && self.focus.focused_app_age().is_some_and(|a| a < Duration::from_secs(20));
        let deadline = Instant::now() + Duration::from_secs(if just_opened { 8 } else { 0 });
        loop {
            match winctl::manage(action, target.clone()).await {
                Err(e) if e.starts_with("No open window") && Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(500)).await,
                other => return other,
            }
        }
    }

    /// "Close it": the tab of a website just opened, the app the conversation is about (quit, or
    /// only its window with `window_only`), otherwise the window Sir was working in.
    async fn close_focused(&self, window_only: bool) -> Result<String, String> {
        if self.focus.website_is_latest() {
            if let Ok(out) = winctl::manage(WindowAction::CloseTab, WindowTarget::Focus("browser".into())).await {
                self.focus.forget_app("browser");
                return Ok(format!("{out} (the tab that was just opened)"));
            }
        }
        if !window_only {
            if let Some(app) = self.focus.focused_app().filter(|a| a != "browser" && a != "explorer") {
                return apps::close(&app, true).await.inspect(|_| self.focus.forget_app(&app));
            }
        }
        self.window_action(WindowAction::Close, None).await
    }

    /// The folder of the file the conversation is about.
    fn focus_dir(&self) -> Option<PathBuf> {
        self.focus.last_file().map(|p| if p.is_dir() { p.clone() } else { p.parent().map(|d| d.to_path_buf()).unwrap_or(p) })
    }

    /// After a file write the file is the conversation focus and its diff appears in the HUD
    /// (`preview`: panel = a widget card (default), window = a separate window, none).
    fn after_file_write(&self, result: Result<EditOutcome, String>, args: &Value) -> Result<String, String> {
        let outcome = result?;
        self.focus.note_file(&outcome.path);
        let name = outcome.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let shown = outcome.path.display().to_string();
        let preview = match str_arg(args, &["preview"]).unwrap_or("panel").to_lowercase().as_str() {
            "none" | "off" | "false" | "no" => Ok(String::new()),
            "window" | "separate" | "new_window" if gui::windows_supported() => self
                .widgets
                .spawn_window(&json!({
                    "id": "file_edit", "title": format!("Edit: {name}"), "content_title": shown, "code": outcome.diff,
                    "language": "diff", "replace_content": true, "width": 680, "height": 480,
                }))
                .inspect(|_| gui::sync_windows()),
            _ => self
                .widgets
                .create_or_update(&json!({
                    "id": "file_edit_preview", "type": "MarkdownNoteCard", "title": format!("EDIT // {name}"),
                    "markdown": format!("`{shown}`\n```diff\n{}\n```", outcome.diff),
                }))
                .inspect(|_| gui::refresh()),
        };
        if let Err(e) = preview {
            warn!("[FILES] Diff preview failed: {e}");
        }
        Ok(outcome.summary)
    }

    /// `customize_hud_and_windows`: the main HUD (`target` hud) or JAI's extra windows (an id,
    /// a title, "last" or "all").
    fn customize(&self, args: &Value) -> Result<String, String> {
        let mut target = str_arg(args, &["target", "window", "window_id", "id"]).unwrap_or("hud").to_string();
        // "That window": the JAI window the conversation was last about.
        if matches!(target.as_str(), "focus" | "that" | "it") {
            target = self.focus.snapshot().last_active_jai_window_id.unwrap_or_else(|| "last".into());
        }
        let target = target.as_str();
        let hud = matches!(
            target.to_lowercase().as_str(),
            "hud" | "main" | "main hud" | "jai" | "yourself" | "interface" | "main window" | "the hud" | "gui"
        );
        if !hud {
            return self.widgets.style_window(target, args).inspect(|_| gui::sync_windows());
        }
        let mut hud_args = args.as_object().cloned().unwrap_or_default();
        hud_args.remove("target");
        self.configure_interface(&Value::Object(hud_args))
    }

    /// "Move Discord to monitor 2", "put the PC stats window on my left screen", "move yourself
    /// to the next monitor": JAI's own windows by id / title / live view, the HUD itself, or any
    /// application (named, or the one the conversation is about).
    async fn move_to_monitor(&self, args: &Value) -> Result<String, String> {
        let name = str_arg(args, &["app_or_window_name", "app_name", "window", "target_window", "name", "window_id"]).unwrap_or("it");
        let monitor = args.get("target_monitor").or_else(|| args.get("monitor")).cloned().unwrap_or_else(|| json!("next"));
        if names_jai(name) || matches!(name.to_lowercase().as_str(), "hud" | "the hud" | "your hud" | "your window" | "your interface") {
            return self.move_hud(&monitor).await;
        }
        // One of JAI's windows: named ("the drive space window", "pc_stats"), or "it" right
        // after it was opened.
        let jai_window = if focus::is_pronoun(name) {
            self.focus.jai_window_is_latest().and_then(|id| self.widgets.find_window_named(&id))
        } else {
            self.widgets.find_window_named(name).filter(|w| {
                let n = name.to_lowercase();
                // "Discord" is never a JAI window, even one titled after it by accident.
                n.contains("window") || n.contains("fenster") || w.id == n.replace(' ', "_") || widgets::BlockKind::live_from_name(n.trim_end_matches(" window")).is_some()
            })
        };
        if let Some(w) = jai_window {
            let out = self.widgets.update_window(&json!({ "id": w.id, "monitor": monitor }))?;
            self.focus.note_jai_window(&w.id);
            gui::sync_windows();
            let index = self.widgets.find_window_named(&w.id).and_then(|w| w.monitor).unwrap_or(1);
            return Ok(format!("Moved your window '{}' to Monitor {index}. {out}", w.title));
        }
        // One of his windows by its title (the text console, a report, the system column):
        // "No open window matches 'Text Console'" was the answer on 2026-11-14. A program of
        // that name comes first.
        if !focus::is_pronoun(name) && apps::find_spec(name).is_none() {
            if let Some(done) = crate::own_windows::to_monitor(name, &monitor) {
                return done;
            }
        }
        if !focus::is_pronoun(name) {
            let out = monitors::move_app(WindowTarget::Named(name.to_string()), monitor).await?;
            self.focus.note_app(name);
            return Ok(out);
        }
        let mut last_err = "There is no window to move.".to_string();
        for target in self.focus.window_targets() {
            match monitors::move_app(target.clone(), monitor.clone()).await {
                Ok(out) => return Ok(out),
                Err(e) if matches!(target, WindowTarget::Focus(_)) => return Err(e),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// The HUD itself to another monitor ("move yourself to monitor 2").
    async fn move_hud(&self, monitor: &Value) -> Result<String, String> {
        #[cfg(windows)]
        use crate::win32 as wm;
        #[cfg(not(windows))]
        use crate::x11 as wm;
        let hwnd = gui::hud_hwnd();
        if hwnd == 0 {
            return Err("My HUD window is not open.".into());
        }
        let monitors = monitors::list();
        // As the desktop's background he is on every monitor: his emblem goes ("the other
        // monitor": the other than the one it stands on).
        if crate::wallpaper::in_use() {
            let on = gui::emblem_monitor(&self.config, &monitors);
            let index = monitors::resolve(monitor, on, &monitors).ok_or_else(|| format!("There is no monitor {monitor}."))?;
            return gui::send_emblem_to_monitor(&self.config, &monitors, index);
        }
        let current = wm::window_display_rect(hwnd).and_then(|r| monitors::monitor_at(&monitors, (r[0] + r[2]) / 2, (r[1] + r[3]) / 2));
        let index = monitors::resolve(monitor, current, &monitors).ok_or_else(|| format!("There is no monitor {monitor}."))?;
        // The fullscreen board (also one that spans every monitor) goes there as the
        // fullscreen board; only a plain window is moved like any other window.
        if gui::send_board_to_monitor(&self.config, index) {
            return Ok(format!("Moved the HUD to Monitor {index}."));
        }
        let m = monitors[index - 1].clone();
        #[cfg(windows)]
        {
            let display = crate::win32::DisplayInfo { rect: m.rect, work: m.work, primary: m.primary, device: m.name };
            crate::win32::call(move || {
                crate::win32::show_hud(hwnd);
                crate::win32::move_to_display(hwnd, &display)
            })
            .await??;
        }
        // (A plain window keeps its size, in the middle of that monitor.)
        #[cfg(not(windows))]
        {
            let (now, ..) = wm::window_state(hwnd).ok_or("My HUD window is not open.")?;
            wm::set_window_rect(hwnd, wm::spot_in(wm::Spot::Center, m.work, now))?;
        }
        gui::remember_placement(&self.config);
        Ok(format!("Moved the HUD to Monitor {index}."))
    }

    /// A protocol step: makes sure an application runs (launching it from its executable or
    /// name), then puts its window on a monitor — at a saved place and size, or keeping its
    /// size — and maximises it when asked.
    async fn place_app(&self, args: &Value) -> Result<String, String> {
        let app = str_arg(args, &["app_name", "app", "name"]).ok_or("place_application_window needs 'app_name'.")?.to_string();
        let monitor = args.get("monitor").cloned().unwrap_or(json!(1));
        #[cfg(windows)]
        {
            let target = WindowTarget::Named(app.clone());
            let find = |t: WindowTarget| crate::win32::call(move || crate::winctl::find_windows(&t).ok().and_then(|l| l.into_iter().next()));
            let mut window = find(target.clone()).await?;
            let mut launched = false;
            if window.is_none() {
                launched = true;
                match str_arg(args, &["exe_path"]).filter(|p| std::path::Path::new(p).exists()) {
                    Some(exe) => {
                        let exe = exe.to_string();
                        crate::win32::call(move || crate::win32::shell_execute(&exe, None, None)).await??;
                    }
                    None => {
                        apps::open(&app, &[]).await?;
                    }
                }
                let deadline = Instant::now() + Duration::from_secs(20);
                while window.is_none() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    window = find(target.clone()).await?;
                }
            }
            let w = window.ok_or_else(|| format!("{} did not open a window in time.", apps::display_name(&app)))?;
            // Windows that just opened settle (splash screens, restored sizes) before moving.
            if launched {
                tokio::time::sleep(Duration::from_millis(1200)).await;
            }
            let monitors = monitors::list();
            let wanted = monitors::resolve(&monitor, None, &monitors);
            // A saved monitor that is not connected now: the closest one there is.
            let index = wanted.unwrap_or_else(|| monitor.as_u64().map(|n| (n as usize).clamp(1, monitors.len().max(1))).unwrap_or(1));
            let m = monitors.get(index.saturating_sub(1)).cloned().ok_or("Windows reported no monitors.")?;
            let maximise = bool_arg(args, "maximized").or_else(|| bool_arg(args, "maximised")).unwrap_or(false);
            let num = |k: &str| args.get(k).and_then(Value::as_i64).map(|v| v as i32);
            let hwnd = w.hwnd;
            match (num("x"), num("y"), num("width"), num("height")) {
                (Some(x), Some(y), Some(width), Some(height)) => {
                    let [l, t, r, b] = m.work;
                    let width = width.min(r - l);
                    let height = height.min(b - t);
                    let rect = [(l + x).clamp(l, r - width), (t + y).clamp(t, b - height), width, height];
                    crate::win32::call(move || crate::win32::place_window(hwnd, rect, maximise)).await??;
                }
                _ => {
                    let display = crate::win32::DisplayInfo { rect: m.rect, work: m.work, primary: m.primary, device: m.name.clone() };
                    crate::win32::call(move || {
                        crate::win32::move_to_display(hwnd, &display)?;
                        if maximise {
                            crate::win32::show_window(hwnd, crate::win32::WindowCmd::Maximize);
                        }
                        Ok::<(), String>(())
                    })
                    .await??;
                }
            }
            self.focus.note_app(&app);
            let note = match wanted {
                None => format!(" (Monitor {} is not connected, so it went to Monitor {index})", monitor),
                Some(_) => String::new(),
            };
            Ok(format!("{} {} on Monitor {index}{}{note}.", if launched { "Opened" } else { "Placed" }, apps::display_name(&app), if maximise { ", maximised" } else { "" }))
        }
        // shortcut: KDE lets no program set another's place and size (only a KWin script could),
        // so a saved place keeps the window's own size there; upgrade if KWin offers a call for it.
        #[cfg(not(windows))]
        {
            let target = WindowTarget::Named(app.clone());
            // (Under KDE until its window is there, at most 20 s; elsewhere 2 s.)
            let shown = || crate::plasma::available() && crate::winctl::kde_windows(&target).is_ok();
            if !shown() {
                let _ = apps::open(&app, &[]).await;
                let deadline = Instant::now() + Duration::from_secs(if crate::plasma::available() { 20 } else { 2 });
                while !shown() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            let out = monitors::move_app(target.clone(), monitor).await?;
            if bool_arg(args, "maximized").or_else(|| bool_arg(args, "maximised")).unwrap_or(false) {
                winctl::manage(WindowAction::Maximize, target).await?;
            }
            self.focus.note_app(&app);
            Ok(out)
        }
    }

    /// run / capture / list / create / delete. Returns the result and (modality, state,
    /// shutdown) effects of the steps that ran.
    async fn protocols_tool(&self, args: &Value) -> (Result<String, String>, (bool, bool, bool)) {
        let action = str_arg(args, &["action"]).unwrap_or("run").to_lowercase().replace([' ', '-'], "_");
        let name = str_arg(args, &["protocol_name", "name", "protocol"]).unwrap_or("");
        let mut effects = (false, false, false);
        let result = match action.as_str() {
            "capture_current_workspace" | "capture" | "save_workspace" | "snapshot" | "capture_workspace" | "save_current_setup" => {
                if name.is_empty() {
                    Err("Which name should the protocol get?".to_string())
                } else {
                    let apps = tokio::task::spawn_blocking(protocols::scan_workspace).await.unwrap_or_default();
                    let jai = self.widgets.windows();
                    let music = self.music.last_played();
                    let steps = protocols::workspace_steps(&apps, &jai, music.as_ref());
                    let summary: Vec<String> = apps.iter().map(|a| format!("{} on Monitor {}{}", apps::display_name(&a.process), a.monitor, if a.maximized { " (maximised)" } else { "" })).collect();
                    self.protocols
                        .save(Protocol {
                            when: Vec::new(),
                            name: name.to_string(),
                            description: str_arg(args, &["description"]).map(String::from).unwrap_or_else(|| format!("Captured workspace: {}.", summary.join(", "))),
                            trigger_phrases: Vec::new(),
                            steps,
                        })
                        .map(|p| {
                            // (A desk layout just kept is his by its name at once: "the
                            // standard layout" a moment later is this one, not the HUD's.)
                            crate::kit_screen::sync_layouts(self);
                            format!(
                                "Saved the current workspace as the {} ({} steps): {}{}{}.",
                                protocols::display_name(&p.name),
                                p.steps.len(),
                                if summary.is_empty() { "no application windows".to_string() } else { summary.join(", ") },
                                if jai.iter().any(|w| !w.live_kinds().is_empty()) { ", plus my live windows" } else { "" },
                                music.map(|m| format!(", and {} playing", m.name)).unwrap_or_default()
                            )
                        })
                }
            }
            run if protocols::RUNS.contains(&run) => match self.protocols.get(name) {
                None => Err(format!(
                    "No protocol called '{name}'. Known protocols: {}.",
                    self.protocols.list().iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
                )),
                Some(p) => {
                    // Steps in his own words follow the rest, one after the other, as if he had
                    // said them.
                    let (said, steps): (Vec<protocols::Step>, Vec<protocols::Step>) = p.steps.iter().cloned().partition(|s| s.tool == protocols::ORDER);
                    // ("Tell me" is said at once: what set the protocol off.)
                    let (told, orders): (Vec<String>, Vec<String>) = said.iter().filter_map(|s| s.args["order"].as_str().map(String::from)).partition(|order| crate::kit_notes::is_tell_me(order));
                    let because = str_arg(args, &["because"]);
                    if let Some(news) = because.filter(|_| !told.is_empty()) {
                        let _ = self.stark.events.send(crate::stark::StarkEvent::Alert(news.to_string()));
                    }
                    // Its one line, when no turn of his says it: set off by a time or a rule, or by
                    // RUN NOW in its window. (Set off by a sound, `quiet`: not a word of his own.)
                    if (because.is_some() && args["quiet"] != true) || args["announce"] == true {
                        let _ = self.stark.events.send(crate::stark::StarkEvent::Say(protocols::activating(&p.name)));
                    }
                    let p = Protocol { steps, ..p };
                    // Steps on different things (each app, the music, JAI's own windows)
                    // run side by side; steps on the same thing keep their order.
                    let stages = protocols::plan(&p.steps);
                    let abreast = stages.iter().map(Vec::len).max().unwrap_or(1);
                    info!("[PROTOCOL] Engaging '{}' ({} steps, up to {abreast} side by side)", p.name, p.steps.len());
                    let started = std::time::Instant::now();
                    let mut outcomes: Vec<Option<ToolOutcome>> = p.steps.iter().map(|_| None).collect();
                    for stage in stages {
                        match stage.as_slice() {
                            [lane] if lane.len() == 1 => {
                                let step = &p.steps[lane[0]];
                                outcomes[lane[0]] = Some(Box::pin(self.execute(&step.tool, &step.args)).await);
                            }
                            lanes => {
                                let lanes = lanes.iter().map(|lane| lane.iter().map(|i| (*i, p.steps[*i].tool.clone(), p.steps[*i].args.clone())).collect()).collect();
                                for (index, outcome) in crate::hyper_cortex::run_lanes(self.clone(), lanes).await {
                                    outcomes[index] = Some(outcome);
                                }
                            }
                        }
                    }
                    info!("[PROTOCOL] '{}' ran in {} ms", p.name, started.elapsed().as_millis());
                    let mut lines = Vec::new();
                    let mut failed = 0;
                    for (step, outcome) in p.steps.iter().zip(outcomes) {
                        let Some(outcome) = outcome else {
                            failed += 1;
                            lines.push(format!("FAIL {}: the step stopped unexpectedly", step.tool));
                            continue;
                        };
                        effects.0 |= outcome.modality_changed;
                        effects.1 |= outcome.state_changed;
                        effects.2 |= outcome.shutdown_requested;
                        if !outcome.success {
                            failed += 1;
                        }
                        let first = outcome.output.lines().next().unwrap_or("").chars().take(120).collect::<String>();
                        lines.push(format!("{} {}: {first}", if outcome.success { "OK" } else { "FAIL" }, step.tool));
                    }
                    let then = if orders.is_empty() { String::new() } else { format!("\nThen: {}.", orders.join(", then ")) };
                    if !orders.is_empty() {
                        let _ = self.stark.events.send(crate::stark::StarkEvent::RunMacro { name: format!("{} protocol", p.name), steps: orders.clone(), quiet: true });
                    }
                    let summary = if p.steps.is_empty() {
                        format!("{} engaged.{then}", protocols::display_name(&p.name))
                    } else {
                        format!(
                            "{} engaged ({} of {} steps succeeded). {}\n{}{then}",
                            protocols::display_name(&p.name),
                            p.steps.len() - failed,
                            p.steps.len(),
                            protocols::choreography(&p),
                            lines.join("\n")
                        )
                    };
                    if failed == p.steps.len() && orders.is_empty() { Err(summary) } else { Ok(summary) }
                }
            },
            "list" | "show" => Ok(self
                .protocols
                .list()
                .iter()
                .map(|p| {
                    format!(
                        "- {} protocol: {} ({} steps{})",
                        p.name,
                        if p.description.is_empty() { "custom routine" } else { &p.description },
                        p.steps.len(),
                        if p.trigger_phrases.is_empty() { String::new() } else { format!("; also \"{}\"", p.trigger_phrases.join("\", \"")) }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")),
            "create" | "save" | "add" | "update" => {
                let list = |key: &str| -> Vec<String> {
                    args.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
                };
                self.protocols
                    .save(Protocol {
                        when: Vec::new(),
                        name: name.to_string(),
                        description: str_arg(args, &["description"]).unwrap_or("").to_string(),
                        trigger_phrases: list("trigger_phrases"),
                        steps: protocols::parse_steps(args),
                    })
                    .map(|p| format!("Protocol '{}' saved with {} step(s); say \"engage {} protocol\".", p.name, p.steps.len(), p.name))
            }
            "delete" | "remove" => {
                if self.protocols.delete(name) {
                    Ok(format!("Deleted the {name} protocol."))
                } else {
                    Err(format!("No protocol called '{name}'."))
                }
            }
            other => Err(format!("Unknown protocols action '{other}' (run, list, create, delete).")),
        };
        (result, effects)
    }

    async fn media(&self, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or("").to_lowercase();
        let level = int_arg(args, "level");
        match action.as_str() {
            "jai_faster" | "jai_slower" | "set_jai_speed" | "speak_faster" | "speak_slower" => {
                let current = f32::from_bits(self.voice_speed.load(Ordering::Relaxed));
                let next = match action.as_str() {
                    "set_jai_speed" => level.map(|l| l as f32 / 100.0).unwrap_or(current),
                    a if a.ends_with("faster") => current + 0.15,
                    _ => current - 0.15,
                }
                .clamp(0.6, 1.8);
                self.voice_speed.store(next.to_bits(), Ordering::Relaxed);
                self.config.update(|c| c.kokoro_speed = (next * 100.0).round() / 100.0);
                Ok(format!("JAI now speaks at {:.0}% speed.", next * 100.0))
            }
            "jai_louder" | "jai_quieter" | "set_jai_volume" | "voice_louder" | "voice_quieter" => {
                let current = self.voice_volume.load(Ordering::Relaxed);
                let next = match action.as_str() {
                    "set_jai_volume" => level.unwrap_or(current as i64).clamp(0, 200) as u8,
                    a if a.ends_with("louder") => current.saturating_add(20).min(200),
                    _ => current.saturating_sub(20).max(10),
                };
                self.voice_volume.store(next, Ordering::Relaxed);
                Ok(format!("JAI voice loudness set to {next}%."))
            }
            // A volume for one app, or a level with a sign ("-50" is a step down, never the
            // level 0): the mixer's, which knows both.
            "set_volume" | "volume" | "set" | "volume_up" | "volume_down" | "mute" | "unmute"
                if str_arg(args, &["app", "app_name", "application", "target"]).is_some() || args.get("level").is_some_and(|l| crate::audio_mixer::signed_step(l).is_some()) =>
            {
                let mut mixer = args.clone();
                mixer["action"] = json!(match action.as_str() {
                    "volume_up" | "volume_down" | "mute" | "unmute" => action.as_str(),
                    _ => "set",
                });
                if matches!(action.as_str(), "volume_up" | "volume_down") {
                    mixer["delta"] = json!(level.unwrap_or(10));
                    if let Some(m) = mixer.as_object_mut() {
                        m.remove("level");
                    }
                }
                crate::audio_mixer::tool(&mixer).await
            }
            _ => match MediaAction::parse(&action, level) {
                Some(a) => media::execute(a).await,
                None => Err(format!("Unknown media action '{action}' (set_volume needs a 'level').")),
            },
        }
    }

    /// Write → test → save: the script must run cleanly once before it becomes a skill.
    async fn create_persistent_skill(&self, args: &Value) -> Result<String, String> {
        let name = str_arg(args, &["name"]).ok_or("create_persistent_skill needs 'name'.")?;
        let script = str_arg(args, &["script", "code"]).ok_or("create_persistent_skill needs 'script'.")?;
        let list = |key: &str| -> Vec<String> {
            args.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        let skill = JaiSkill {
            name: name.to_string(),
            title: str_arg(args, &["title"]).unwrap_or("").to_string(),
            description: str_arg(args, &["description"]).unwrap_or("Custom skill").to_string(),
            trigger_phrases: list("trigger_phrases"),
            parameters: args.get("parameters").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
            runtime: crate::skills::normalize_runtime(str_arg(args, &["language", "runtime"]).unwrap_or("")),
            script: script.to_string(),
            usage_count: 0,
            last_output: String::new(),
            updated_at: String::new(),
        };
        let test_args = args.get("test_args").cloned().unwrap_or_else(|| json!({}));
        let needs_input = skill.parameters.iter().any(|p| p.required && test_args.get(&p.name).is_none())
            || (script.contains("{{") && test_args.as_object().is_none_or(|o| o.is_empty()));
        let test = if needs_input {
            "not test-run (it needs parameters)".to_string()
        } else {
            match self.skills.test_run(&skill, &test_args).await {
                Ok(out) => format!("test run OK:\n{}", out.chars().take(800).collect::<String>()),
                Err(e) => return Err(format!("The test run failed, so the skill was NOT saved. Fix the script and try again. {e}")),
            }
        };
        let saved = self.skills.save(&skill).map_err(|e| e.to_string())?;
        Ok(format!("Learned skill '{}' ({}, {}); it is now the tool skill_{}. {test}", saved.name, saved.title, saved.runtime, saved.name))
    }

    async fn create_skill(&self, args: &Value) -> Result<String, String> {
        let name = str_arg(args, &["name"]).ok_or("create_skill needs 'name'.")?;
        let script = str_arg(args, &["script", "code"]).ok_or("create_skill needs 'script'.")?;
        let list = |key: &str| -> Vec<String> {
            args.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        let parameters: Vec<SkillParameter> = args
            .get("parameters")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let saved = self
            .skills
            .save(&JaiSkill {
                name: name.to_string(),
                title: str_arg(args, &["title"]).unwrap_or("").to_string(),
                description: str_arg(args, &["description"]).unwrap_or("Custom skill").to_string(),
                trigger_phrases: list("trigger_phrases"),
                parameters,
                runtime: String::new(),
                script: script.to_string(),
                usage_count: 0,
                last_output: String::new(),
                updated_at: String::new(),
            })
            .map_err(|e| e.to_string())?;
        if bool_arg(args, "run_immediately").unwrap_or(true) && saved.parameters.iter().all(|p| !p.required) {
            match self.skills.execute(&saved.name, &json!({})).await {
                Ok(out) => Ok(format!("Learned skill '{}' and ran it. Output:\n{out}", saved.name)),
                Err(e) => Ok(format!("Learned skill '{}', but the first run reported: {e}", saved.name)),
            }
        } else {
            Ok(format!("Learned skill '{}' ({}).", saved.name, saved.title))
        }
    }
}

fn pc_tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "open_application",
            "description": "Open any installed application, folder or file (e.g. browser = Sir's default web browser, spotify, discord, chrome, vs code, steam, explorer, terminal, calculator, downloads folder). Call it whenever Sir asks to open, start or launch something. Websites, channels and site searches: open_website.",
            "parameters": {"type": "object", "properties": {
                "app_name": {"type": "string", "description": "Application name, URL, folder or path"},
                "args": {"type": "array", "items": {"type": "string"}, "description": "Optional command-line arguments"}
            }, "required": ["app_name"]}
        }}),
        json!({"type": "function", "function": {
            "name": "close_application",
            "description": "Close a running application by name. app_name \"it\" = the app or website just discussed (a website's tab only).",
            "parameters": {"type": "object", "properties": {
                "app_name": {"type": "string"},
                "force": {"type": "boolean", "description": "Terminate if it refuses to close (default true)"}
            }, "required": ["app_name"]}
        }}),
        json!({"type": "function", "function": {
            "name": "window_management",
            "description": "Focus, minimise, maximise, true full screen (F11), exit full screen, restore, close, snap, make bigger / smaller an app's window, or minimise everything. Omit target_window for \"it\"/\"that\": the app or website just discussed, else the window Sir was working in — never yourself.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["focus", "minimize", "maximize", "fullscreen", "exit_fullscreen", "restore", "close", "close_tab", "snap_left", "snap_right", "snap_top", "snap_bottom", "bigger", "smaller", "minimize_all", "show_desktop"]},
                "target_window": {"type": "string", "description": "App or window title; omit for the app in focus"}
            }, "required": ["action"]}
        }}),
        sites::tool_definition(),
        json!({"type": "function", "function": {
            "name": "media_and_volume_control",
            "description": "System volume (exact %), mute, media playback keys, or JAI's own voice loudness.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["set_volume", "volume_up", "volume_down", "mute", "unmute", "get_volume", "play_pause", "next_track", "prev_track", "stop", "jai_louder", "jai_quieter", "set_jai_volume", "jai_faster", "jai_slower", "set_jai_speed"]},
                "level": {"type": "integer", "description": "0-100 for set_volume, 0-200 for set_jai_volume, 60-180 (percent) for set_jai_speed, step size for volume_up/down"}
            }, "required": ["action"]}
        }}),
        json!({"type": "function", "function": {
            "name": "get_system_status",
            "description": "Live CPU load, RAM, disk space and GPU temperature/VRAM.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "get_current_datetime",
            "description": "Current local date and time.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "run_shell_command",
            "description": "Run a PowerShell (Windows) or Bash (Linux) command and return its output (10 s timeout).",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}
        }}),
        json!({"type": "function", "function": {
            "name": "web_search",
            "description": "Search the live web (DuckDuckGo + Wikipedia + live weather) for news, weather, prices, people, facts or anything time-sensitive or uncertain. The best page is read automatically; answer with its concrete facts, numbers and dates.",
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}
        }}),
        json!({"type": "function", "function": {
            "name": "read_webpage",
            "description": "Fetch a web page and return its readable article text (up to 4,500 characters), e.g. a result URL from web_search.",
            "parameters": {"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}
        }}),
        interface::tool_definition(),
        interface::control_tool_definition(),
        interface::orb_mode_tool_definition(),
        crate::audio_mixer::tool_definition(),
        json!({"type": "function", "function": {
            "name": "undo_last_action",
            "description": "\"Undo that\", \"put it back\", \"revert that\": takes back the last thing YOU changed of your own (the HUD's look, layout or screens, a volume, a window or widget of yours that was closed or opened, the music you stopped, a file edit). One step, the latest. Never for another program's window or a message.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "recent_actions",
            "description": "\"What did you just do?\", \"what did you change?\", \"did you mute Discord?\": your own last actions as they really went. Never answer that from memory.",
            "parameters": {"type": "object", "properties": {"about": {"type": "string", "description": "Only what you did to this program or view (\"discord\", \"mixer\"); leave it out for everything."}}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "whats_new",
            "description": "\"What's new?\", \"any new features?\": what this build of you does that the last one did not.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "whats_open",
            "description": "\"What's open?\", \"what's on the board?\", \"which widgets are open?\": the board, blocks, windows and widgets of yours that are up right now, by name. Never answer that from memory.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "is_app_running",
            "description": "\"Is Firefox open?\", \"is Discord running?\", \"läuft Spotify?\": whether a program runs right now. Never answer that from memory.",
            "parameters": {"type": "object", "properties": {
                "app_name": {"type": "string", "description": "The program"}
            }, "required": ["app_name"]}
        }}),
        json!({"type": "function", "function": {
            "name": "timer_left",
            "description": "\"How long is left on the timer?\", \"how much time is left?\", \"what's the stopwatch at?\": read off the widgets panel. Never work that out yourself.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "set_listening_mode",
            "description": "How YOU listen: hands_free (answer relevant speech without the wake word; \"switch to hands-free mode\", \"Freisprechmodus aktivieren\") or wake_word (only when Sir says JAI; \"only listen when I say JAI\").",
            "parameters": {"type": "object", "properties": {
                "mode": {"type": "string", "enum": ["hands_free", "wake_word"]}
            }, "required": ["mode"]}
        }}),
        json!({"type": "function", "function": {
            "name": "customize_hud_and_windows",
            "description": "Restyle your main HUD (target \"hud\") or one of your extra windows (target = its id or title, \"last\" or \"all\"): theme (stark_gold, mono, arc_reactor, hot_rod, gamma, vibranium, ember, graphite) or an accent_color (a colour name or #RRGGBB), opacity 0.2-1.0, borderless, always_on_top, font_scale 0.8-1.6; for the HUD also compact (Mini-HUD) and layout.",
            "parameters": {"type": "object", "properties": {
                "target": {"type": "string"},
                "theme": {"type": "string"},
                "accent_color": {"type": "string"},
                "opacity": {"type": "number"},
                "borderless": {"type": "boolean"},
                "always_on_top": {"type": "boolean"},
                "font_scale": {"type": "number"},
                "compact": {"type": "boolean"},
                "layout": {"type": "string", "enum": ["full", "compact", "orb"]}
            }, "required": ["target"]}
        }}),
        modality::tool_definition(),
        json!({"type": "function", "function": {
            "name": "control_jai_lifecycle",
            "description": "Control YOURSELF (the J.A.I. app, never the computer): shutdown_jai when Sir says shut down, quit, exit, close yourself, go away or similar; restart_jai; mute_tts / unmute_tts (your voice); sleep_mode (ignore everything until 'wake up'); wake_up.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["shutdown_jai", "restart_jai", "mute_tts", "unmute_tts", "sleep_mode", "wake_up"]}
            }, "required": ["action"]}
        }}),
    ]
}

/// Everyday assistant abilities (reminders, briefing, maths, clipboard, screen, files, diagnostics).
fn assistant_tool_definitions() -> Vec<Value> {
    let mut tools = vec![
        reminders::tool_definition(),
        json!({"type": "function", "function": {
            "name": "calculate",
            "description": "Exact arithmetic and unit conversion. ALWAYS use it instead of doing maths yourself: '15% of 240', '(3+4)*2^5', 'sqrt(2)', '5 miles to km', '100 F in C'.",
            "parameters": {"type": "object", "properties": {"expression": {"type": "string"}}, "required": ["expression"]}
        }}),
        clipboard::tool_definition(),
        screen::inspect_tool_definition(),
    ];
    tools.extend(file_ops::tool_definitions());
    tools.extend(diagnostics::tool_definitions());
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A silent scripted session (`--silent`) carries out what stays inside JAI and what
    /// only reads; everything that would reach the PC around him is noted and left undone.
    #[test]
    fn a_silent_session_acts_inside_jai_only() {
        for (name, args) in [
            ("configure_interface", json!({"span_all_monitors": true})),
            ("spawn_native_window", json!({"live": "pc_stats"})),
            ("create_or_update_widget", json!({"type": "MetricGauge", "title": "CPU load"})),
            ("hud_board", json!({"action": "close"})),
            // The volume is his there: the mixer works on a copy of the real one.
            ("control_audio_mixer", json!({"action": "mute_others", "app": "discord"})),
            ("media_and_volume_control", json!({"action": "set_volume", "level": 40})),
            ("get_system_status", json!({})),
            ("web_search", json!({"query": "the height of Mount Everest"})),
            ("control_music", json!({"action": "what_is_playing"})),
            ("move_window_to_monitor", json!({"app_or_window_name": "yourself", "target_monitor": 2})),
            ("show_mobile_satellite_instructions", json!({"topic": "permanent_address"})),
            // A simulation is a window of his own.
            ("launch_interactive_simulation", json!({"sim_type": "pendulum"})),
        ] {
            assert!(stays_inside(name, &args), "{name} {args}");
        }
        // One of his own windows goes to another monitor; a program of Sir's never does.
        let console = json!({"app_or_window_name": "text console", "target_monitor": 2});
        assert!(!stays_inside("move_window_to_monitor", &console), "no such window of his is open");
        crate::own_windows::pretend(&["Text Console \u{2014} J.A.I."]);
        assert!(stays_inside("move_window_to_monitor", &console));
        // A program's name stays the program's, as its move does, even where a window of his
        // has it in its title.
        crate::own_windows::pretend(&["Discord Audio \u{2014} J.A.I.", "Spotify \u{2014} J.A.I."]);
        for app in ["discord", "spotify"] {
            assert!(!stays_inside("move_window_to_monitor", &json!({"app_or_window_name": app, "target_monitor": 2})), "{app}");
        }
        crate::own_windows::pretend(&[]);
        for (name, args) in [
            ("open_application", json!({"app_name": "spotify"})),
            ("close_application", json!({"app_name": "spotify", "force": true})),
            ("open_website", json!({"platform_or_url": "youtube"})),
            ("control_music", json!({"action": "play", "playlist_name": "JaysRock"})),
            ("control_music", json!({"action": "set_volume", "level": "-50"})),
            ("media_and_volume_control", json!({"action": "next_track"})),
            ("window_management", json!({"action": "minimize_all"})),
            ("move_window_to_monitor", json!({"app_or_window_name": "sonora", "target_monitor": 2})),
            ("manage_stark_protocol", json!({"action": "execute_protocol", "protocol_name": "gaming"})),
            ("show_mobile_satellite_instructions", json!({})),
            ("reminders", json!({"action": "create", "text": "x"})),
            ("discord_call", json!({"action": "listen"})),
            ("shutdown_jai", json!({})),
            ("configure_interface", json!({"start_with_system": true})),
        ] {
            assert!(!stays_inside(name, &args), "{name} {args}");
        }
        assert!(!silent_session(), "only `--silent` switches it on");
    }

    /// "Should I close Firefox?" had the model call close_application (silent session,
    /// 2026-11-07): on a plain question it gets what reads, and nothing else.
    #[test]
    fn a_plain_question_only_reads() {
        for (name, args) in [
            ("get_system_status", json!({})),
            ("web_search", json!({"query": "x"})),
            ("calculate", json!({"expression": "2+2"})),
            ("check_unread_emails", json!({})),
            ("find_files", json!({"query": "x"})),
            ("inspect_screen_or_selection", json!({})),
            ("lookup_formula", json!({"name": "x"})),
            ("thermal_advisor", json!({})),
            ("control_audio_mixer", json!({"action": "get_app", "app": "discord"})),
            ("control_orb_mode", json!({"action": "status"})),
            ("control_music", json!({"action": "what_is_playing"})),
            ("manage_stark_protocol", json!({"action": "list"})),
            ("manage_flashcards", json!({"action": "stats"})),
            (crate::venue_guide::TOOL, json!({"action": "recommend", "request": "x"})),
            ("clipboard", json!({"action": "read"})),
            // "What's next?", "what did I miss?": he shows it, and that is the answer.
            ("configure_interface", json!({"next_now": true})),
            ("configure_interface", json!({"missed": true})),
        ] {
            assert!(only_looks(name, &args), "{name} {args}");
        }
        for (name, args) in [
            ("close_application", json!({"app_name": "firefox"})),
            ("open_application", json!({"app_name": "firefox"})),
            ("control_orb_mode", json!({"enabled": false})),
            ("control_audio_mixer", json!({"action": "set_master", "level": 20})),
            ("control_audio_mixer", json!({"action": "no_sound"})),
            ("control_music", json!({"action": "stop_all"})),
            ("configure_interface", json!({"mixer_view": true})),
            ("configure_interface", json!({"next_now": true, "desk_clock": false})),
            ("configure_interface", json!({})),
            ("window_management", json!({"action": "minimize_all"})),
            ("manage_stark_protocol", json!({"action": "execute_protocol", "protocol_name": "gaming"})),
            ("run_shell_command", json!({"command": "x"})),
            ("send_whatsapp_message", json!({"to": "x", "text": "y"})),
            ("shutdown_jai", json!({})),
            ("clipboard", json!({"action": "write", "text": "x"})),
            ("reminders", json!({"action": "create", "text": "x"})),
        ] {
            assert!(!only_looks(name, &args), "{name} {args}");
        }
        assert_eq!(only_asked("close_application", &json!({})).output, ONLY_ASKED);
    }

    /// Live 2026-11-15: the question guard dropped "is game mode on" (game_status was no reading
    /// action), and the model answered "Game mode engaged" with game mode off.
    #[test]
    fn a_question_about_game_mode_reads_it() {
        for said in ["is game mode on", "am I in game mode"] {
            assert!(!crate::intents::beyond_the_question(said, "manage_vram", &json!({"action": "game_status"})), "{said}");
        }
        assert!(crate::intents::beyond_the_question("is game mode on", "manage_vram", &json!({"action": "game_on"})), "a question never switches it on");
    }

    #[test]
    fn the_door_answers_with_the_parameters_asked_for() {
        let all = definitions_with(Vec::new());
        let shown = more_tools(&all, &json!({"names": ["chemistry_helper", "Lookup Formula"]})).unwrap();
        assert!(shown.contains("\"chemistry_helper\"") && shown.contains("\"lookup_formula\"") && shown.contains("parameters"), "{shown}");
        assert!(!shown.contains("\"discord_bot\""), "only what was asked for");
        assert!(more_tools(&all, &json!({})).unwrap_err().contains("chemistry_helper"), "no name: the list");
        assert!(only_looks("more_tools", &json!({"names": ["chemistry_helper"]})) && stays_inside("more_tools", &json!({})));
    }

    /// Live 2026-11-15: behind the door all five went worse (made-up arguments for flashcards, the
    /// auto-reply and the price watch, another tool for a restaurant, and "screen time today"
    /// answered with an invented 8 h 23 min).
    #[test]
    fn everyday_tools_stay_in_sight() {
        let shown = prompt_definitions_from(definitions_with(Vec::new()));
        let names: Vec<&str> = shown.iter().filter_map(|t| t["function"]["name"].as_str()).collect();
        for name in ["screen_time_report", "set_auto_responder", "manage_price_sniper", "manage_flashcards", crate::venue_guide::TOOL] {
            assert!(names.contains(&name), "{name} waits behind the door");
        }
        assert!(names.contains(&"more_tools") && !names.contains(&"discord_bot"), "the door stays, with the rare ones behind it");
    }

    /// Live 2026-11-15: a door tool that failed for another reason (the other PC was off) came back
    /// with "Call these by name now", and the model called it six times, each one a timeout.
    #[test]
    fn the_door_gives_parameters_not_orders() {
        let shown = more_tools(&definitions_with(Vec::new()), &json!({"names": ["beam_to_device"]})).unwrap();
        let said = shown.split('[').next().unwrap_or_default();
        assert!(!said.contains("now"), "{said}");
        assert!(shown.contains("\"beam_to_device\"") && shown.contains("parameters"), "{shown}");
    }

    /// Live 2026-11-15: the other PC was off, and each failure handed the model the parameters again.
    #[test]
    fn a_door_tool_failing_again_carries_no_parameters() {
        // (A name no other test fails: the set is shared by the whole process.)
        assert!(first_failure("door_test_only_tool"), "the first failure carries its parameters");
        assert!(!first_failure("door_test_only_tool"), "the second in the same turn carries none");
    }

    #[test]
    fn invented_tool_names_are_repaired() {
        let known: Vec<String> = ["open_application", "web_search", "reminders", "create_or_update_widget", "calculate"].map(String::from).to_vec();
        assert_eq!(canonical_tool_name("open_app", &known).as_deref(), Some("open_application"));
        assert_eq!(canonical_tool_name("search-web", &known).as_deref(), Some("web_search"));
        assert_eq!(canonical_tool_name("set_reminder", &known).as_deref(), Some("reminders"));
        assert_eq!(canonical_tool_name("open_aplication", &known).as_deref(), Some("open_application"), "typo");
        assert_eq!(canonical_tool_name("web_search", &known).as_deref(), Some("web_search"));
        assert_eq!(canonical_tool_name("fly_to_the_moon", &known), None);
    }

    #[test]
    fn argument_helpers_are_lenient() {
        let args = json!({"level": "55%", "force": "yes", "name": "  x  "});
        assert_eq!(int_arg(&args, "level"), Some(55));
        assert_eq!(bool_arg(&args, "force"), Some(true));
        assert_eq!(str_arg(&args, &["missing", "name"]), Some("x"));
        assert_eq!(int_arg(&json!({"level": 42.6}), "level"), Some(43));
    }

    #[test]
    fn closing_jai_means_quitting() {
        for name in ["JAI", "J.A.I.", "JAI.exe", "yourself", "your app", "dich"] {
            assert!(names_jai(name), "{name}");
        }
        for name in ["spotify", "discord", "jai-notes", "your browser"] {
            assert!(!names_jai(name), "{name}");
        }
    }

    /// Repro (jai.log, 2026-11-08): close_application "earth you", "up" and "off" reached
    /// Windows ("Earth You isn't running, Sir."). Whoever made the call (the parser or the
    /// model), a filler is no program and his own view closes as his.
    #[test]
    fn a_close_call_naming_fillers_or_his_view_stays_inside_jai() {
        let call = |app: &str| own_close("close_application", &json!({ "app_name": app, "force": true }));
        assert_eq!(call("earth you"), Some(("hud_board", json!({"action": "close", "target": "earth"}))));
        assert_eq!(call("Earth, you."), Some(("hud_board", json!({"action": "close", "target": "earth"}))));
        assert_eq!(call("the world"), Some(("hud_board", json!({"action": "close", "target": "earth"}))));
        assert_eq!(call("the radar please"), Some(("hud_board", json!({"action": "close", "target": "radar"}))));
        let own = Some(("close_application", json!({"app_name": "it", "force": true, "own_only": true})));
        for filler in ["up", "off", "Off.", "for me now"] {
            assert_eq!(call(filler), own, "{filler}");
        }
        // Programs, pronouns and JAI himself stay as they were asked for.
        for app in ["Spotify", "notepad++", "it", "that", "yourself", "you", "JAI"] {
            assert_eq!(call(app), None, "{app}");
        }
        assert_eq!(own_close("open_application", &json!({"app_name": "up"})), None);
    }
}
