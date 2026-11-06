//! The Stark systems hub: one handle for everything added on top of the personal manager — the
//! working-memory context graph, contacts, WhatsApp, the omni-inbox, voice macros, the hivemind
//! (remote nodes + the mobile satellite), location / weather / commute, the Simulation Lab and the
//! fifteen field systems (`features`).
//! It owns their tool schemas and dispatcher (like `pim.rs`) and the event channel the app
//! listens to (core pulses, alerts, message cards, arrival at home, macros to run).

use crate::config::ConfigStore;
use crate::contacts::ContactBook;
use crate::context_graph::ContextGraph;
use crate::features::Features;
use crate::hivemind_mesh::Hivemind;
use crate::integration_registry::IntegrationRegistry;
use crate::location_engine::LocationEngine;
use crate::macros::MacroStore;
use crate::omni_inbox::OmniInbox;
use crate::simulation_lab::SimulationLab;
use crate::tools::ToolContext;
use crate::whatsapp_client::WhatsApp;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tracing::warn;

/// (next id, queued (id, text) entries) for the mobile satellite.
type Outbox = (u64, VecDeque<(u64, String)>);

/// The Integrations hub tab with WhatsApp, the satellite, location and the remote nodes.
pub const QUICK_CONNECTIONS: &str = "quick_connections";

#[derive(Debug, Clone)]
pub enum StarkEvent {
    /// Light a path of nodes in the HUD core (`alert`: crimson).
    Pulse { path: Vec<String>, alert: bool },
    /// Something Sir must hear now.
    Alert(String),
    /// A quiet HUD notice.
    Notice(String),
    /// The phone crossed into the home geofence.
    Arrived { distance_m: f64 },
    /// Run a voice macro's steps one after the other.
    RunMacro { name: String, steps: Vec<String> },
}

#[derive(Clone)]
pub struct StarkSystems {
    pub graph: ContextGraph,
    pub contacts: ContactBook,
    pub whatsapp: WhatsApp,
    pub inbox: OmniInbox,
    pub macros: MacroStore,
    pub hivemind: Hivemind,
    pub location: LocationEngine,
    pub simlab: SimulationLab,
    /// Every integration's last verification (`diagnose_integrations`).
    pub integrations: IntegrationRegistry,
    /// Air-Drop, Instant Replay, Exam Coach, Deal Sniper, Finance, Janitor, Smart Alarm …
    pub features: Features,
    pub events: broadcast::Sender<StarkEvent>,
    /// Content for the mobile satellite (id, text), newest last.
    outbox: Arc<Mutex<Outbox>>,
}

impl StarkSystems {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Self {
        let db = db_path.as_ref();
        let warn_memory = |what: &str, e: String| warn!("[STARK] {what} cannot be persisted ({e}); it lasts for this session only.");
        let contacts = ContactBook::open(db).unwrap_or_else(|e| {
            warn_memory("Contacts", e);
            ContactBook::in_memory()
        });
        let whatsapp = WhatsApp::open(db).unwrap_or_else(|e| {
            warn_memory("WhatsApp messages", e);
            WhatsApp::in_memory()
        });
        let macros = MacroStore::open(db).unwrap_or_else(|e| {
            warn_memory("Voice macros", e);
            MacroStore::in_memory()
        });
        let hivemind = Hivemind::open(db).unwrap_or_else(|e| {
            warn_memory("Remote nodes", e);
            Hivemind::in_memory()
        });
        Self::with(contacts, whatsapp, macros, hivemind, Features::open(db))
    }

    pub fn in_memory() -> Self {
        Self::with(ContactBook::in_memory(), WhatsApp::in_memory(), MacroStore::in_memory(), Hivemind::in_memory(), Features::in_memory())
    }

    fn with(contacts: ContactBook, whatsapp: WhatsApp, macros: MacroStore, hivemind: Hivemind, features: Features) -> Self {
        let (events, _) = broadcast::channel(128);
        Self {
            graph: ContextGraph::new(),
            contacts,
            whatsapp,
            inbox: OmniInbox::default(),
            macros,
            hivemind,
            location: LocationEngine::new(),
            simlab: SimulationLab::default(),
            integrations: IntegrationRegistry::default(),
            features,
            events,
            outbox: Arc::new(Mutex::new((0, VecDeque::new()))),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StarkEvent> {
        self.events.subscribe()
    }

    /// Lights a path of core nodes ("brain" → "whatsapp").
    pub fn pulse(&self, path: &[&str], alert: bool) {
        let _ = self.events.send(StarkEvent::Pulse { path: path.iter().map(|s| s.to_string()).collect(), alert });
    }

    /// Shows text on the mobile satellite.
    pub fn push_to_satellite(&self, text: &str) {
        let mut g = self.outbox.lock().unwrap_or_else(|e| e.into_inner());
        g.0 += 1;
        let id = g.0;
        g.1.push_back((id, text.chars().take(4000).collect()));
        while g.1.len() > 40 {
            g.1.pop_front();
        }
    }

    /// Satellite content newer than `since`.
    pub fn satellite_since(&self, since: u64) -> Vec<(u64, String)> {
        let g = self.outbox.lock().unwrap_or_else(|e| e.into_inner());
        g.1.iter().filter(|(id, _)| *id > since).cloned().collect()
    }

    /// Background work: the WhatsApp listener and the auto-responder.
    pub fn start_background(&self, tools: &ToolContext, config: ConfigStore) {
        self.whatsapp.start_listener(config, tools.pim.config().vault().clone());
        self.inbox.start_background(tools.clone());
        self.features.start_background(tools);
    }

    /// The Stark lines of the live context block.
    pub fn live_context(&self) -> String {
        let mut out = String::new();
        let graph = self.graph.describe();
        if !graph.is_empty() {
            out.push_str(&graph);
            out.push('\n');
        }
        let unread = self.whatsapp.unread_count();
        if unread > 0 {
            out.push_str(&format!("WhatsApp: {unread} unread message{} (check_whatsapp_messages)\n", if unread == 1 { "" } else { "s" }));
        }
        if let Some(line) = self.inbox.describe() {
            out.push_str(&line);
            out.push('\n');
        }
        let sims = self.simlab.list();
        if !sims.is_empty() {
            out.push_str(&format!(
                "Open simulations: {} (adjust_simulation changes their sliders)\n",
                sims.iter()
                    .map(|s| format!("{} \"{}\" [{}]", s.id, s.title, s.params.iter().map(|p| format!("{}={}", p.name, (p.value * 100.0).round() / 100.0)).collect::<Vec<_>>().join(", ")))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let macros = self.macros.list();
        if !macros.is_empty() {
            out.push_str(&format!(
                "Voice macros: {}\n",
                macros.iter().map(|m| format!("\"{}\"", m.trigger_phrases.first().cloned().unwrap_or_else(|| m.name.clone()))).collect::<Vec<_>>().join(", ")
            ));
        }
        let nodes = self.hivemind.list();
        if !nodes.is_empty() {
            out.push_str(&format!("Hivemind nodes: {}\n", nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(", ")));
        }
        out.push_str(&self.features.live_context());
        out
    }
}

// --------------------------------------------------------------------------- tools ----

pub fn is_stark_tool(name: &str) -> bool {
    crate::whatsapp_client::is_tool(name)
        || crate::omni_inbox::is_tool(name)
        || crate::teleport::is_tool(name)
        || crate::macros::is_tool(name)
        || crate::hivemind_mesh::is_tool(name)
        || crate::location_engine::is_tool(name)
        || crate::commute_advisor::is_tool(name)
        || crate::simulation_lab::is_tool(name)
        || crate::integration_registry::is_tool(name)
        || crate::satellite::is_tool(name)
        || crate::features::is_tool(name)
        || name == "manage_contacts"
}

/// Tools that go to the network or other machines (Jarvis acknowledges first).
pub fn is_slow(name: &str) -> bool {
    matches!(
        name,
        "check_whatsapp_messages" | "check_all_communications" | "manage_remote_node" | "get_precise_location_weather" | "find_nearby_places_or_route" | "check_ride_and_commute_status"
            | "diagnose_integrations"
    ) || crate::features::is_slow(name)
}

pub fn tool_definitions() -> Vec<Value> {
    let mut tools = crate::whatsapp_client::tool_definitions();
    tools.push(crate::contacts::tool_definition());
    tools.extend(crate::omni_inbox::tool_definitions());
    tools.push(crate::teleport::tool_definition());
    tools.push(crate::macros::tool_definition());
    tools.push(crate::hivemind_mesh::tool_definition());
    tools.extend(crate::location_engine::tool_definitions());
    tools.push(crate::commute_advisor::tool_definition());
    tools.extend(crate::simulation_lab::tool_definitions());
    tools.push(crate::integration_registry::tool_definition());
    tools.push(crate::satellite::tool_definition());
    tools.extend(crate::features::tool_definitions());
    tools
}

/// The core node of a tool.
pub fn node_of(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "check_whatsapp_messages" | "send_whatsapp_message" | "reply_to_last_whatsapp" | "manage_contacts" | "check_all_communications" | "set_auto_responder" => "whatsapp",
        "teleport_content" | "manage_voice_macro" => "context",
        "manage_remote_node" => "hivemind",
        "get_precise_location_weather" | "find_nearby_places_or_route" | "manage_location_settings" => "gps",
        "check_ride_and_commute_status" => "commute",
        "launch_interactive_simulation" | "adjust_simulation" => "simlab",
        "diagnose_integrations" => "context",
        "show_mobile_satellite_instructions" => "hivemind",
        other => return crate::features::node_of(other),
    })
}

pub async fn run_tool(tools: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    let result = match name {
        n if crate::whatsapp_client::is_tool(n) => crate::whatsapp_client::run_tool(tools, n, args).await,
        "manage_contacts" => {
            tools.stark.pulse(&["brain", "context", "whatsapp"], false);
            let cc = tools.config.read(|c| c.whatsapp.default_country_code.clone());
            crate::contacts::tool(&tools.stark.contacts, args, &cc)
        }
        n if crate::omni_inbox::is_tool(n) => crate::omni_inbox::run_tool(tools, n, args).await,
        n if crate::teleport::is_tool(n) => crate::teleport::run_tool(tools, args).await,
        n if crate::macros::is_tool(n) => crate::macros::run_tool(tools, args),
        n if crate::hivemind_mesh::is_tool(n) => crate::hivemind_mesh::run_tool(tools, args).await,
        n if crate::location_engine::is_tool(n) => crate::location_engine::run_tool(tools, n, args).await,
        n if crate::commute_advisor::is_tool(n) => crate::commute_advisor::run_tool(tools, args).await,
        n if crate::integration_registry::is_tool(n) => crate::integration_registry::run_tool(tools).await,
        n if crate::satellite::is_tool(n) => crate::satellite::run_tool(tools).await,
        n if crate::features::is_tool(n) => crate::features::run_tool(tools, n, args).await,
        n if crate::simulation_lab::is_tool(n) => {
            tools.stark.pulse(&["brain", "simlab", "windows"], false);
            crate::simulation_lab::run_tool(&tools.stark.simlab, n, args)
        }
        other => Err(format!("Unknown Stark tool '{other}'.")),
    };
    if result.is_err() {
        if let Some(node) = node_of(name) {
            tools.stark.pulse(&["brain", node], true);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stark_tool_is_routed_and_has_a_node() {
        for def in tool_definitions() {
            let name = def["function"]["name"].as_str().unwrap();
            assert!(is_stark_tool(name), "{name}");
            assert!(node_of(name).is_some(), "{name}");
        }
        let s = StarkSystems::in_memory();
        s.push_to_satellite("one");
        s.push_to_satellite("two");
        assert_eq!(s.satellite_since(1), vec![(2, "two".to_string())]);
    }
}
