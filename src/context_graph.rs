//! The working-memory context graph: the people, topics, artifacts, apps and places of the last
//! 25 turns, across every tool. It is what lets Sir talk the way people talk:
//!
//! * "Send that to him on WhatsApp": "that" is the active artifact (the homework JAI just
//!   read, the e-mail summary, the clipboard), "him" the active person.
//! * "What about the day after?": the last date-based query runs again one day later.
//! * "Put it on the other screen and make it bigger": "it" is the active app.
//! * "Reply to him that I'll be there in ten minutes": the sender of the last message read.
//!
//! Tools feed it (`observe_tool`, plus the WhatsApp / location modules directly), Sir's own words
//! feed it (`note_turn`), and every LLM turn sees it as a working-memory section. Pronouns in
//! tool arguments are resolved against it before the tool runs (`resolve_args`).

use crate::contacts::ContactBook;
use crate::tools::ToolOutcome;
use chrono::{Duration as ChronoDuration, Local, NaiveDate};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Entities older than this many turns no longer count as "active".
pub const TURN_WINDOW: u64 = 25;
/// "What about the day after?" refers to a date query this recent.
const ELLIPSIS_TTL: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct ActivePerson {
    pub name: String,
    pub phone: String,
    pub email: String,
    pub whatsapp: String,
    /// "whatsapp", "email", "mention" …
    pub source: String,
    pub turn: u64,
    pub at: Instant,
}

impl ActivePerson {
    pub fn describe(&self) -> String {
        let mut bits = Vec::new();
        if !self.phone.is_empty() {
            bits.push(self.phone.clone());
        }
        if !self.email.is_empty() {
            bits.push(self.email.clone());
        }
        if !self.whatsapp.is_empty() && !self.whatsapp.eq_ignore_ascii_case(&self.name) {
            bits.push(format!("WhatsApp \"{}\"", self.whatsapp));
        }
        if bits.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, bits.join(", "))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    WhatsAppMessage,
    Email,
    Homework,
    Schedule,
    File,
    Url,
    Clipboard,
    Simulation,
    RemoteNode,
    Weather,
    Text,
}

impl ArtifactKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::WhatsAppMessage => "WhatsApp message",
            Self::Email => "e-mail",
            Self::Homework => "homework / course update",
            Self::Schedule => "schedule",
            Self::File => "file",
            Self::Url => "web page",
            Self::Clipboard => "clipboard text",
            Self::Simulation => "simulation",
            Self::RemoteNode => "remote node",
            Self::Weather => "weather report",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub title: String,
    /// The raw data (message text, tool output, path, URL).
    pub content: String,
    /// JAI's own words about it (his reply right after), the best thing to pass on.
    pub summary: String,
    /// Who it came from (a message's sender).
    pub from: Option<String>,
    pub turn: u64,
    pub at: Instant,
}

impl Artifact {
    /// What "send that" sends.
    pub fn shareable(&self) -> String {
        match self.kind {
            ArtifactKind::File | ArtifactKind::Url if !self.content.is_empty() => self.content.clone(),
            _ if !self.summary.trim().is_empty() => self.summary.clone(),
            _ => self.content.chars().take(1500).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveApp {
    pub app: String,
    pub monitor: Option<usize>,
    pub turn: u64,
    pub at: Instant,
}

/// A position fix (phone GPS, home coordinates or IP geolocation).
#[derive(Debug, Clone, PartialEq)]
pub struct GeoFix {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub speed_kmh: Option<f64>,
    pub altitude_m: Option<f64>,
    pub heading: Option<f64>,
    pub address: Option<String>,
    /// "phone GPS", "home", "IP geolocation".
    pub source: String,
    pub at: Instant,
}

/// A query about a day, for "what about the day after?".
#[derive(Debug, Clone, PartialEq)]
pub struct DateQuery {
    pub tool: String,
    /// The argument holding the day.
    pub key: String,
    pub date: NaiveDate,
    pub args: Value,
    pub at: Instant,
}

/// Who "reply to him" answers.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyTarget {
    /// "whatsapp" or "email".
    pub channel: String,
    pub name: String,
    /// E-mail address (for e-mail) or WhatsApp chat name.
    pub address: String,
    pub subject: String,
    pub at: Instant,
}

/// What one turn mentioned.
#[derive(Debug, Clone, Default)]
struct TurnEntities {
    persons: Vec<String>,
    topics: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkingMemoryContextGraph {
    pub active_person_or_contact: Option<ActivePerson>,
    pub active_subject_or_topic: Option<(String, u64)>,
    pub active_artifact: Option<Artifact>,
    pub active_app_and_monitor: Option<ActiveApp>,
    pub live_user_location: Option<GeoFix>,
    pub last_date_query: Option<DateQuery>,
    pub reply_target: Option<ReplyTarget>,
    turns: VecDeque<TurnEntities>,
    turn: u64,
    /// A data tool ran this turn: JAI's reply becomes the artifact's summary.
    artifact_this_turn: bool,
}

#[derive(Clone, Default)]
pub struct ContextGraph {
    inner: Arc<RwLock<WorkingMemoryContextGraph>>,
}

fn fresh(turn: u64, now: u64) -> bool {
    now.saturating_sub(turn) < TURN_WINDOW
}

fn ago(at: Instant) -> String {
    match at.elapsed().as_secs() {
        0..=59 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

/// Words that stand for the active person.
pub fn is_person_pronoun(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    matches!(
        t.trim_matches(|c: char| !c.is_alphanumeric() && c != ' '),
        "" | "him" | "her" | "them" | "he" | "she" | "they" | "ihm" | "ihr" | "ihn" | "sie" | "the sender" | "sender" | "that person" | "this person"
            | "the person" | "them back" | "him back" | "her back" | "dem" | "der" | "die person" | "the guy" | "that guy"
    )
}

/// Words that stand for the active artifact.
pub fn is_thing_pronoun(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    matches!(
        t.trim_matches(|c: char| !c.is_alphanumeric() && c != ' '),
        "" | "that" | "it" | "this" | "those" | "these" | "das" | "es" | "dies" | "diese" | "dieses" | "the message" | "that message" | "the result"
            | "that info" | "the info" | "that information" | "this information" | "the answer" | "that answer" | "the homework" | "my homework"
            | "the summary" | "that summary" | "the schedule" | "die nachricht" | "das ergebnis"
    )
}

impl ContextGraph {
    pub fn new() -> Self {
        Self::default()
    }

    fn read<R>(&self, f: impl FnOnce(&WorkingMemoryContextGraph) -> R) -> R {
        f(&self.inner.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn write<R>(&self, f: impl FnOnce(&mut WorkingMemoryContextGraph) -> R) -> R {
        f(&mut self.inner.write().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn snapshot(&self) -> WorkingMemoryContextGraph {
        self.read(|g| g.clone())
    }

    pub fn clear(&self) {
        let location = self.location();
        self.write(|g| {
            *g = WorkingMemoryContextGraph::default();
            g.live_user_location = location;
        });
    }

    // ------------------------------------------------------------------ updates ----

    pub fn note_person(&self, name: &str, phone: &str, email: &str, whatsapp: &str, source: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.write(|g| {
            let turn = g.turn;
            g.active_person_or_contact = Some(ActivePerson {
                name: name.to_string(),
                phone: phone.to_string(),
                email: email.to_string(),
                whatsapp: whatsapp.to_string(),
                source: source.to_string(),
                turn,
                at: Instant::now(),
            });
        });
    }

    pub fn note_artifact(&self, kind: ArtifactKind, title: &str, content: &str, from: Option<&str>) {
        self.write(|g| {
            let turn = g.turn;
            g.active_artifact = Some(Artifact {
                kind,
                title: title.chars().take(120).collect(),
                content: content.chars().take(6000).collect(),
                summary: String::new(),
                from: from.map(str::to_string),
                turn,
                at: Instant::now(),
            });
            g.artifact_this_turn = true;
        });
    }

    pub fn note_topic(&self, topic: &str) {
        let topic = topic.trim();
        if topic.is_empty() {
            return;
        }
        self.write(|g| g.active_subject_or_topic = Some((crate::pim::capitalise(topic), g.turn)));
    }

    pub fn note_app(&self, app: &str, monitor: Option<usize>) {
        let app = crate::apps::normalize(app);
        if app.is_empty() || crate::focus::is_pronoun(&app) || crate::tools::names_jai(&app) {
            if let Some(m) = monitor {
                self.write(|g| {
                    if let Some(a) = g.active_app_and_monitor.as_mut() {
                        a.monitor = Some(m);
                    }
                });
            }
            return;
        }
        self.write(|g| {
            let keep = g.active_app_and_monitor.as_ref().filter(|a| a.app == app).and_then(|a| a.monitor);
            g.active_app_and_monitor = Some(ActiveApp { app, monitor: monitor.or(keep), turn: g.turn, at: Instant::now() });
        });
    }

    pub fn note_reply_target(&self, channel: &str, name: &str, address: &str, subject: &str) {
        self.write(|g| {
            g.reply_target = Some(ReplyTarget {
                channel: channel.to_string(),
                name: name.to_string(),
                address: address.to_string(),
                subject: subject.to_string(),
                at: Instant::now(),
            })
        });
    }

    pub fn note_date_query(&self, tool: &str, key: &str, date: NaiveDate, args: &Value) {
        self.write(|g| g.last_date_query = Some(DateQuery { tool: tool.to_string(), key: key.to_string(), date, args: args.clone(), at: Instant::now() }));
    }

    pub fn set_location(&self, fix: GeoFix) {
        self.write(|g| g.live_user_location = Some(fix));
    }

    pub fn location(&self) -> Option<GeoFix> {
        self.read(|g| g.live_user_location.clone())
    }

    /// A new turn: the people and topics Sir names become active.
    pub fn note_turn(&self, text: &str, contacts: &ContactBook) {
        let lower = text.to_lowercase();
        let mut entities = TurnEntities::default();
        for c in contacts.list() {
            let names = [&c.name, &c.nickname, &c.whatsapp_name];
            let hit = names.iter().filter(|n| n.chars().count() >= 3).any(|n| {
                let n = n.to_lowercase();
                let first = n.split_whitespace().next().unwrap_or("").to_string();
                contains_word(&lower, &n) || (first.chars().count() >= 3 && contains_word(&lower, &first))
            });
            if hit {
                entities.persons.push(c.name.clone());
                self.note_person(&c.name, &c.phone_number, &c.email, &c.whatsapp_name, "mention");
            }
        }
        for word in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 4) {
            if let Some(subject) = crate::pim::subject_aliases(word).first() {
                if crate::pim::subject_aliases(word).iter().any(|a| a == word) {
                    entities.topics.push(subject.clone());
                }
            }
        }
        for topic in ["minecraft", "homework", "hausaufgaben", "exam", "klausur", "test", "project", "projekt", "server", "motorcycle", "motorrad"] {
            if contains_word(&lower, topic) {
                entities.topics.push(topic.to_string());
            }
        }
        self.write(|g| {
            g.turn += 1;
            g.artifact_this_turn = false;
            if let Some(t) = entities.topics.first() {
                g.active_subject_or_topic = Some((crate::pim::capitalise(t), g.turn));
            }
            g.turns.push_back(entities);
            while g.turns.len() as u64 > TURN_WINDOW {
                g.turns.pop_front();
            }
        });
    }

    /// JAI's reply to a turn that fetched something becomes that thing's summary.
    pub fn note_reply(&self, reply: &str) {
        let reply = reply.trim();
        if reply.is_empty() {
            return;
        }
        self.write(|g| {
            if g.artifact_this_turn {
                if let Some(a) = g.active_artifact.as_mut() {
                    a.summary = reply.chars().take(1500).collect();
                }
            }
        });
    }

    /// Everything the tools did that the conversation may refer to later.
    pub fn observe_tool(&self, outcome: &ToolOutcome, focused_app: Option<String>) {
        if !outcome.success {
            return;
        }
        let args = &outcome.args;
        let out = outcome.output.as_str();
        let arg = |keys: &[&str]| crate::tools::str_arg(args, keys).map(str::to_string);
        let today = Local::now().date_naive();
        let monitor_in = || out.split("Monitor ").nth(1).and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse::<usize>().ok());
        match outcome.tool.as_str() {
            "check_school_schedule" => {
                let raw = arg(&["target_date", "date", "day"]).unwrap_or_else(|| "today".into());
                let range = crate::pim::parse_range(Some(&raw), today);
                self.note_date_query("check_school_schedule", "target_date", range.from, args);
                self.note_artifact(ArtifactKind::Schedule, &format!("school timetable for {}", range.label), out, None);
                self.note_topic("school timetable");
            }
            "get_calendar_schedule" => {
                let raw = arg(&["date_or_range", "date", "range"]).unwrap_or_else(|| "today".into());
                let range = crate::pim::parse_range(Some(&raw), today);
                self.note_date_query("get_calendar_schedule", "date_or_range", range.from, args);
                self.note_artifact(ArtifactKind::Schedule, &format!("calendar for {}", range.label), out, None);
            }
            "get_precise_location_weather" | "check_ride_and_commute_status" => {
                let raw = arg(&["day", "date"]).unwrap_or_else(|| "today".into());
                let day = crate::pim::parse_day(&raw, today).unwrap_or(today);
                self.note_date_query(&outcome.tool, "day", day, args);
                self.note_artifact(ArtifactKind::Weather, "weather at Sir's location", out, None);
            }
            "check_course_updates" => {
                self.note_artifact(ArtifactKind::Homework, "itslearning updates", out, None);
                if let Some(c) = arg(&["course_filter", "course"]) {
                    self.note_topic(&c);
                }
            }
            "sync_study_materials_and_deadlines" => self.note_artifact(ArtifactKind::Homework, "study deadlines", out, None),
            "run_executive_briefing" | "check_all_communications" => self.note_artifact(ArtifactKind::Schedule, "briefing", out, None),
            "summarize_email" => {
                let from = out.strip_prefix("E-mail from ").and_then(|r| r.split(" — ").next()).map(str::trim).map(str::to_string);
                let subject = out.split('"').nth(1).unwrap_or("").to_string();
                self.note_artifact(ArtifactKind::Email, &format!("e-mail \"{subject}\""), out, from.as_deref());
                if let Some(f) = from {
                    let (name, address) = split_address(&f);
                    self.note_person(&name, "", &address, "", "email");
                    self.note_reply_target("email", &name, &address, &subject);
                }
            }
            "clipboard" if !out.starts_with("Copied") => self.note_artifact(ArtifactKind::Clipboard, "clipboard", out, None),
            "inspect_screen_or_selection" | "read_screen" => self.note_artifact(ArtifactKind::Text, "screen text", out, None),
            "web_search" => {
                if let Some(q) = arg(&["query"]) {
                    self.note_topic(&q);
                    self.note_artifact(ArtifactKind::Text, &q, out, None);
                }
            }
            "read_webpage" | "open_website" => {
                let url = arg(&["url", "platform_or_url"]).unwrap_or_default();
                self.note_artifact(ArtifactKind::Url, &url, &url, None);
            }
            "find_files" | "read_text_file" => {
                let path = out.lines().find_map(|l| l.split(" (modified").next().filter(|p| p.contains(['\\', '/']))).map(|p| p.trim_start_matches("Opened ").trim().to_string());
                if let Some(p) = path {
                    self.note_artifact(ArtifactKind::File, &p, &p, None);
                }
            }
            "open_application" | "close_application" | "window_management" | "move_window_to_monitor" | "place_application_window" => {
                let named = arg(&["app_name", "app_or_window_name", "target_window"]).filter(|n| !crate::focus::is_pronoun(n));
                if let Some(app) = named.or(focused_app) {
                    self.note_app(&app, monitor_in());
                }
            }
            "launch_interactive_simulation" => self.note_artifact(ArtifactKind::Simulation, &arg(&["title", "sim_type"]).unwrap_or_default(), out, None),
            "manage_remote_node" => {
                if let Some(n) = arg(&["node_name", "name"]) {
                    self.note_artifact(ArtifactKind::RemoteNode, &n, out, None);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- resolution ----

    /// "What about the day after?", "and tomorrow?", "und am Freitag?": the last date query
    /// again for another day.
    pub fn resolve_ellipsis(&self, text: &str) -> Option<(String, Value)> {
        let q = self.read(|g| g.last_date_query.clone())?;
        if q.at.elapsed() > ELLIPSIS_TTL {
            return None;
        }
        let s: String = text
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == ' ' { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let s = s.trim_start_matches("jai ").trim_end_matches(" jai").trim_end_matches('.').trim().to_string();
        if s.split_whitespace().count() > 8 {
            return None;
        }
        let mut rest = s.as_str();
        let mut had_prefix = false;
        for prefix in [
            "and what about ", "what about ", "how about ", "and how about ", "and for ", "and on ", "and ", "und was ist mit ", "was ist mit ",
            "und wie sieht es ", "wie sieht es ", "und wie ist es ", "und für ", "und am ", "und ", "what s on ", "whats on ", "for ", "on ",
        ] {
            if let Some(r) = rest.strip_prefix(prefix) {
                rest = r.trim();
                had_prefix = true;
                break;
            }
        }
        let rest = rest.trim_end_matches(" then").trim_end_matches(" dann").trim_end_matches(" aus").trim_end_matches(" so").trim();
        let today = Local::now().date_naive();
        let after = ["the day after", "the day after that", "day after", "the next day", "the following day", "next day", "tag danach", "am tag danach", "den tag danach", "dem tag danach", "the day after it"];
        let before = ["the day before", "the previous day", "day before", "the day before that", "tag davor", "am tag davor", "den tag davor"];
        let date = if after.contains(&rest) {
            q.date + ChronoDuration::days(1)
        } else if before.contains(&rest) {
            q.date - ChronoDuration::days(1)
        } else if had_prefix && rest != "it" {
            // "and tomorrow?", "what about Friday?" (a bare "Friday?" is not enough).
            crate::pim::parse_day(rest, today)?
        } else {
            return None;
        };
        let mut args = q.args.clone();
        if !args.is_object() {
            args = json!({});
        }
        args[q.key.as_str()] = json!(date.format("%Y-%m-%d").to_string());
        tracing::info!("[CONTEXT] '{text}' → {} for {date}", q.tool);
        Some((q.tool, args))
    }

    /// Pronouns in a tool's arguments → the active person / artifact.
    pub fn resolve_args(&self, tool: &str, mut args: Value) -> Value {
        let g = self.snapshot();
        let person = g.active_person_or_contact.as_ref().filter(|p| fresh(p.turn, g.turn)).map(|p| p.name.clone());
        let thing = g.active_artifact.as_ref().filter(|a| fresh(a.turn, g.turn)).map(|a| a.shareable());
        let Some(obj) = args.as_object_mut() else { return args };
        let replace_person = |obj: &mut serde_json::Map<String, Value>, key: &str| {
            let value = obj.get(key).and_then(Value::as_str).map(str::to_string);
            if value.as_deref().is_none_or(is_person_pronoun) {
                if let Some(p) = &person {
                    obj.insert(key.into(), json!(p));
                }
            }
        };
        match tool {
            "send_whatsapp_message" => {
                replace_person(obj, "recipient_name_or_phone");
                let text = obj.get("message_text").and_then(Value::as_str).map(str::to_string);
                if text.as_deref().is_none_or(is_thing_pronoun) {
                    if let Some(t) = &thing {
                        obj.insert("message_text".into(), json!(t));
                    }
                }
            }
            "teleport_content" => {
                let dest = obj.get("destination_type").and_then(Value::as_str).unwrap_or("").to_lowercase();
                if matches!(dest.as_str(), "whatsapp" | "email" | "e-mail" | "mail") {
                    replace_person(obj, "recipient");
                }
            }
            _ => {}
        }
        args
    }

    /// The working-memory section of the live context (plain text: bracketed headers invite the
    /// model to write bracketed notes of its own).
    pub fn describe(&self) -> String {
        let g = self.snapshot();
        let mut lines = Vec::new();
        if let Some(p) = g.active_person_or_contact.as_ref().filter(|p| fresh(p.turn, g.turn)) {
            lines.push(format!("Person (\"him\"/\"her\"/\"them\"): {} — via {}, {}", p.describe(), p.source, ago(p.at)));
        }
        if let Some((t, _)) = g.active_subject_or_topic.as_ref().filter(|(_, t)| fresh(*t, g.turn)) {
            lines.push(format!("Topic: {t}"));
        }
        // Everyone and everything else named in the last 25 turns, newest first.
        let mut people: Vec<&str> = Vec::new();
        let mut topics: Vec<&str> = Vec::new();
        for t in g.turns.iter().rev() {
            for p in &t.persons {
                if !people.contains(&p.as_str()) && g.active_person_or_contact.as_ref().is_none_or(|a| a.name != *p) {
                    people.push(p);
                }
            }
            for topic in &t.topics {
                if !topics.contains(&topic.as_str()) {
                    topics.push(topic);
                }
            }
        }
        if !people.is_empty() || topics.len() > 1 {
            lines.push(format!(
                "Also mentioned recently: {}",
                people.iter().chain(topics.iter().skip(1)).take(8).copied().collect::<Vec<_>>().join(", ")
            ));
        }
        if let Some(a) = g.active_artifact.as_ref().filter(|a| fresh(a.turn, g.turn)) {
            let source = if a.summary.is_empty() { a.content.replace('\n', " ") } else { a.summary.clone() };
            let excerpt: String = source.chars().take(220).collect();
            lines.push(format!(
                "Artifact (\"that\"/\"it\"): {} \"{}\"{} — {}: {excerpt}",
                a.kind.label(),
                a.title,
                a.from.as_deref().map(|f| format!(" from {f}")).unwrap_or_default(),
                ago(a.at)
            ));
        }
        if let Some(a) = g.active_app_and_monitor.as_ref().filter(|a| fresh(a.turn, g.turn)) {
            lines.push(format!(
                "App: {}{} ({})",
                crate::apps::display_name(&a.app),
                a.monitor.map(|m| format!(" on Monitor {m}")).unwrap_or_default(),
                ago(a.at)
            ));
        }
        if let Some(q) = g.last_date_query.as_ref().filter(|q| q.at.elapsed() < ELLIPSIS_TTL) {
            lines.push(format!(
                "Last date query: {} for {} (\"the day after\" = {})",
                q.tool,
                q.date.format("%A %-d %B"),
                (q.date + ChronoDuration::days(1)).format("%A %-d %B")
            ));
        }
        if let Some(r) = &g.reply_target {
            lines.push(format!(
                "Reply target (\"reply to him\"): {}{} on {}{}",
                r.name,
                if r.address.is_empty() || r.address == r.name { String::new() } else { format!(" <{}>", r.address) },
                if r.channel == "whatsapp" { "WhatsApp (reply_to_last_whatsapp)" } else { "e-mail" },
                if r.subject.is_empty() { String::new() } else { format!(", re \"{}\"", r.subject) }
            ));
        }
        if let Some(f) = &g.live_user_location {
            lines.push(format!(
                "Sir's location: {:.5}, {:.5} ±{:.0} m ({}, {}){}{}",
                f.lat,
                f.lon,
                f.accuracy_m,
                f.source,
                ago(f.at),
                f.speed_kmh
                    .filter(|s| *s > 3.0)
                    .map(|s| format!(", moving at {s:.0} km/h{}", f.heading.map(|h| format!(" heading {h:.0}°")).unwrap_or_default()))
                    .unwrap_or_default(),
                f.address.as_deref().map(|a| format!(" — {a}")).unwrap_or_default()
            ));
        }
        if lines.is_empty() {
            return String::new();
        }
        format!("Working memory:\n{}", lines.join("\n"))
    }
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before = haystack[..start].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = haystack[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if before && after {
            return true;
        }
        from = end;
    }
    false
}

static SEND_THAT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(?:jai[,\s]+)?(?:please\s+|bitte\s+)?(?:send|forward|share|text|schick|schicke|leite)\s+(that|this|it|das|es|the message|that message|the summary|the result|the info|that info|those|these|my homework|the homework|the schedule|my schedule)\s+(?:to\s+|an\s+)?(.+?)\s+(?:on|via|over|through|per|über|bei|in)\s+whats\s?app(?:\s+weiter)?[.!]?$",
    )
    .expect("valid send-that regex")
});
static SEND_HIM_THAT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(?:jai[,\s]+)?(?:please\s+)?(?:send|text|schick|schicke)\s+(him|her|them|ihm|ihr)\s+(that|this|it|das|es)\s+(?:on|via|over|per|über)\s+whats\s?app[.!]?$")
        .expect("valid send-him-that regex")
});
static REPLY_THAT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(?:jai[,\s]+)?(?:please\s+)?(?:reply|respond|answer|write back|text back)\s+(?:to\s+)?(?:him|her|them|the sender)(?:\s+on\s+whats\s?app)?\s*(?:that|saying|with|:)\s+(.+?)[.!]?$")
        .expect("valid reply regex")
});

/// "Send that to him on WhatsApp", "send Max the schedule on WhatsApp", "reply to him that I'll
/// be there in ten minutes" — resolved against the working memory without an LLM round. None
/// when the memory does not know who or what is meant (the LLM asks Sir instead).
pub fn messaging_command(text: &str, graph: &ContextGraph) -> Option<crate::intents::Intent> {
    use crate::intents::Intent;
    let text = text.trim();
    let g = graph.snapshot();
    let person = g.active_person_or_contact.as_ref().filter(|p| fresh(p.turn, g.turn)).map(|p| p.name.clone());
    let artifact = g.active_artifact.as_ref().filter(|a| fresh(a.turn, g.turn));
    if let Some(c) = REPLY_THAT.captures(text) {
        let mut reply = c[1].trim().to_string();
        if let Some(first) = reply.chars().next() {
            reply = first.to_uppercase().collect::<String>() + &reply[first.len_utf8()..];
        }
        return Some(match g.reply_target.as_ref().filter(|r| r.channel == "email" && !r.address.is_empty()) {
            Some(r) => Intent::Tool {
                name: "teleport_content",
                args: json!({ "source_type": "text", "content": reply, "destination_type": "email", "recipient": r.address, "subject": format!("Re: {}", r.subject) }),
            },
            None => Intent::Tool { name: "reply_to_last_whatsapp", args: json!({ "reply_text": reply }) },
        });
    }
    let (what, who) = if let Some(c) = SEND_THAT.captures(text) {
        (c[1].to_lowercase(), c[2].trim().to_string())
    } else {
        let c = SEND_HIM_THAT.captures(text)?;
        (c[2].to_lowercase(), c[1].to_string())
    };
    let recipient = if is_person_pronoun(&who) { person? } else { who };
    // "the schedule": fetched fresh; "that": what the conversation is about.
    if what.contains("schedule") {
        return Some(Intent::Tool { name: "teleport_content", args: json!({ "source_type": "schedule", "destination_type": "whatsapp", "recipient": recipient }) });
    }
    let content = artifact?.shareable();
    Some(Intent::Tool { name: "send_whatsapp_message", args: json!({ "recipient_name_or_phone": recipient, "message_text": content }) })
}

/// "Max Müller <max@x.de>" → ("Max Müller", "max@x.de").
pub fn split_address(from: &str) -> (String, String) {
    match (from.find('<'), from.find('>')) {
        (Some(a), Some(b)) if b > a => (from[..a].trim().trim_matches('"').to_string(), from[a + 1..b].trim().to_string()),
        _ if from.contains('@') => (from.split('@').next().unwrap_or(from).to_string(), from.trim().to_string()),
        _ => (from.trim().to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(tool: &str, args: Value, output: &str) -> ToolOutcome {
        ToolOutcome {
            tool: tool.into(),
            args,
            output: output.into(),
            success: true,
            duration_ms: 1,
            memories_changed: false,
            skills_changed: false,
            shutdown_requested: false,
            modality_changed: false,
            state_changed: false,
        }
    }

    #[test]
    fn day_after_reruns_the_last_date_query() {
        let g = ContextGraph::new();
        g.observe_tool(&outcome("check_school_schedule", json!({"target_date": "2026-10-23"}), "WebUntis timetable for Friday: …"), None);
        let (tool, args) = g.resolve_ellipsis("What about the day after?").expect("resolved");
        assert_eq!(tool, "check_school_schedule");
        assert_eq!(args["target_date"], "2026-10-24");
        assert_eq!(g.resolve_ellipsis("and the day before?").unwrap().1["target_date"], "2026-10-22");
        assert!(g.resolve_ellipsis("Open Discord").is_none());
        assert!(g.resolve_ellipsis("what about it").is_none());
    }

    #[test]
    fn pronouns_resolve_to_the_active_person_and_artifact() {
        let g = ContextGraph::new();
        let book = ContactBook::in_memory();
        book.upsert(&crate::contacts::Contact { name: "Max".into(), phone_number: "+491511234567".into(), ..Default::default() }, "+49").unwrap();
        g.note_turn("What did Max write?", &book);
        g.observe_tool(&outcome("check_course_updates", json!({}), "Physics: worksheet 4 due Friday"), None);
        g.note_reply("Your physics worksheet 4 is due on Friday, Sir.");
        let args = g.resolve_args("send_whatsapp_message", json!({"recipient_name_or_phone": "him", "message_text": "that"}));
        assert_eq!(args["recipient_name_or_phone"], "Max");
        assert_eq!(args["message_text"], "Your physics worksheet 4 is due on Friday, Sir.");
        let block = g.describe();
        assert!(block.contains("Working memory:") && block.contains("Max") && block.contains("homework"));
        assert_eq!(split_address("Herr Müller <mueller@school.de>"), ("Herr Müller".into(), "mueller@school.de".into()));
    }
}
