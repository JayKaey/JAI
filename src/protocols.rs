//! Protocols — named multi-step routines, the way Tony Stark has his "House Party Protocol":
//! "engage gaming protocol" opens Steam and Discord, sets the volume and keeps JAI quiet in
//! full-screen games. Sir creates his own by voice or chat ("create a protocol called …");
//! they are persisted in `jai_memory.db` and run through the normal tools — steps on
//! different things (each application, the music, JAI's own windows) side by side, steps on
//! the same thing in their order (`plan`).
//!
//! Focus mode is the built-in protocol with a timer: replies stay silent except for alerts, a
//! "Focus session" countdown appears in the HUD, and the previous reply mode returns at the end.
//!
//! Stark Protocols: "save my current setup as Gaming Protocol" captures the whole workspace —
//! every open application (its executable, monitor, position, size and maximised state), JAI's
//! own live windows and the playlist he started last — as steps; engaging it relaunches the apps,
//! moves each window back to its monitor and place, reopens the telemetry windows and starts the
//! music: "Initiating Gaming Protocol, Sir. Moving Discord to Monitor 2 and starting JaysRock."

use crate::modality::{ModalityStore, VoiceResponseMode};
use crate::tools::str_arg;
use crate::widgets::WidgetEngine;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;
use tracing::{info, warn};

/// Tools a protocol may not call (no recursion, no self-destruction mid-routine).
const FORBIDDEN_STEPS: &[&str] = &["protocols", "manage_stark_protocol", "patch_own_source", "recompile_and_restart", "restore_source_backup"];
/// Built-in protocols and their version: a newer version replaces the stored copy once; one Sir
/// deleted stays deleted.
const SEED_VERSION: i64 = 2;
const MAX_STEPS: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

/// A step in Sir's own words ("open Spotify", "tell Max's PC to open Minecraft"): carried out
/// after the others as if he had said it (`StarkEvent::RunMacro`), so it can be anything he
/// understands.
pub const ORDER: &str = "order";

impl Step {
    /// A line of the protocol window as a step: "tool_name {…}" stays that tool's call, any
    /// other line is an order in his words.
    pub fn of_line(line: &str) -> Option<Step> {
        let line = line.trim();
        let call = line.split_once(' ').filter(|(tool, _)| tool.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()));
        if let Some((tool, Ok(args @ Value::Object(_)))) = call.map(|(tool, args)| (tool, serde_json::from_str::<Value>(args.trim()))) {
            return Some(Step { tool: tool.to_string(), args });
        }
        (!line.is_empty()).then(|| Step { tool: ORDER.to_string(), args: json!({ "order": line }) })
    }

    /// The step in plain words, for the protocol window: his own words as he gave them, a
    /// built-in step as what it does ("Open Spotify", "Set the volume to 65 %").
    pub fn words(&self) -> String {
        let a = &self.args;
        let text = |keys: &[&str]| str_arg(a, keys).map(String::from).or_else(|| keys.iter().find_map(|k| a.get(*k).filter(|v| v.is_number()).map(Value::to_string)));
        let app = || crate::apps::display_name(str_arg(a, &["app_name", "app_or_window_name", "name"]).unwrap_or("the app"));
        let pairs = || a.as_object().map(|o| o.iter().map(|(k, v)| format!("{} {}", k.replace('_', " "), v.as_str().map_or_else(|| v.to_string(), String::from))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        match (self.tool.as_str(), str_arg(a, &["action"])) {
            (ORDER, _) => text(&["order"]).unwrap_or_default(),
            ("open_application", _) => format!("Open {}", app()),
            ("place_application_window" | "move_window_to_monitor", _) => {
                format!("Put {} on monitor {}{}", app(), text(&["monitor", "target_monitor"]).unwrap_or_else(|| "1".into()), if a["maximized"] == true { ", full size" } else { "" })
            }
            ("control_music", Some("pause")) => "Pause the music".into(),
            ("control_music", Some("resume")) => "Resume the music".into(),
            ("control_music", _) => format!("Play {}", text(&["playlist_name", "song_name", "query"]).unwrap_or_else(|| "music".into())),
            ("media_and_volume_control", Some("set_volume")) => format!("Set the volume to {} %", text(&["level"]).unwrap_or_default()),
            ("spawn_native_window", _) => format!("Open the {} window", text(&["title", "live"]).unwrap_or_else(|| "extra".into()).replace('_', " ")),
            ("close_native_window", _) => "Close my extra windows".into(),
            ("window_management", Some("minimize_all")) => "Clear the desktop".into(),
            ("focus_mode", _) => format!("Start {} minutes of focus", text(&["minutes"]).unwrap_or_else(|| "25".into())),
            ("open_jai_view", _) => format!("Show the {} view", text(&["view"]).unwrap_or_default().replace('_', " ")),
            ("configure_interface", _) => format!("Change my look: {}", pairs()),
            ("set_response_modality", _) if a["custom_condition"] == "mute_when_fullscreen_app_active" => "Stay quiet while a game is full screen".into(),
            ("sync_study_materials_and_deadlines", _) => "Show my school deadlines".into(),
            (tool, _) => {
                let tool = crate::kit_notes::capitalised(&tool.replace('_', " "));
                match pairs() {
                    rest if rest.is_empty() => tool,
                    rest => format!("{tool}: {rest}"),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Protocol {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Extra phrases besides "engage <name> protocol" ("gaming mode", "zeit zu zocken").
    #[serde(default)]
    pub trigger_phrases: Vec<String>,
    pub steps: Vec<Step>,
    /// When it runs by itself, in the rules' words (`kit_notes::when_of`): "Minecraft starts",
    /// "every day at 7:30", "Max comes online", "I start a game".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<String>,
}

fn key(name: &str) -> String {
    let n = name.trim().to_lowercase().replace('&', " and ").replace(['-', '_', '/'], " ");
    let n = n.trim_end_matches(" protocol").trim_end_matches(" protokoll").trim_start_matches("the ");
    n.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "music and chill" and "music chill" (the fast path drops "&") are the same protocol.
fn loose(name: &str) -> String {
    name.split_whitespace().filter(|w| !matches!(*w, "and" | "und")).collect::<Vec<_>>().join(" ")
}

/// (protocol name, extra trigger phrases), for the instant command parser.
type TriggerTable = Vec<(String, Vec<String>)>;

/// Names and phrases for the instant command parser (no database access there).
static TRIGGERS: LazyLock<RwLock<TriggerTable>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// The protocol a whole sentence asks for: "engage the house party protocol", "gaming
/// protocol", "aktiviere das Gaming-Protokoll", or one of its trigger phrases.
pub fn match_utterance(normalized: &str) -> Option<String> {
    static ENGAGE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(?:(?:engage|activate|initiate|run|start|execute|begin|launch|aktiviere|aktivier|starte|start|führe|fuehre)\s+)?(?:the\s+|das\s+|den\s+)?(.+?)[\s-]+(?:protocol|protokoll)(?:\s+aus)?$")
            .expect("valid protocol regex")
    });
    let s = normalized.trim();
    let triggers = TRIGGERS.read().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = ENGAGE.captures(s) {
        let wanted = key(&c[1]);
        if let Some((name, _)) = triggers.iter().find(|(n, _)| *n == wanted || loose(n) == loose(&wanted)) {
            return Some(name.clone());
        }
        // A spoken name close to a stored one ("house partie").
        return triggers
            .iter()
            .map(|(n, _)| (n, strsim::jaro_winkler(n, &wanted)))
            .filter(|(_, score)| *score >= 0.9)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n.clone());
    }
    drop(triggers);
    his_phrase(s)
}

/// The protocol whose own phrase a sentence is ("wake up daddys home"): what Sir set is his
/// order as it stands, before every other router.
pub fn his_phrase(text: &str) -> Option<String> {
    let s = crate::intents::normalize_phrase(text);
    let triggers = TRIGGERS.read().unwrap_or_else(|e| e.into_inner());
    triggers.iter().find(|(_, phrases)| !s.is_empty() && phrases.iter().any(|p| *p == s)).map(|(n, _)| n.clone())
}

struct FocusSession {
    until_ms: i64,
    previous: VoiceResponseMode,
    generation: u64,
}

struct Inner {
    conn: Option<Mutex<Connection>>,
    protocols: RwLock<Vec<Protocol>>,
    focus: Mutex<Option<FocusSession>>,
    focus_generation: Mutex<u64>,
}

#[derive(Clone)]
pub struct ProtocolStore {
    inner: Arc<Inner>,
}

impl ProtocolStore {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS protocols (name TEXT PRIMARY KEY, spec_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS protocol_seeds (name TEXT PRIMARY KEY, version INTEGER NOT NULL);",
        )?;
        let mut protocols: Vec<Protocol> = {
            let mut stmt = conn.prepare("SELECT spec_json FROM protocols ORDER BY name")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.flatten().filter_map(|raw| serde_json::from_str(&raw).ok()).collect()
        };
        // Built-ins new to this database, or newer than the copy it holds (once: a built-in Sir
        // deleted or re-captured is left alone afterwards).
        let seeded: Vec<(String, i64)> = {
            let mut stmt = conn.prepare("SELECT name, version FROM protocol_seeds")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.flatten().collect()
        };
        let fresh_db = protocols.is_empty() && seeded.is_empty();
        let mut upgraded = Vec::new();
        for p in starter_protocols() {
            let version = seeded.iter().find(|(n, _)| *n == p.name).map(|(_, v)| *v);
            let stored = protocols.iter().any(|x| x.name == p.name);
            // The first built-ins were seeded before this table existed: one of them missing from
            // the store was deleted by Sir. Built-ins added since are simply new.
            const V1_BUILTINS: &[&str] = &["house party", "gaming", "clean slate", "focus"];
            let due = match version {
                None => fresh_db || stored || !V1_BUILTINS.contains(&p.name.as_str()),
                Some(v) => v < SEED_VERSION && stored,
            };
            if due {
                protocols.retain(|x| x.name != p.name);
                upgraded.push(p.name.clone());
                protocols.push(p);
            }
        }
        protocols.sort_by(|a, b| a.name.cmp(&b.name));
        let store = Self::with(protocols, Some(conn));
        for name in &upgraded {
            if let Some(p) = store.get(name) {
                store.persist(&p);
            }
        }
        if let Some(conn) = &store.inner.conn {
            let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
            for p in starter_protocols() {
                let _ = conn.execute(
                    "INSERT INTO protocol_seeds (name, version) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET version = excluded.version",
                    params![p.name, SEED_VERSION],
                );
            }
        }
        if !upgraded.is_empty() {
            info!("[PROTOCOL] Built-in protocols installed or upgraded: {}", upgraded.join(", "));
        }
        info!("[PROTOCOL] {} protocol(s): {}", store.list().len(), store.list().iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "));
        Ok(store)
    }

    pub fn in_memory() -> Self {
        Self::with(starter_protocols(), None)
    }

    fn with(protocols: Vec<Protocol>, conn: Option<Connection>) -> Self {
        let store = Self {
            inner: Arc::new(Inner {
                conn: conn.map(Mutex::new),
                protocols: RwLock::new(protocols),
                focus: Mutex::new(None),
                focus_generation: Mutex::new(0),
            }),
        };
        store.publish_triggers();
        store
    }

    pub fn list(&self) -> Vec<Protocol> {
        self.inner.protocols.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn get(&self, name: &str) -> Option<Protocol> {
        let k = key(name);
        self.list().into_iter().find(|p| p.name == k)
    }

    fn publish_triggers(&self) {
        let triggers = self
            .list()
            .into_iter()
            .map(|p| (p.name.clone(), p.trigger_phrases.iter().map(|t| crate::intents::normalize_phrase(t)).collect()))
            .collect();
        *TRIGGERS.write().unwrap_or_else(|e| e.into_inner()) = triggers;
    }

    fn persist(&self, p: &Protocol) {
        if let Some(conn) = &self.inner.conn {
            let raw = serde_json::to_string(p).unwrap_or_default();
            if let Err(e) = conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
                "INSERT INTO protocols (name, spec_json) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET spec_json = excluded.spec_json",
                params![p.name, raw],
            ) {
                warn!("[PROTOCOL] Could not save '{}': {e}", p.name);
            }
        }
    }

    pub fn save(&self, mut p: Protocol) -> Result<Protocol, String> {
        p.name = key(&p.name);
        if p.name.is_empty() {
            return Err("A protocol needs a name.".into());
        }
        if p.steps.is_empty() {
            return Err("A protocol needs at least one step.".into());
        }
        if p.steps.len() > MAX_STEPS {
            return Err(format!("A protocol may have at most {MAX_STEPS} steps."));
        }
        if let Some(bad) = p.steps.iter().find(|s| FORBIDDEN_STEPS.contains(&s.tool.as_str())) {
            return Err(format!("'{}' cannot be a protocol step.", bad.tool));
        }
        p.trigger_phrases.retain(|t| !t.trim().is_empty());
        {
            let mut list = self.inner.protocols.write().unwrap_or_else(|e| e.into_inner());
            list.retain(|x| x.name != p.name);
            list.push(p.clone());
            list.sort_by(|a, b| a.name.cmp(&b.name));
        }
        self.persist(&p);
        self.publish_triggers();
        Ok(p)
    }

    pub fn delete(&self, name: &str) -> bool {
        let k = key(name);
        let removed = {
            let mut list = self.inner.protocols.write().unwrap_or_else(|e| e.into_inner());
            let before = list.len();
            list.retain(|p| p.name != k);
            before != list.len()
        };
        if removed {
            if let Some(conn) = &self.inner.conn {
                let _ = conn.lock().unwrap_or_else(|e| e.into_inner()).execute("DELETE FROM protocols WHERE name = ?1", params![k]);
            }
            self.publish_triggers();
        }
        removed
    }

    // ----------------------------------------------------------------------- focus ----

    pub fn focus_remaining_ms(&self) -> Option<i64> {
        let focus = self.inner.focus.lock().unwrap_or_else(|e| e.into_inner());
        focus.as_ref().map(|f| (f.until_ms - crate::widgets::now_ms()).max(0))
    }

    /// Silent replies (alerts still spoken), a countdown in the HUD, restore at the end.
    pub fn start_focus(&self, minutes: f64, modality: &ModalityStore, widgets: &WidgetEngine) -> Result<String, String> {
        let minutes = minutes.clamp(1.0, 240.0);
        let generation = {
            let mut g = self.inner.focus_generation.lock().unwrap_or_else(|e| e.into_inner());
            *g += 1;
            *g
        };
        let previous = {
            let mut focus = self.inner.focus.lock().unwrap_or_else(|e| e.into_inner());
            let previous = focus.as_ref().map(|f| f.previous.clone()).unwrap_or_else(|| modality.get());
            *focus = Some(FocusSession { until_ms: crate::widgets::now_ms() + (minutes * 60_000.0) as i64, previous: previous.clone(), generation });
            previous
        };
        modality.set(VoiceResponseMode::CustomCondition("only_on_alerts".into()));
        widgets.create_or_update(&json!({
            "id": "focus_session", "type": "CountdownOrStopwatch", "title": "Focus session", "mode": "countdown",
            "duration_secs": minutes * 60.0, "running": true, "elapsed_ms": 0, "alerted": false, "started_at_ms": crate::widgets::now_ms(),
        }))?;
        let (store, modality, widgets) = (self.clone(), modality.clone(), widgets.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(minutes * 60.0)).await;
            let current = store.inner.focus.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|f| f.generation);
            if current == Some(generation) {
                // The countdown widget itself announces the end; give it a moment.
                tokio::time::sleep(Duration::from_secs(3)).await;
                store.end_focus(&modality, &widgets);
            }
        });
        info!("[FOCUS] {minutes} minute session (reply mode before: {})", previous.display());
        Ok(format!("Focus mode for {minutes:.0} minutes: replies stay silent except alerts until it ends; previous reply mode {} returns afterwards.", previous.display()))
    }

    pub fn end_focus(&self, modality: &ModalityStore, widgets: &WidgetEngine) -> Option<String> {
        let session = self.inner.focus.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        modality.set(session.previous.clone());
        widgets.remove("focus_session");
        info!("[FOCUS] Ended; reply mode back to {}", session.previous.display());
        Some(format!("Focus mode ended; reply mode is {} again.", session.previous.display()))
    }
}

fn step(tool: &str, args: Value) -> Step {
    Step { tool: tool.into(), args }
}

/// Applications the music step launches itself: their window is placed after it.
const PLAYERS: &[&str] = &["spotify", "sonora", "music", "apple music", "itunes", "vlc", "tidal", "deezer", "foobar2000", "youtube music"];

/// What a step works on. Steps on the same thing keep their order; steps on different things
/// may run side by side. None: the step may depend on whatever came before it (a key press, a
/// shell command, "minimise everything"), so it runs alone, once all of that is done.
fn lane_of(step: &Step, index: usize) -> Option<String> {
    match step.tool.as_str() {
        "open_application" | "place_application_window" | "move_window_to_monitor" => {
            let app = str_arg(&step.args, &["app_name", "app_or_window_name", "name"])?.trim().to_lowercase();
            Some(if PLAYERS.contains(&app.as_str()) { "media".to_string() } else { format!("app:{app}") })
        }
        "control_music" | "media_and_volume_control" | "control_audio_mixer" => Some("media".to_string()),
        "spawn_native_window" | "close_native_window" | "update_native_window" | "configure_interface" | "control_orb_mode" | "open_jai_view" | "hud_board" | "pop_up_ui_card" | "arrange_jai_window" => {
            Some("hud".to_string())
        }
        "set_response_modality" | "focus_mode" | "set_listening_mode" => Some("mode".to_string()),
        // A step that only reads disturbs nothing and waits for nothing.
        tool if crate::hyper_cortex::is_read_only(tool, &step.args) => Some(format!("read:{index}")),
        _ => None,
    }
}

/// The order a protocol's steps run in, as indices into `steps`: stages one after another;
/// within a stage, lanes side by side; within a lane, steps in order.
pub fn plan(steps: &[Step]) -> Vec<Vec<Vec<usize>>> {
    fn close(open: &mut Vec<(String, Vec<usize>)>, stages: &mut Vec<Vec<Vec<usize>>>) {
        if !open.is_empty() {
            stages.push(std::mem::take(open).into_iter().map(|(_, lane)| lane).collect());
        }
    }
    let mut stages: Vec<Vec<Vec<usize>>> = Vec::new();
    // The lanes of the stage being built, by what they work on.
    let mut open: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        match lane_of(step, index) {
            Some(key) => match open.iter_mut().find(|(k, _)| *k == key) {
                Some((_, lane)) => lane.push(index),
                None => open.push((key, vec![index])),
            },
            None => {
                close(&mut open, &mut stages);
                stages.push(vec![vec![index]]);
            }
        }
    }
    close(&mut open, &mut stages);
    stages
}

/// Whether a protocol is one J.A.I. comes with (his to change all the same).
pub fn built_in(name: &str) -> bool {
    static NAMES: LazyLock<Vec<String>> = LazyLock::new(|| starter_protocols().into_iter().map(|p| p.name).collect());
    NAMES.iter().any(|n| *n == key(name))
}

fn starter_protocols() -> Vec<Protocol> {
    vec![
        Protocol {
            when: Vec::new(),
            name: "house party".into(),
            description: "Music on, volume up, party colours.".into(),
            trigger_phrases: vec!["party time".into(), "party mode".into()],
            steps: vec![
                step("open_application", json!({ "app_name": "spotify" })),
                step("media_and_volume_control", json!({ "action": "set_volume", "level": 65 })),
                step("configure_interface", json!({ "accent_color": "magenta" })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "gaming".into(),
            description: "Steam up, Discord on Monitor 2, JaysRock playing, volume at 40 %, JAI silent while a full-screen game runs.".into(),
            trigger_phrases: vec!["gaming mode".into(), "zeit zu zocken".into(), "game time".into()],
            steps: vec![
                step("open_application", json!({ "app_name": "steam" })),
                step("place_application_window", json!({ "app_name": "discord", "monitor": 2, "maximized": true })),
                step("control_music", json!({ "action": "play", "playlist_name": "JaysRock" })),
                step("media_and_volume_control", json!({ "action": "set_volume", "level": 40 })),
                step("set_response_modality", json!({ "mode": "custom_condition", "custom_condition": "mute_when_fullscreen_app_active" })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "coding".into(),
            description: "VS Code on Monitor 1, the browser and live PC stats on Monitor 2, focus music.".into(),
            trigger_phrases: vec!["coding mode".into(), "time to code".into(), "zeit zu coden".into()],
            steps: vec![
                step("place_application_window", json!({ "app_name": "vscode", "monitor": 1, "maximized": true })),
                step("place_application_window", json!({ "app_name": "browser", "monitor": 2, "maximized": true })),
                step("spawn_native_window", json!({ "live": "pc_stats", "title": "PC Stats", "monitor": 2, "width": 520, "height": 640 })),
                step("control_music", json!({ "action": "play", "playlist_name": "Jays Focus" })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "study".into(),
            description: "A 50-minute focus session, the timetable and the course files on screen, quiet focus music.".into(),
            trigger_phrases: vec!["study mode".into(), "time to study".into(), "lernmodus".into(), "zeit zu lernen".into()],
            steps: vec![
                step("focus_mode", json!({ "action": "start", "minutes": 50 })),
                step("open_jai_view", json!({ "view": "schedule" })),
                step("sync_study_materials_and_deadlines", json!({ "action": "list" })),
                step("control_music", json!({ "action": "play", "playlist_name": "Jays Focus" })),
                step("media_and_volume_control", json!({ "action": "set_volume", "level": 25 })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "modding".into(),
            description: "VS Code on Monitor 1, the browser and live PC stats on Monitor 2, focus music; a failed Gradle build shows up as a card.".into(),
            trigger_phrases: vec!["modding mode".into(), "time to mod".into(), "zeit zu modden".into()],
            steps: vec![
                step("place_application_window", json!({ "app_name": "vscode", "monitor": 1, "maximized": true })),
                step("place_application_window", json!({ "app_name": "browser", "monitor": 2, "maximized": true })),
                step("spawn_native_window", json!({ "live": "pc_stats", "title": "PC Stats", "monitor": 2, "width": 520, "height": 640 })),
                step("control_music", json!({ "action": "play", "playlist_name": "Jays Focus" })),
                step("media_and_volume_control", json!({ "action": "set_volume", "level": 30 })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "music and chill".into(),
            description: "Spotify on Monitor 2 with JaysRock, volume at 35 %, the HUD as just the core.".into(),
            trigger_phrases: vec!["music and chill".into(), "chill mode".into(), "chill time".into()],
            steps: vec![
                step("control_music", json!({ "action": "play", "playlist_name": "JaysRock" })),
                step("place_application_window", json!({ "app_name": "spotify", "monitor": 2 })),
                step("media_and_volume_control", json!({ "action": "set_volume", "level": 35 })),
                step("configure_interface", json!({ "layout": "orb" })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "clean slate".into(),
            description: "Stealth: music paused, JAI's extra windows closed, every window out of the way.".into(),
            trigger_phrases: vec!["clear my desk".into(), "clean up my screen".into(), "stealth protocol".into(), "stealth mode".into()],
            steps: vec![
                step("control_music", json!({ "action": "pause" })),
                step("close_native_window", json!({ "id": "all" })),
                step("window_management", json!({ "action": "minimize_all" })),
            ],
        },
        Protocol {
            when: Vec::new(),
            name: "focus".into(),
            description: "A 25-minute focus session (silent except alerts).".into(),
            trigger_phrases: vec![],
            steps: vec![step("focus_mode", json!({ "action": "start", "minutes": 25 }))],
        },
    ]
}

/// "Moving Discord to Monitor 2 and starting JaysRock": what engaging a protocol does, in words.
pub fn choreography(p: &Protocol) -> String {
    let app = |args: &Value| crate::apps::display_name(str_arg(args, &["app_name", "app_or_window_name", "name"]).unwrap_or("the app"));
    let mut parts: Vec<String> = Vec::new();
    for s in &p.steps {
        let phrase = match s.tool.as_str() {
            "place_application_window" | "move_window_to_monitor" => {
                let monitor = s.args.get("monitor").or_else(|| s.args.get("target_monitor")).map(|m| match m {
                    Value::String(x) => x.clone(),
                    other => other.to_string(),
                });
                Some(match monitor {
                    Some(m) if m.chars().all(|c| c.is_ascii_digit()) => format!("moving {} to Monitor {m}", app(&s.args)),
                    Some(m) => format!("moving {} to the {m} monitor", app(&s.args)),
                    None => format!("bringing up {}", app(&s.args)),
                })
            }
            "open_application" => Some(format!("opening {}", app(&s.args))),
            "control_music" => match (str_arg(&s.args, &["action"]), str_arg(&s.args, &["playlist_name", "song_name"])) {
                (Some("play") | None, Some(name)) => Some(format!("starting {name}")),
                (Some("pause"), _) => Some("pausing the music".into()),
                _ => None,
            },
            "spawn_native_window" => str_arg(&s.args, &["live"]).map(|l| format!("opening the {} window", l.replace('_', " "))),
            "close_native_window" => Some("closing my extra windows".into()),
            "window_management" if str_arg(&s.args, &["action"]) == Some("minimize_all") => Some("clearing the desktop".into()),
            _ => None,
        };
        if let Some(phrase) = phrase {
            parts.push(phrase);
        }
    }
    parts.truncate(3);
    let mut sentence = match parts.as_slice() {
        [] => return String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    if let Some(first) = sentence.get(..1) {
        sentence = first.to_uppercase() + &sentence[1..];
    }
    format!("{sentence}.")
}

/// "Gaming" → "Gaming Protocol", "music and chill" → "Music And Chill Protocol".
pub fn display_name(name: &str) -> String {
    let title: Vec<String> = name
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect();
    format!("{} Protocol", title.join(" ").replace(" And ", " & "))
}

/// One application window of a captured workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedWindow {
    pub process: String,
    pub exe: String,
    pub title: String,
    pub monitor: usize,
    /// Visible bounds relative to the monitor's work area (x, y, width, height).
    pub rect: [i32; 4],
    pub maximized: bool,
}

/// Processes that are part of Windows (or of KDE), not Sir's workspace.
fn is_shell_process(p: &str) -> bool {
    matches!(
        p,
        "" | "explorer" | "applicationframehost" | "textinputhost" | "shellexperiencehost" | "searchhost" | "startmenuexperiencehost"
            | "systemsettings" | "lockapp" | "jai" | "nvidia overlay" | "gamebar" | "widgets" | "shellhost" | "crossdeviceresume"
            | "plasmashell" | "krunner" | "kwin_wayland"
    )
}

/// The open application windows: one per program (its top window), with monitor and place.
#[cfg(windows)]
pub fn scan_workspace() -> Vec<CapturedWindow> {
    use crate::win32;
    let monitors = crate::monitors::list();
    let mut out: Vec<CapturedWindow> = Vec::new();
    for w in win32::app_windows() {
        if win32::is_jai_window(&w) || is_shell_process(&w.process) || matches!(w.class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            continue;
        }
        if out.iter().any(|c| c.process == w.process) {
            continue;
        }
        let Some((rect, maximized, minimised)) = win32::window_state(w.hwnd) else { continue };
        if minimised {
            continue;
        }
        // Maximised windows: the monitor they fill; others: where their centre is.
        let (cx, cy) = (rect[0] + rect[2] / 2, rect[1] + rect[3] / 2);
        let Some(index) = crate::monitors::monitor_at(&monitors, cx, cy) else { continue };
        let work = monitors[index - 1].work;
        out.push(CapturedWindow {
            process: w.process.clone(),
            exe: win32::process_path(w.pid).unwrap_or_default(),
            title: w.title.clone(),
            monitor: index,
            rect: [rect[0] - work[0], rect[1] - work[1], rect[2], rect[3]],
            maximized,
        });
    }
    out
}

/// KDE: Plasma's window list, the same way (a program's exe stays empty: it starts by its name).
#[cfg(not(windows))]
pub fn scan_workspace() -> Vec<CapturedWindow> {
    use crate::plasma::state;
    let monitors = crate::monitors::list();
    let me = std::process::id();
    let mut out: Vec<CapturedWindow> = Vec::new();
    for w in crate::plasma::windows() {
        let process = w.program();
        if w.pid == me || w.is(state::SKIP_TASKBAR) || w.is(state::MINIMIZED) || is_shell_process(&process.to_lowercase()) || out.iter().any(|c| c.process == process) {
            continue;
        }
        let [x, y, width, height] = w.geometry;
        let Some(index) = crate::monitors::monitor_at(&monitors, x + width / 2, y + height / 2) else { continue };
        let work = monitors[index - 1].work;
        out.push(CapturedWindow { process, exe: String::new(), title: w.title.clone(), monitor: index, rect: [x - work[0], y - work[1], width, height], maximized: w.is(state::MAXIMIZED) });
    }
    out
}

/// The steps that rebuild a captured workspace.
pub fn workspace_steps(apps: &[CapturedWindow], jai_windows: &[crate::widgets::NativeWindowSpec], music: Option<&crate::music::LibraryItem>) -> Vec<Step> {
    let mut steps: Vec<Step> = apps
        .iter()
        .map(|a| {
            step(
                "place_application_window",
                json!({
                    "app_name": a.process, "exe_path": a.exe, "title": a.title, "monitor": a.monitor,
                    "x": a.rect[0], "y": a.rect[1], "width": a.rect[2], "height": a.rect[3], "maximized": a.maximized,
                }),
            )
        })
        .collect();
    for w in jai_windows {
        let live = w.live_kinds();
        if let Some(kind) = live.first() {
            let mut args = json!({ "id": w.id, "title": w.title, "live": kind, "width": w.actual.map(|a| a[2]).unwrap_or(w.width), "height": w.actual.map(|a| a[3]).unwrap_or(w.height) });
            if let Some(m) = w.monitor {
                args["monitor"] = json!(m);
            }
            if live.len() > 1 {
                args["add_content"] = json!(live[1..]);
            }
            if w.always_on_top {
                args["always_on_top"] = json!(true);
            }
            steps.push(step("spawn_native_window", args));
        }
    }
    if let Some(m) = music {
        let key = if m.kind == crate::music::ItemKind::Track { "song_name" } else { "playlist_name" };
        steps.push(step("control_music", json!({ "action": "play", key: m.name })));
    }
    steps.truncate(MAX_STEPS);
    steps
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "manage_stark_protocol",
            "description": "Stark Protocols: named workspace setups and routines. execute_protocol (\"initiate coding protocol\": launches apps, moves windows to their monitors, opens live windows, starts music); capture_current_workspace (\"save my current setup as Gaming Protocol\": records every open app's monitor, position and size, your live windows and the playlist); list; create (steps [{tool, args}]); delete.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["execute_protocol", "capture_current_workspace", "list", "create", "delete"]},
                "protocol_name": {"type": "string"},
                "description": {"type": "string"},
                "trigger_phrases": {"type": "array", "items": {"type": "string"}},
                "steps": {"type": "array", "items": {"type": "object", "properties": {"tool": {"type": "string"}, "args": {"type": "object"}}}}
            }, "required": ["action"]}
        }}),
        json!({"type": "function", "function": {
            "name": "focus_mode",
            "description": "Focus session: action start (minutes, default 25) keeps you silent except for alerts and shows a countdown; stop ends it early.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["start", "stop"]},
                "minutes": {"type": "number"}
            }, "required": ["action"]}
        }}),
    ]
}

/// Parses `steps` from tool arguments (also accepts "tool" / "name" and "args" / "arguments").
pub fn parse_steps(args: &Value) -> Vec<Step> {
    args.get("steps")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let tool = str_arg(s, &["tool", "name", "function"])?.to_string();
                    let args = s.get("args").or_else(|| s.get("arguments")).cloned().unwrap_or_else(|| json!({}));
                    Some(Step { tool, args })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: the trigger table is process-wide.
    #[test]
    fn engage_phrases_find_protocols_and_new_ones_are_validated() {
        let store = ProtocolStore::in_memory();
        assert!(store.get("House Party Protocol").is_some());
        assert_eq!(match_utterance("engage the house party protocol").as_deref(), Some("house party"));
        assert_eq!(match_utterance("house party protocol").as_deref(), Some("house party"));
        assert_eq!(match_utterance("aktiviere das gaming protokoll").as_deref(), Some("gaming"));
        assert_eq!(match_utterance("gaming mode").as_deref(), Some("gaming"));
        assert_eq!(match_utterance("initiate house partie protocol").as_deref(), Some("house party"));
        assert_eq!(match_utterance("engage the barn door protocol"), None);
        assert_eq!(match_utterance("open spotify"), None);

        let saved = store
            .save(Protocol {
                when: Vec::new(),
                name: "Movie Night Protocol".into(),
                description: String::new(),
                trigger_phrases: vec!["movie time".into()],
                steps: parse_steps(&json!({"steps": [{"tool": "media_and_volume_control", "arguments": {"action": "set_volume", "level": 30}}]})),
            })
            .unwrap();
        assert_eq!(saved.name, "movie night");
        assert_eq!(match_utterance("movie time").as_deref(), Some("movie night"));
        // Sir's own (2026-11-21): his phrase wins over the built-in homecoming, a step is his words
        // or a tool's call, and the protocol window reads both in plain words.
        let lines = ["open Spotify", "media_and_volume_control {\"action\":\"set_volume\",\"level\":30}"];
        store
            .save(Protocol { name: "daddy".into(), description: String::new(), trigger_phrases: vec!["Wake up, Daddy's home!".into()], when: vec!["Max comes online".into()], steps: lines.iter().filter_map(|l| Step::of_line(l)).collect() })
            .unwrap();
        // (Every store another test opens publishes its own phrases: the table is published
        // again before each look, and the phrase is withdrawn at once.)
        let (said, routed) = (0..20)
            .map(|_| {
                store.publish_triggers();
                (his_phrase("wake up daddys home"), crate::phrasings::route("Wake up, daddy's home.").map(|(tool, args)| (tool, args["protocol_name"].clone())))
            })
            .find(|(said, routed)| said.is_some() && routed.as_ref().is_some_and(|(tool, _)| tool == "manage_stark_protocol"))
            .unwrap_or_default();
        let daddy = store.get("daddy").unwrap();
        assert!(store.delete("daddy"));
        assert_eq!(said.as_deref(), Some("daddy"));
        assert_eq!(routed, Some(("manage_stark_protocol".to_string(), json!("daddy"))));
        assert_eq!(daddy.steps[0], Step { tool: ORDER.into(), args: json!({"order": "open Spotify"}) });
        assert_eq!(daddy.steps[1], Step { tool: "media_and_volume_control".into(), args: json!({"action": "set_volume", "level": 30}) });
        assert_eq!(daddy.steps.iter().map(Step::words).collect::<Vec<_>>(), ["open Spotify", "Set the volume to 30 %"]);
        assert_eq!(Step::of_line("  "), None);
        assert!(store.save(Protocol { name: "loop".into(), description: String::new(), trigger_phrases: vec![], when: vec![], steps: vec![step("protocols", json!({}))] }).is_err());
        assert!(store.delete("movie night"));

        // The Stark Protocols Sir names: study, modding, gaming, house party.
        for (said, name) in [("engage the study protocol", "study"), ("study mode", "study"), ("zeit zu lernen", "study"), ("initiate modding protocol", "modding"), ("time to mod", "modding"), ("gaming protocol", "gaming"), ("party mode", "house party")] {
            assert_eq!(match_utterance(said).as_deref(), Some(name), "{said}");
        }
        assert_eq!(match_utterance("study time"), None, "that phrase is Sir's to teach a macro");
    }

    #[test]
    fn independent_steps_run_side_by_side_and_dependent_ones_keep_their_order() {
        let store = ProtocolStore::in_memory();
        let stages = |name: &str| plan(&store.get(name).unwrap().steps);
        // Gaming: Steam, Discord, the music with its volume, and the reply mode — four lanes.
        assert_eq!(stages("gaming"), vec![vec![vec![0], vec![1], vec![2, 3], vec![4]]]);
        // Modding: two applications, the stats window, the music then its volume.
        assert_eq!(stages("modding"), vec![vec![vec![0], vec![1], vec![2], vec![3, 4]]]);
        // Study: the focus session, the timetable, the course files (a read), the music then its volume.
        assert_eq!(stages("study"), vec![vec![vec![0], vec![1], vec![2], vec![3, 4]]]);
        // House party: Spotify is the player, so its start, the volume stay in order; the colours beside them.
        assert_eq!(stages("house party"), vec![vec![vec![0, 1], vec![2]]]);
        // The player's window is placed after the music step that launches it.
        assert_eq!(stages("music and chill"), vec![vec![vec![0, 1, 2], vec![3]]]);
        // "Minimise everything" waits for all before it and runs alone.
        assert_eq!(stages("clean slate"), vec![vec![vec![0], vec![1]], vec![vec![2]]]);
        // A step that may depend on what came before splits the plan around it; every step is
        // planned exactly once.
        let custom = vec![
            step("open_application", json!({ "app_name": "notepad" })),
            step("open_application", json!({ "app_name": "discord" })),
            step("run_shell_command", json!({ "command": "echo hi" })),
            step("place_application_window", json!({ "app_name": "notepad", "monitor": 2 })),
            step("get_system_status", json!({})),
            step("open_application", json!({})),
        ];
        assert_eq!(plan(&custom), vec![vec![vec![0], vec![1]], vec![vec![2]], vec![vec![3], vec![4]], vec![vec![5]]]);
        let mut all: Vec<usize> = plan(&custom).into_iter().flatten().flatten().collect();
        all.sort_unstable();
        assert_eq!(all, (0..custom.len()).collect::<Vec<_>>());
        assert!(plan(&[]).is_empty());
    }
}
