//! The Deep Research Engine: a multi-hour, crash-resilient research agent ("research
//! solid-state batteries for three hours, focus on cost and safety").
//!
//! Everything but the final synthesis is pure CPU work, so a mission keeps crawling while Sir
//! games and the GPU is empty (`vram_guard`):
//! 1. a query tree (10-30 sub-questions: architecture, benchmarks, papers, forums, edge cases,
//!    history; the language model adds a few when it may run),
//! 2. a polite multi-source crawler (DuckDuckGo HTML, Wikipedia REST, arXiv, Hacker News,
//!    GitHub, then outbound links up to depth 3) with rotating browser user agents, a 12 s
//!    timeout, per-domain rate limits and robots.txt,
//! 3. HTML → clean Markdown (scripts, styles, navigation, footers and ads stripped), domain
//!    authority scores and extracted claims with corroboration / contradiction checks,
//! 4. a telemetry stream every second for the workspace windows and a live multi-chapter
//!    dossier, rewritten every minute and finished by the language model once the GPU is free.
//!
//! Every page, fact and counter is checkpointed in `jai_memory.db`; a mission interrupted by a
//! crash or shutdown resumes on the next start.

use crate::features::{safe_name, spoken, str_arg, user_folder};
use crate::stark::StarkEvent;
use crate::tools::ToolContext;
use regex::Regex;
use reqwest::Url;
use rusqlite::{params, Connection, OptionalExtension};
use scraper::{ElementRef, Html, Node, Selector};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

const MAX_DEPTH: i64 = 3;
const TELEMETRY_CAP: usize = 800;
/// Pages crawled from the frontier between two search queries.
const PAGES_PER_QUERY: usize = 4;
const MAX_QUERIES: i64 = 320;
const MAX_FACTS_PER_PAGE: usize = 8;
const MAX_BODY_BYTES: usize = 3 * 1024 * 1024;
const GLOBAL_GAP: Duration = Duration::from_millis(700);
const DOMAIN_GAP: Duration = Duration::from_millis(2200);

const RUNNING: u8 = 0;
const PAUSED: u8 = 1;
const STOPPING: u8 = 2;

const USER_AGENTS: [&str; 5] = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6_1) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 Edg/129.0.0.0",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36",
];
/// APIs get an honest agent.
const API_AGENT: &str = "JAI-DeepResearch/3.1 (local personal research assistant)";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS research_missions (
    id INTEGER PRIMARY KEY,
    topic TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    target_duration_secs INTEGER NOT NULL,
    elapsed_secs INTEGER NOT NULL DEFAULT 0,
    pages_crawled INTEGER NOT NULL DEFAULT 0,
    facts_extracted INTEGER NOT NULL DEFAULT 0,
    executive_summary TEXT NOT NULL DEFAULT '',
    focus_areas TEXT NOT NULL DEFAULT '',
    dossier_markdown TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS research_queries (
    id INTEGER PRIMARY KEY,
    mission_id INTEGER NOT NULL,
    sub_question TEXT NOT NULL,
    search_query TEXT NOT NULL,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS research_queries_mission ON research_queries(mission_id, status, priority);
CREATE TABLE IF NOT EXISTS research_sources (
    id INTEGER PRIMARY KEY,
    mission_id INTEGER NOT NULL,
    url TEXT NOT NULL,
    domain TEXT NOT NULL,
    title TEXT NOT NULL,
    reliability_score REAL NOT NULL,
    raw_markdown TEXT NOT NULL,
    crawled_at TEXT NOT NULL,
    query_id INTEGER,
    depth INTEGER NOT NULL DEFAULT 0,
    UNIQUE(mission_id, url)
);
CREATE TABLE IF NOT EXISTS research_facts (
    id INTEGER PRIMARY KEY,
    mission_id INTEGER NOT NULL,
    source_id INTEGER NOT NULL,
    category TEXT NOT NULL,
    claim_text TEXT NOT NULL,
    confidence REAL NOT NULL,
    corroborating_count INTEGER NOT NULL DEFAULT 0,
    contradicting_source_id INTEGER
);
CREATE INDEX IF NOT EXISTS research_facts_mission ON research_facts(mission_id);
CREATE TABLE IF NOT EXISTS research_frontier (
    id INTEGER PRIMARY KEY,
    mission_id INTEGER NOT NULL,
    url TEXT NOT NULL,
    depth INTEGER NOT NULL,
    score REAL NOT NULL,
    query_id INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    UNIQUE(mission_id, url)
);
CREATE INDEX IF NOT EXISTS research_frontier_next ON research_frontier(mission_id, status, score);
";

// ------------------------------------------------------------------------- telemetry ----

#[derive(Debug, Clone, Serialize)]
pub enum TelemetryKind {
    Stage { stage: String, detail: String },
    Query { provider: String, query: String, results: usize },
    Fetch { url: String, status: u16, ms: u64, bytes: usize },
    Snippet { url: String, text: String },
    Fact { category: String, claim: String, confidence: f32, corroborated: bool, contradiction: bool },
    Progress { elapsed_secs: u64, target_secs: u64, pages: u64, facts: u64, pages_per_min: f32, frontier: u64, queries_left: u64 },
    Error { url: String, message: String },
    Finished { status: String, summary: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchTelemetryEvent {
    /// Local time (HH:MM:SS).
    pub at: String,
    pub mission_id: i64,
    pub kind: TelemetryKind,
}

/// Live counters of a running mission (the ticker turns them into `Progress` events).
#[derive(Debug, Clone, Default)]
pub struct LiveProgress {
    pub stage: String,
    pub elapsed_secs: u64,
    pub target_secs: u64,
    pub pages: u64,
    pub facts: u64,
    pub frontier: u64,
    pub queries_left: u64,
    crawl_times: VecDeque<Instant>,
}

impl LiveProgress {
    pub fn pages_per_min(&self) -> f32 {
        self.crawl_times.iter().filter(|t| t.elapsed() < Duration::from_secs(60)).count() as f32
    }
}

pub struct ResearchEngine {
    db_path: String,
    read: Mutex<Option<Connection>>,
    controls: Mutex<HashMap<i64, Arc<AtomicU8>>>,
    live: Mutex<HashMap<i64, LiveProgress>>,
    telemetry: Mutex<VecDeque<ResearchTelemetryEvent>>,
    /// The latest `Progress` event per mission (kept apart so the once-a-second ticks never
    /// push crawl events out of the feed).
    progress: Mutex<HashMap<i64, ResearchTelemetryEvent>>,
    revision: AtomicU64,
}

static ENGINE: OnceLock<ResearchEngine> = OnceLock::new();

pub fn engine() -> Option<&'static ResearchEngine> {
    ENGINE.get()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

impl ResearchEngine {
    pub fn emit(&self, mission_id: i64, kind: TelemetryKind) {
        let event = ResearchTelemetryEvent { at: chrono::Local::now().format("%H:%M:%S").to_string(), mission_id, kind };
        if matches!(event.kind, TelemetryKind::Progress { .. }) {
            lock(&self.progress).insert(mission_id, event);
        } else {
            let mut t = lock(&self.telemetry);
            if t.len() >= TELEMETRY_CAP {
                t.pop_front();
            }
            t.push_back(event);
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    /// The last once-a-second `Progress` event of a mission.
    pub fn last_progress(&self, mission_id: i64) -> Option<ResearchTelemetryEvent> {
        lock(&self.progress).get(&mission_id).cloned()
    }

    /// Changes whenever telemetry arrives (the windows repaint).
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// The last `n` events (of one mission, or all).
    pub fn recent_events(&self, mission_id: Option<i64>, n: usize) -> Vec<ResearchTelemetryEvent> {
        let t = lock(&self.telemetry);
        let mut out: Vec<ResearchTelemetryEvent> = t.iter().rev().filter(|e| mission_id.is_none_or(|m| e.mission_id == m)).take(n).cloned().collect();
        out.reverse();
        out
    }

    pub fn live(&self, mission_id: i64) -> Option<LiveProgress> {
        lock(&self.live).get(&mission_id).cloned()
    }

    fn update_live(&self, mission_id: i64, f: impl FnOnce(&mut LiveProgress)) {
        f(lock(&self.live).entry(mission_id).or_default());
    }

    pub fn is_running(&self, mission_id: i64) -> bool {
        lock(&self.controls).contains_key(&mission_id)
    }

    fn with_read<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> Option<R> {
        let mut slot = lock(&self.read);
        if slot.is_none() {
            let conn = Connection::open(&self.db_path).ok()?;
            let _ = conn.busy_timeout(Duration::from_secs(2));
            *slot = Some(conn);
        }
        f(slot.as_ref()?).ok()
    }
}

/// Creates the tables and resumes every mission a crash or shutdown interrupted.
pub fn start(db_path: &str, tools: ToolContext) {
    match Connection::open(db_path).and_then(|c| c.execute_batch(SCHEMA)) {
        Ok(()) => {}
        Err(e) => {
            warn!("[RESEARCH] Deep research tables unavailable: {e}");
            return;
        }
    }
    let instance = ResearchEngine {
        db_path: db_path.to_string(),
        read: Mutex::new(None),
        controls: Mutex::new(HashMap::new()),
        live: Mutex::new(HashMap::new()),
        telemetry: Mutex::new(VecDeque::new()),
        progress: Mutex::new(HashMap::new()),
        revision: AtomicU64::new(0),
    };
    if ENGINE.set(instance).is_err() {
        return;
    }
    let interrupted: Vec<i64> = engine().and_then(|e| e.with_read(interrupted_missions)).unwrap_or_default();
    for id in interrupted {
        info!("[RESEARCH] Resuming interrupted mission #{id} from its checkpoint.");
        launch(&tools, id);
    }
}

fn launch(tools: &ToolContext, mission_id: i64) {
    let Some(engine) = engine() else { return };
    let control = Arc::new(AtomicU8::new(RUNNING));
    {
        let mut controls = lock(&engine.controls);
        if controls.contains_key(&mission_id) {
            return;
        }
        controls.insert(mission_id, Arc::clone(&control));
    }
    tokio::spawn(ticker(mission_id));
    tokio::spawn(run_mission(tools.clone(), mission_id, control));
}

/// One `Progress` event per second while the mission runs.
async fn ticker(mission_id: i64) {
    let Some(engine) = engine() else { return };
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        if !engine.is_running(mission_id) {
            return;
        }
        if let Some(p) = engine.live(mission_id) {
            engine.emit(
                mission_id,
                TelemetryKind::Progress {
                    elapsed_secs: p.elapsed_secs,
                    target_secs: p.target_secs,
                    pages: p.pages,
                    facts: p.facts,
                    pages_per_min: p.pages_per_min(),
                    frontier: p.frontier,
                    queries_left: p.queries_left,
                },
            );
        }
    }
}

// --------------------------------------------------------------------------- storage ----

#[derive(Debug, Clone)]
pub struct MissionRow {
    pub id: i64,
    pub topic: String,
    pub status: String,
    pub created_at: String,
    pub target_secs: i64,
    pub elapsed_secs: i64,
    pub pages: i64,
    pub facts: i64,
    pub executive_summary: String,
    pub focus_areas: String,
}

#[derive(Debug, Clone)]
pub struct QueryRow {
    pub id: i64,
    pub sub_question: String,
    pub search_query: String,
    pub status: String,
    pub priority: i64,
}

#[derive(Debug, Clone)]
pub struct SourceRow {
    pub id: i64,
    pub url: String,
    pub domain: String,
    pub title: String,
    pub reliability: f32,
    pub query_id: Option<i64>,
    pub crawled_at: String,
    pub depth: i64,
}

#[derive(Debug, Clone)]
pub struct FactRow {
    pub id: i64,
    pub source_id: i64,
    pub category: String,
    pub claim: String,
    pub confidence: f32,
    pub corroborating: i64,
    pub contradicting_source: Option<i64>,
}

/// Everything the dossier, the graph and the crawler windows show of one mission.
#[derive(Debug, Clone)]
pub struct MissionSnapshot {
    pub mission: MissionRow,
    pub queries: Vec<QueryRow>,
    pub sources: Vec<SourceRow>,
    pub facts: Vec<FactRow>,
}

impl MissionSnapshot {
    pub fn source(&self, id: i64) -> Option<&SourceRow> {
        self.sources.iter().find(|s| s.id == id)
    }
}

const MISSION_COLUMNS: &str = "id, topic, status, created_at, target_duration_secs, elapsed_secs, pages_crawled, facts_extracted, executive_summary, focus_areas";

fn mission_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<MissionRow> {
    Ok(MissionRow {
        id: r.get(0)?,
        topic: r.get(1)?,
        status: r.get(2)?,
        created_at: r.get(3)?,
        target_secs: r.get(4)?,
        elapsed_secs: r.get(5)?,
        pages: r.get(6)?,
        facts: r.get(7)?,
        executive_summary: r.get(8)?,
        focus_areas: r.get(9)?,
    })
}

fn read_snapshot(c: &Connection, mission_id: i64, max_facts: usize) -> rusqlite::Result<Option<MissionSnapshot>> {
    let Some(mission) = c
        .query_row(&format!("SELECT {MISSION_COLUMNS} FROM research_missions WHERE id = ?1"), [mission_id], mission_from)
        .optional()?
    else {
        return Ok(None);
    };
    let mut st = c.prepare("SELECT id, sub_question, search_query, status, priority FROM research_queries WHERE mission_id = ?1 ORDER BY priority DESC, id")?;
    let queries = st
        .query_map([mission_id], |r| Ok(QueryRow { id: r.get(0)?, sub_question: r.get(1)?, search_query: r.get(2)?, status: r.get(3)?, priority: r.get(4)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = c.prepare(
        "SELECT id, url, domain, title, reliability_score, query_id, crawled_at, depth FROM research_sources WHERE mission_id = ?1 ORDER BY reliability_score DESC, id",
    )?;
    let sources = st
        .query_map([mission_id], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                url: r.get(1)?,
                domain: r.get(2)?,
                title: r.get(3)?,
                reliability: r.get::<_, f64>(4)? as f32,
                query_id: r.get(5)?,
                crawled_at: r.get(6)?,
                depth: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = c.prepare(
        "SELECT id, source_id, category, claim_text, confidence, corroborating_count, contradicting_source_id FROM research_facts
         WHERE mission_id = ?1 ORDER BY (contradicting_source_id IS NOT NULL) DESC, corroborating_count DESC, confidence DESC LIMIT ?2",
    )?;
    let facts = st
        .query_map(params![mission_id, max_facts as i64], |r| {
            Ok(FactRow {
                id: r.get(0)?,
                source_id: r.get(1)?,
                category: r.get(2)?,
                claim: r.get(3)?,
                confidence: r.get::<_, f64>(4)? as f32,
                corroborating: r.get(5)?,
                contradicting_source: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(MissionSnapshot { mission, queries, sources, facts }))
}

/// A mission's data for the windows (`max_facts` strongest facts first).
pub fn snapshot(mission_id: i64, max_facts: usize) -> Option<MissionSnapshot> {
    engine()?.with_read(|c| read_snapshot(c, mission_id, max_facts)).flatten()
}

/// The newest mission (running ones first).
pub fn latest_mission_id() -> Option<i64> {
    engine()?
        .with_read(|c| {
            c.query_row(
                "SELECT id FROM research_missions ORDER BY (status IN ('running', 'paused', 'synthesizing')) DESC, id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
        })
        .flatten()
}

/// The stored Markdown of a page (the inspector window).
pub fn source_markdown(source_id: i64) -> Option<(String, String)> {
    engine()?
        .with_read(|c| c.query_row("SELECT url, raw_markdown FROM research_sources WHERE id = ?1", [source_id], |r| Ok((r.get(0)?, r.get(1)?))).optional())
        .flatten()
}

pub fn list_missions(limit: usize) -> Vec<MissionRow> {
    engine()
        .and_then(|e| {
            e.with_read(|c| {
                let mut st = c.prepare(&format!("SELECT {MISSION_COLUMNS} FROM research_missions ORDER BY id DESC LIMIT ?1"))?;
                let rows = st.query_map([limit as i64], mission_from)?.collect::<rusqlite::Result<Vec<_>>>();
                rows
            })
        })
        .unwrap_or_default()
}

/// The mission task's own connection (writes).
struct MissionDb {
    conn: Connection,
}

impl MissionDb {
    fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    fn mission(&self, id: i64) -> Option<MissionRow> {
        self.conn.query_row(&format!("SELECT {MISSION_COLUMNS} FROM research_missions WHERE id = ?1"), [id], mission_from).ok()
    }

    fn set_status(&self, id: i64, status: &str) {
        let _ = self.conn.execute("UPDATE research_missions SET status = ?2, updated_at = ?3 WHERE id = ?1", params![id, status, now_text()]);
    }

    fn checkpoint(&self, id: i64, elapsed: u64) {
        let _ = self.conn.execute(
            "UPDATE research_missions SET elapsed_secs = ?2, updated_at = ?3,
                 pages_crawled = (SELECT COUNT(*) FROM research_sources WHERE mission_id = ?1),
                 facts_extracted = (SELECT COUNT(*) FROM research_facts WHERE mission_id = ?1)
             WHERE id = ?1",
            params![id, elapsed as i64, now_text()],
        );
    }

    fn counts(&self, id: i64) -> (u64, u64, u64, u64) {
        let one = |sql: &str| self.conn.query_row(sql, [id], |r| r.get::<_, i64>(0)).unwrap_or(0).max(0) as u64;
        (
            one("SELECT COUNT(*) FROM research_sources WHERE mission_id = ?1"),
            one("SELECT COUNT(*) FROM research_facts WHERE mission_id = ?1"),
            one("SELECT COUNT(*) FROM research_frontier WHERE mission_id = ?1 AND status = 'pending'"),
            one("SELECT COUNT(*) FROM research_queries WHERE mission_id = ?1 AND status = 'pending'"),
        )
    }

    fn query_count(&self, id: i64) -> i64 {
        self.conn.query_row("SELECT COUNT(*) FROM research_queries WHERE mission_id = ?1", [id], |r| r.get(0)).unwrap_or(0)
    }

    fn has_query(&self, id: i64, search: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM research_queries WHERE mission_id = ?1 AND lower(search_query) = lower(?2)", params![id, search], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    fn add_query(&self, id: i64, q: &PlannedQuery) -> bool {
        if self.has_query(id, &q.search) {
            return false;
        }
        self.conn
            .execute(
                "INSERT INTO research_queries (mission_id, sub_question, search_query, status, priority) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![id, q.question, q.search, q.priority],
            )
            .is_ok()
    }

    fn next_query(&self, id: i64) -> Option<QueryRow> {
        self.conn
            .query_row(
                "SELECT id, sub_question, search_query, status, priority FROM research_queries WHERE mission_id = ?1 AND status = 'pending' ORDER BY priority DESC, id LIMIT 1",
                [id],
                |r| Ok(QueryRow { id: r.get(0)?, sub_question: r.get(1)?, search_query: r.get(2)?, status: r.get(3)?, priority: r.get(4)? }),
            )
            .optional()
            .ok()
            .flatten()
    }

    fn finish_query(&self, query_id: i64, status: &str) {
        let _ = self.conn.execute("UPDATE research_queries SET status = ?2 WHERE id = ?1", params![query_id, status]);
    }

    fn known_url(&self, id: i64, url: &str) -> bool {
        let hit = |sql: &str| self.conn.query_row(sql, params![id, url], |_| Ok(())).optional().ok().flatten().is_some();
        hit("SELECT 1 FROM research_sources WHERE mission_id = ?1 AND url = ?2") || hit("SELECT 1 FROM research_frontier WHERE mission_id = ?1 AND url = ?2")
    }

    fn push_frontier(&self, id: i64, url: &str, depth: i64, score: f32, query_id: Option<i64>) {
        if depth > MAX_DEPTH || self.known_url(id, url) {
            return;
        }
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO research_frontier (mission_id, url, depth, score, query_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, url, depth, score as f64, query_id],
        );
    }

    fn next_frontier(&self, id: i64) -> Option<(i64, String, i64, Option<i64>, f32)> {
        self.conn
            .query_row(
                "SELECT id, url, depth, query_id, score FROM research_frontier WHERE mission_id = ?1 AND status = 'pending' ORDER BY score DESC, depth, id LIMIT 1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, f64>(4)? as f32)),
            )
            .optional()
            .ok()
            .flatten()
    }

    fn mark_frontier(&self, frontier_id: i64, status: &str) {
        let _ = self.conn.execute("UPDATE research_frontier SET status = ?2 WHERE id = ?1", params![frontier_id, status]);
    }

    fn add_source(&self, id: i64, page: &StoredPage) -> Option<i64> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO research_sources (mission_id, url, domain, title, reliability_score, raw_markdown, crawled_at, query_id, depth)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, page.url, page.domain, page.title, page.reliability as f64, page.markdown, now_text(), page.query_id, page.depth],
        );
        match inserted {
            Ok(1) => Some(self.conn.last_insert_rowid()),
            _ => None,
        }
    }

    fn add_fact(&self, id: i64, source_id: i64, f: &Candidate, corroborating: i64, contradicting: Option<i64>) -> Option<i64> {
        self.conn
            .execute(
                "INSERT INTO research_facts (mission_id, source_id, category, claim_text, confidence, corroborating_count, contradicting_source_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, source_id, f.category, f.claim, f.confidence as f64, corroborating, contradicting],
            )
            .ok()
            .map(|_| self.conn.last_insert_rowid())
    }

    fn corroborate(&self, fact_id: i64) {
        let _ = self.conn.execute("UPDATE research_facts SET corroborating_count = corroborating_count + 1 WHERE id = ?1", [fact_id]);
    }

    fn contradict(&self, fact_id: i64, source_id: i64) {
        let _ = self.conn.execute(
            "UPDATE research_facts SET contradicting_source_id = ?2 WHERE id = ?1 AND contradicting_source_id IS NULL",
            params![fact_id, source_id],
        );
    }

    fn fact_index_rows(&self, id: i64) -> Vec<(i64, i64, String, String)> {
        let Ok(mut st) = self.conn.prepare(
            "SELECT f.id, f.source_id, s.domain, f.claim_text FROM research_facts f JOIN research_sources s ON s.id = f.source_id
             WHERE f.mission_id = ?1 ORDER BY f.id DESC LIMIT 5000",
        ) else {
            return Vec::new();
        };
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map(|rows| rows.flatten().collect()).unwrap_or_default();
        rows
    }

    fn save_dossier(&self, id: i64, markdown: &str, summary: Option<&str>) {
        let _ = self.conn.execute("UPDATE research_missions SET dossier_markdown = ?2, updated_at = ?3 WHERE id = ?1", params![id, markdown, now_text()]);
        if let Some(s) = summary {
            let _ = self.conn.execute("UPDATE research_missions SET executive_summary = ?2 WHERE id = ?1", params![id, s]);
        }
    }
}

// ----------------------------------------------------------------- stage 1: query tree ----

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedQuery {
    pub question: String,
    pub search: String,
    pub priority: i64,
}

/// The facets every topic is researched along (sub-question, query, priority).
const FACETS: [(&str, &str, i64); 18] = [
    ("What is {t} and how does it work?", "{t} overview explained", 100),
    ("How is {t} built technically?", "{t} architecture technical design", 95),
    ("How does {t} perform in benchmarks?", "{t} benchmark performance comparison", 92),
    ("What does the academic research say about {t}?", "{t} research paper", 90),
    ("Which surveys summarise {t}?", "{t} survey review", 85),
    ("What do practitioners report about {t}?", "{t} discussion experience", 82),
    ("Which implementations of {t} exist?", "{t} github implementation", 80),
    ("Where does {t} fail or hit edge cases?", "{t} limitations edge cases problems", 78),
    ("What are the main criticisms of {t}?", "{t} criticism drawbacks", 75),
    ("How did {t} develop historically?", "history of {t}", 72),
    ("What is the current state of {t}?", "{t} latest developments {y}", 70),
    ("Where is {t} heading?", "{t} future outlook roadmap", 66),
    ("How does {t} compare to its alternatives?", "{t} vs alternatives comparison", 64),
    ("Which numbers and statistics describe {t}?", "{t} statistics data numbers", 62),
    ("What does {t} cost?", "{t} cost pricing", 58),
    ("Which risks and security issues does {t} have?", "{t} security risks safety", 56),
    ("What are the best practices for {t}?", "{t} best practices guide", 54),
    ("What does the official documentation say about {t}?", "{t} official documentation", 52),
];

/// 18 facet queries plus three per focus area, at most 30.
pub fn plan_queries(topic: &str, focus: &[String]) -> Vec<PlannedQuery> {
    let year = chrono::Local::now().format("%Y").to_string();
    let fill = |s: &str| s.replace("{t}", topic.trim()).replace("{y}", &year);
    let mut out: Vec<PlannedQuery> = Vec::new();
    for f in focus.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        for (q, s, p) in [
            (format!("What matters about {topic} regarding {f}?"), format!("{topic} {f}"), 99),
            (format!("How does {topic} measure up on {f}?"), format!("{topic} {f} benchmark data"), 93),
            (format!("What does research say about {f} in {topic}?"), format!("{topic} {f} research"), 88),
        ] {
            out.push(PlannedQuery { question: q, search: s, priority: p });
        }
    }
    for (q, s, p) in FACETS {
        out.push(PlannedQuery { question: fill(q), search: fill(s), priority: p });
    }
    let mut seen = HashSet::new();
    out.retain(|q| seen.insert(q.search.to_lowercase()));
    out.truncate(30);
    out
}

// --------------------------------------------------------------- stage 3: html → md ----

const SKIP_TAGS: [&str; 17] = [
    "script", "style", "noscript", "nav", "footer", "header", "aside", "form", "iframe", "svg", "button", "select", "input", "template", "canvas",
    "dialog", "object",
];
const SKIP_TOKENS: [&str; 30] = [
    "ad", "ads", "advert", "advertisement", "adsbygoogle", "sponsored", "sponsor", "promo", "cookie", "cookies", "consent", "banner", "newsletter",
    "subscribe", "social", "share", "sharing", "sidebar", "breadcrumb", "breadcrumbs", "navbox", "navbar", "nav", "menu", "popup", "modal",
    "footer", "toc", "editsection", "reflist",
];

fn skipped(el: &ElementRef<'_>) -> bool {
    let v = el.value();
    if SKIP_TAGS.contains(&v.name()) {
        return true;
    }
    if v.attr("aria-hidden") == Some("true") || v.attr("hidden").is_some() || v.attr("role").is_some_and(|r| matches!(r, "navigation" | "banner" | "contentinfo")) {
        return true;
    }
    let marks = format!("{} {}", v.attr("class").unwrap_or(""), v.attr("id").unwrap_or("")).to_lowercase();
    marks.split(|c: char| c.is_whitespace() || c == '-' || c == '_').any(|t| SKIP_TOKENS.contains(&t))
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text of an element without the skipped parts (inline `code` in backticks).
fn inline_text(el: ElementRef<'_>) -> String {
    let mut out = String::new();
    fn walk(el: ElementRef<'_>, out: &mut String) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => {
                    out.push_str(t);
                }
                Node::Element(_) => {
                    if let Some(c) = ElementRef::wrap(child) {
                        if skipped(&c) {
                            continue;
                        }
                        match c.value().name() {
                            "br" => out.push(' '),
                            "code" => {
                                out.push_str(" `");
                                out.push_str(&collapse(&c.text().collect::<String>()));
                                out.push_str("` ");
                            }
                            "sup" if c.value().attr("class").is_some_and(|k| k.contains("reference")) => {}
                            _ => walk(c, out),
                        }
                    }
                }
                _ => {}
            }
        }
    }
    walk(el, &mut out);
    collapse(&out).replace(" ,", ",").replace(" .", ".")
}

fn table_markdown(el: ElementRef<'_>) -> String {
    static ROW: LazyLock<Selector> = LazyLock::new(|| Selector::parse("tr").unwrap());
    static CELL: LazyLock<Selector> = LazyLock::new(|| Selector::parse("th, td").unwrap());
    let rows: Vec<Vec<String>> = el
        .select(&ROW)
        .take(40)
        .map(|tr| tr.select(&CELL).take(8).map(|c| inline_text(c).replace('|', "/")).collect::<Vec<_>>())
        .filter(|r: &Vec<String>| r.iter().any(|c| !c.is_empty()))
        .collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.len() < 2 || width == 0 {
        return String::new();
    }
    let mut md = String::from("\n\n");
    for (i, row) in rows.iter().enumerate() {
        let mut cells = row.clone();
        cells.resize(width, String::new());
        md.push_str(&format!("| {} |\n", cells.join(" | ")));
        if i == 0 {
            md.push_str(&format!("|{}\n", " --- |".repeat(width)));
        }
    }
    md.push('\n');
    md
}

fn block_markdown(el: ElementRef<'_>, out: &mut String) {
    for child in el.children() {
        let Some(c) = ElementRef::wrap(child) else {
            if let Node::Text(t) = child.value() {
                let t = collapse(t);
                if !t.is_empty() {
                    out.push(' ');
                    out.push_str(&t);
                }
            }
            continue;
        };
        if skipped(&c) {
            continue;
        }
        match c.value().name() {
            h @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                let text = inline_text(c);
                if !text.is_empty() {
                    let level = h[1..].parse::<usize>().unwrap_or(2);
                    out.push_str(&format!("\n\n{} {text}\n\n", "#".repeat(level)));
                }
            }
            "p" | "dd" | "dt" | "figcaption" | "summary" => {
                let text = inline_text(c);
                if !text.is_empty() {
                    out.push_str(&format!("\n\n{text}\n\n"));
                }
            }
            "li" => {
                let text = inline_text(c);
                if !text.is_empty() {
                    out.push_str(&format!("\n- {text}"));
                }
            }
            "pre" => {
                let code: String = c.text().collect();
                let code = code.trim_matches('\n');
                if !code.trim().is_empty() {
                    out.push_str(&format!("\n\n```\n{}\n```\n\n", code.chars().take(4000).collect::<String>()));
                }
            }
            "blockquote" => {
                let text = inline_text(c);
                if !text.is_empty() {
                    out.push_str(&format!("\n\n> {text}\n\n"));
                }
            }
            "table" => out.push_str(&table_markdown(c)),
            "br" => out.push('\n'),
            "img" | "picture" | "video" | "audio" | "map" | "link" | "meta" => {}
            _ => block_markdown(c, out),
        }
    }
}

/// A fetched page as clean Markdown with its outbound links.
#[derive(Debug, Clone, Default)]
pub struct CleanPage {
    pub title: String,
    pub markdown: String,
    pub links: Vec<(String, String)>,
}

/// HTML → Markdown: the main content (article / main / the largest content block), headings,
/// paragraphs, lists, code and tables; scripts, styles, navigation, footers and ads dropped.
pub fn html_to_markdown(html: &str, base: Option<&Url>) -> CleanPage {
    static TITLE: LazyLock<Selector> = LazyLock::new(|| Selector::parse("title, h1").unwrap());
    static MAIN: LazyLock<Selector> = LazyLock::new(|| {
        Selector::parse("article, main, [role=main], #mw-content-text, #content, .content, .post, .entry-content, .markdown-body, #readme").unwrap()
    });
    static BODY: LazyLock<Selector> = LazyLock::new(|| Selector::parse("body").unwrap());
    static LINK: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[href]").unwrap());
    let doc = Html::parse_document(html);
    let title = doc.select(&TITLE).map(|t| collapse(&t.text().collect::<String>())).find(|t| !t.is_empty()).unwrap_or_default();
    let text_len = |e: &ElementRef<'_>| e.text().map(str::len).sum::<usize>();
    let root = doc
        .select(&MAIN)
        .filter(|e| !skipped(e))
        .max_by_key(|e| text_len(e))
        .filter(|e| text_len(e) > 400)
        .or_else(|| doc.select(&BODY).next());
    let Some(root) = root else {
        return CleanPage { title, ..Default::default() };
    };
    let mut md = String::new();
    block_markdown(root, &mut md);
    // Tidy: trimmed lines, at most one blank line in a row.
    let mut tidy = String::new();
    let mut blank = 0;
    for line in md.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        tidy.push_str(line.trim_start_matches(' '));
        tidy.push('\n');
    }
    let links = root
        .select(&LINK)
        .filter_map(|a| {
            let href = a.value().attr("href")?.trim();
            if href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") {
                return None;
            }
            let abs = match base {
                Some(b) => b.join(href).ok()?,
                None => Url::parse(href).ok()?,
            };
            matches!(abs.scheme(), "http" | "https").then(|| (normalize_url(abs.as_str()), collapse(&a.text().collect::<String>())))
        })
        .collect();
    CleanPage { title, markdown: tidy.trim().to_string(), links }
}

/// Drops fragments, tracking parameters and a trailing slash.
pub fn normalize_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw.trim()) else { return raw.trim().to_string() };
    url.set_fragment(None);
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !(k.starts_with("utm_") || matches!(k.as_ref(), "ref" | "fbclid" | "gclid" | "mc_cid" | "mc_eid")))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    let path = url.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        url.set_path(path.trim_end_matches('/'));
    }
    url.to_string()
}

pub fn domain_of(url: &str) -> String {
    Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_lowercase())).unwrap_or_default()
}

/// Domain authority 0-1: universities, governments, arXiv, GitHub, Wikipedia and official docs
/// get +0.3 on the 0.5 base; content farms and social sites lose.
pub fn reliability(url: &str) -> f32 {
    let domain = domain_of(url);
    let path = Url::parse(url).map(|u| u.path().to_lowercase()).unwrap_or_default();
    let ends = |s: &str| domain == s || domain.ends_with(&format!(".{s}"));
    let mut score: f32 = 0.5;
    let authority = domain.ends_with(".edu")
        || domain.ends_with(".gov")
        || domain.contains(".ac.")
        || domain.contains(".gov.")
        || ["arxiv.org", "github.com", "raw.githubusercontent.com", "wikipedia.org", "nature.com", "science.org", "ieee.org", "acm.org", "nih.gov", "who.int", "europa.eu", "springer.com", "sciencedirect.com", "mozilla.org", "rust-lang.org", "python.org", "kernel.org", "w3.org", "learn.microsoft.com", "nvidia.com"]
            .iter()
            .any(|d| ends(d))
        || domain.starts_with("docs.")
        || domain.starts_with("developer.")
        || domain.contains("readthedocs")
        || path.starts_with("/docs");
    if authority {
        score += 0.3;
    }
    if ends("stackoverflow.com") || ends("stackexchange.com") {
        score += 0.15;
    }
    if ends("news.ycombinator.com") {
        score += 0.05;
    }
    if ends("medium.com") || ends("substack.com") {
        score -= 0.05;
    }
    if ["quora.com", "pinterest.com", "facebook.com", "tiktok.com", "instagram.com", "answers.com", "ehow.com"].iter().any(|d| ends(d)) {
        score -= 0.25;
    }
    score.clamp(0.05, 0.98)
}

fn blocked_domain(domain: &str) -> bool {
    ["duckduckgo.com", "google.", "bing.com", "youtube.com", "youtu.be", "facebook.com", "instagram.com", "tiktok.com", "twitter.com", "x.com", "pinterest.", "linkedin.com", "accounts.", "login."]
        .iter()
        .any(|b| domain == *b || domain.contains(b))
}

fn media_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or("");
    [".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".mp4", ".mp3", ".zip", ".gz", ".tar", ".exe", ".msi", ".dmg", ".iso", ".woff", ".css", ".js"]
        .iter()
        .any(|e| path.ends_with(e))
}

// ------------------------------------------------------------------ fact extraction ----

/// A claim found on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub category: String,
    pub claim: String,
    pub confidence: f32,
}

const STOPWORDS: [&str; 64] = [
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "were", "been", "have", "has", "had", "into", "than", "then", "they", "their",
    "them", "which", "while", "will", "would", "could", "should", "about", "also", "more", "most", "such", "some", "what", "when", "where", "there",
    "these", "those", "only", "other", "over", "under", "between", "after", "before", "because", "each", "many", "much", "very", "your", "you",
    "its", "can", "may", "not", "but", "all", "any", "how", "why", "who", "our", "out",
];

/// Lower-case content words (≥ 3 letters, no stop words, a plural "s" dropped).
pub fn terms(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(w))
        .map(|w| if w.len() > 4 && w.ends_with('s') && !w.ends_with("ss") { w[..w.len() - 1].to_string() } else { w.to_string() })
        .collect()
}

/// Sentence split on . ! ? followed by a capital, a digit or a quote (abbreviations kept).
pub fn split_sentences(text: &str) -> Vec<String> {
    const ABBREV: [&str; 14] = ["e.g", "i.e", "etc", "vs", "dr", "mr", "mrs", "fig", "no", "approx", "al", "inc", "ltd", "cf"];
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '.' | '!' | '?') && i + 2 < chars.len() && chars[i + 1].is_whitespace() {
            let next = chars[i + 2];
            let word: String = chars[start..i].iter().rev().take_while(|ch| !ch.is_whitespace()).collect::<Vec<_>>().into_iter().rev().collect();
            let abbrev = ABBREV.contains(&word.to_lowercase().trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '.')) || (word.chars().count() == 1 && c == '.');
            if !abbrev && (next.is_uppercase() || next.is_ascii_digit() || matches!(next, '"' | '“' | '(' | '[')) {
                let s: String = chars[start..=i].iter().collect();
                out.push(s.trim().to_string());
                start = i + 1;
            }
        }
        i += 1;
    }
    let rest: String = chars[start.min(chars.len())..].iter().collect();
    if !rest.trim().is_empty() {
        out.push(rest.trim().to_string());
    }
    out
}

static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(\d+(?:[.,]\d+)?)\s*(%|percent|x\b|×|gb|mb|kb|tb|ghz|mhz|ms|µs|ns|fps|w\b|kw|mw|kwh|wh|v\b|mah|km|m\b|cm|mm|nm|kg|g\b|°c|k\b|tokens?|params?|parameters|billion|million|thousand|usd|eur|\$|€|years?|hours?|minutes?|seconds?)?").unwrap());
static YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(1[89]\d\d|20\d\d)\b").unwrap());

fn category_of(lower: &str, has_number: bool) -> &'static str {
    let any = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if any(&["benchmark", "faster", "slower", "throughput", "latency", "fps", "outperform", "speedup", "speed-up", "score of", "accuracy"]) {
        "benchmark"
    } else if any(&["however", "limitation", "drawback", "cannot", "fails", "failure", "problem", "risk", "criticism", "downside", "vulnerab", "issue"]) {
        "limitation"
    } else if YEAR.is_match(lower) && any(&["founded", "introduced", "released", "invented", "launched", "first", "announced", "began", "developed in", "since"]) {
        "history"
    } else if any(&[" is a ", " is an ", " refers to ", " defined as ", " is the ", " are a "]) {
        "definition"
    } else if any(&["architecture", "design", "component", "layer", "module", "protocol", "algorithm", "pipeline", "consists of", "built on", "uses a"]) {
        "architecture"
    } else if has_number && NUMBER.captures_iter(lower).any(|c| c.get(2).is_some()) {
        "statistic"
    } else {
        "claim"
    }
}

/// The strongest claims of a page about the topic (pure heuristics, no language model).
pub fn extract_facts(markdown: &str, title: &str, topic_terms: &[String], reliability: f32) -> Vec<Candidate> {
    const NOISE: [&str; 12] = ["cookie", "subscribe", "sign up", "log in", "javascript", "privacy policy", "all rights reserved", "click here", "©", "newsletter", "terms of service", "advertis"];
    let wanted: HashSet<&str> = topic_terms.iter().map(String::as_str).collect();
    let title_terms: HashSet<String> = terms(title).into_iter().collect();
    let on_topic_page = wanted.iter().any(|t| title_terms.contains(*t));
    let mut found: Vec<(f32, Candidate)> = Vec::new();
    let mut seen = HashSet::new();
    let mut in_code = false;
    for block in markdown.split("\n\n") {
        let block = block.trim();
        if block.starts_with("```") {
            in_code = !block.ends_with("```") || block.len() < 6;
            continue;
        }
        if in_code {
            if block.ends_with("```") {
                in_code = false;
            }
            continue;
        }
        if block.starts_with('#') || block.starts_with('|') || block.is_empty() {
            continue;
        }
        let text = block.trim_start_matches("- ").trim_start_matches("> ").replace('\n', " ");
        for sentence in split_sentences(&text) {
            let len = sentence.chars().count();
            let lower = sentence.to_lowercase();
            if !(50..=420).contains(&len) || sentence.split_whitespace().count() < 7 || NOISE.iter().any(|n| lower.contains(n)) {
                continue;
            }
            let words: HashSet<String> = terms(&sentence).into_iter().collect();
            let hits = wanted.iter().filter(|t| words.contains(**t)).count();
            if hits == 0 && !on_topic_page {
                continue;
            }
            let relevance = hits as f32 / wanted.len().max(1) as f32;
            let has_number = lower.chars().any(|c| c.is_ascii_digit());
            let has_unit = NUMBER.captures_iter(&lower).any(|c| c.get(2).is_some());
            let category = category_of(&lower, has_number);
            let score = 2.0 * relevance
                + if has_number { 0.6 } else { 0.0 }
                + if has_unit { 0.4 } else { 0.0 }
                + if category != "claim" { 0.3 } else { 0.0 }
                + if on_topic_page { 0.2 } else { 0.0 };
            let key = words.iter().take(12).cloned().collect::<Vec<_>>().join(" ");
            if !seen.insert(key) {
                continue;
            }
            let confidence = (0.25 + 0.45 * reliability + 0.3 * (score / 3.0).min(1.0)).clamp(0.0, 1.0);
            found.push((score, Candidate { category: category.to_string(), claim: sentence, confidence }));
        }
    }
    found.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    found.into_iter().take(MAX_FACTS_PER_PAGE).map(|(_, c)| c).collect()
}

/// A stored fact, reduced for comparisons.
struct IndexedFact {
    id: i64,
    source_id: i64,
    domain: String,
    words: HashSet<String>,
    numbers: Vec<(f64, String)>,
    negated: bool,
}

fn numbers_of(text: &str) -> Vec<(f64, String)> {
    NUMBER
        .captures_iter(&text.to_lowercase())
        .filter_map(|c| {
            let unit = c.get(2)?.as_str().trim().to_string();
            let v = c.get(1)?.as_str().replace(',', ".").parse::<f64>().ok()?;
            Some((v, unit))
        })
        .collect()
}

fn negated(text: &str) -> bool {
    let lower = format!(" {} ", text.to_lowercase());
    [" not ", " no ", " never ", "n't ", " cannot ", " without ", " fails "].iter().any(|n| lower.contains(n))
}

/// Corroboration / contradiction detection across sources.
#[derive(Default)]
struct FactIndex {
    facts: Vec<IndexedFact>,
}

/// (facts of other sites that agree, the first that contradicts).
#[derive(Debug, Default, PartialEq)]
pub struct Verdict {
    pub agreeing: Vec<i64>,
    pub contradicting: Option<(i64, i64)>,
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

/// Two claims about the same thing (≥ 45 % shared words) contradict when their numbers with the
/// same unit differ by more than 25 % or one negates the other; they agree at ≥ 50 % otherwise.
fn compare(words: &HashSet<String>, numbers: &[(f64, String)], neg: bool, other: &IndexedFact) -> Option<bool> {
    let j = jaccard(words, &other.words);
    if j < 0.45 {
        return None;
    }
    let numeric_conflict = numbers.iter().any(|(v, u)| {
        other.numbers.iter().any(|(w, ou)| ou == u && {
            let hi = v.abs().max(w.abs());
            hi > 0.0 && (v - w).abs() / hi > 0.25
        })
    });
    let shared_number = numbers.iter().any(|(v, u)| other.numbers.iter().any(|(w, ou)| ou == u && (v - w).abs() < f64::EPSILON.max(v.abs() * 0.02)));
    if (numeric_conflict && !shared_number) || (neg != other.negated && j >= 0.6) {
        Some(false)
    } else if j >= 0.5 {
        Some(true)
    } else {
        None
    }
}

impl FactIndex {
    fn load(db: &MissionDb, mission_id: i64) -> Self {
        let mut index = Self::default();
        for (id, source_id, domain, claim) in db.fact_index_rows(mission_id) {
            index.insert(id, source_id, domain, &claim);
        }
        index
    }

    fn insert(&mut self, id: i64, source_id: i64, domain: String, claim: &str) {
        if self.facts.len() >= 5000 {
            self.facts.remove(0);
        }
        self.facts.push(IndexedFact { id, source_id, domain, words: terms(claim).into_iter().collect(), numbers: numbers_of(claim), negated: negated(claim) });
    }

    fn judge(&self, domain: &str, claim: &str) -> Verdict {
        let words: HashSet<String> = terms(claim).into_iter().collect();
        let numbers = numbers_of(claim);
        let neg = negated(claim);
        let mut v = Verdict::default();
        for other in self.facts.iter().filter(|f| f.domain != domain) {
            match compare(&words, &numbers, neg, other) {
                Some(true) => v.agreeing.push(other.id),
                Some(false) if v.contradicting.is_none() => v.contradicting = Some((other.id, other.source_id)),
                _ => {}
            }
        }
        v
    }
}

// --------------------------------------------------------------------- the crawler ----

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .connect_timeout(Duration::from_secs(6))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn browser_agent() -> &'static str {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    USER_AGENTS[(NEXT.fetch_add(1, Ordering::Relaxed) as usize) % USER_AGENTS.len()]
}

/// Polite pacing: one request every 0.7 s overall, every 2.2 s per site (6.5 s for GitHub's
/// API, 3 s for arXiv's), and robots.txt.
#[derive(Default)]
struct Pacer {
    last: Option<Instant>,
    per_domain: HashMap<String, Instant>,
    robots: HashMap<String, Vec<String>>,
}

impl Pacer {
    async fn wait(&mut self, domain: &str) {
        let gap = match domain {
            "api.github.com" => Duration::from_millis(6500),
            "export.arxiv.org" => Duration::from_secs(3),
            _ => DOMAIN_GAP,
        };
        let now = Instant::now();
        let mut until = self.last.map(|l| l + GLOBAL_GAP).unwrap_or(now);
        if let Some(d) = self.per_domain.get(domain) {
            until = until.max(*d + gap);
        }
        if until > now {
            tokio::time::sleep(until - now).await;
        }
        let at = Instant::now();
        self.last = Some(at);
        self.per_domain.insert(domain.to_string(), at);
    }

    /// Whether robots.txt lets any crawler fetch `url` (the `*` group's Disallow prefixes).
    async fn allowed(&mut self, url: &Url) -> bool {
        let Some(host) = url.host_str().map(str::to_string) else { return false };
        if !self.robots.contains_key(&host) {
            let rules = match http().get(format!("{}://{host}/robots.txt", url.scheme())).header("User-Agent", API_AGENT).timeout(Duration::from_secs(6)).send().await {
                Ok(r) if r.status().is_success() => parse_robots(&r.text().await.unwrap_or_default()),
                _ => Vec::new(),
            };
            self.robots.insert(host.clone(), rules);
        }
        let path = url.path();
        !self.robots.get(&host).is_some_and(|rules| rules.iter().any(|p| !p.is_empty() && path.starts_with(p.as_str())))
    }
}

pub fn parse_robots(text: &str) -> Vec<String> {
    let mut rules = Vec::new();
    let mut applies = false;
    let mut in_agents = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_lowercase(), v.trim());
        match k.as_str() {
            "user-agent" => {
                if !in_agents {
                    applies = false;
                }
                in_agents = true;
                applies |= v == "*";
            }
            "disallow" => {
                in_agents = false;
                if applies && !v.is_empty() {
                    rules.push(v.trim_end_matches('*').to_string());
                }
            }
            _ => in_agents = false,
        }
    }
    rules
}

struct Fetched {
    url: String,
    status: u16,
    ms: u64,
    content_type: String,
    body: Vec<u8>,
}

async fn fetch(url: &str, agent: &str, accept: &str) -> Result<Fetched, String> {
    let t0 = Instant::now();
    let resp = http().get(url).header("User-Agent", agent).header("Accept", accept).header("Accept-Language", "en-US,en;q=0.8,de;q=0.6").send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    if resp.content_length().is_some_and(|n| n as usize > MAX_BODY_BYTES * 2) {
        return Err(format!("too large ({} bytes)", resp.content_length().unwrap_or(0)));
    }
    let mut body = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
    body.truncate(MAX_BODY_BYTES * 2);
    Ok(Fetched { url: final_url, status, ms: t0.elapsed().as_millis() as u64, content_type, body })
}

/// A page ready for the database.
struct StoredPage {
    url: String,
    domain: String,
    title: String,
    reliability: f32,
    markdown: String,
    query_id: Option<i64>,
    depth: i64,
}

/// A result a provider found: either a page to crawl, or a ready source (API summaries).
enum Found {
    Link { url: String, score: f32 },
    Source { url: String, title: String, markdown: String },
}

async fn ddg(pacer: &mut Pacer, query: &str) -> Result<Vec<Found>, String> {
    static RESULT: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".result").unwrap());
    static LINK: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a.result__a").unwrap());
    pacer.wait("html.duckduckgo.com").await;
    let page = fetch(&format!("https://html.duckduckgo.com/html/?q={}", urlencoding::encode(query)), browser_agent(), "text/html").await?;
    if page.status >= 400 {
        return Err(format!("HTTP {}", page.status));
    }
    let html = String::from_utf8_lossy(&page.body).to_string();
    let doc = Html::parse_document(&html);
    let mut out = Vec::new();
    for (rank, result) in doc.select(&RESULT).filter(|r| !r.value().attr("class").unwrap_or("").contains("result--ad")).enumerate().take(8) {
        let Some(href) = result.select(&LINK).next().and_then(|a| a.value().attr("href")) else { continue };
        let target = if let Some(pos) = href.find("uddg=") {
            let enc = href[pos + 5..].split('&').next().unwrap_or("");
            urlencoding::decode(enc).map(|s| s.into_owned()).unwrap_or_default()
        } else if href.starts_with("http") {
            href.to_string()
        } else {
            continue;
        };
        if target.starts_with("http") {
            out.push(Found::Link { score: 0.62 - rank as f32 * 0.03 + 0.2 * (reliability(&target) - 0.5), url: normalize_url(&target) });
        }
    }
    Ok(out)
}

async fn wikipedia(pacer: &mut Pacer, query: &str) -> Result<Vec<Found>, String> {
    pacer.wait("en.wikipedia.org").await;
    let search = fetch(
        &format!("https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=3&format=json&utf8=1", urlencoding::encode(query)),
        API_AGENT,
        "application/json",
    )
    .await?;
    let v: Value = serde_json::from_slice(&search.body).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for hit in v["query"]["search"].as_array().into_iter().flatten().take(2) {
        let Some(title) = hit["title"].as_str() else { continue };
        let slug = title.replace(' ', "_");
        pacer.wait("en.wikipedia.org").await;
        if let Ok(summary) = fetch(&format!("https://en.wikipedia.org/api/rest_v1/page/summary/{}", urlencoding::encode(&slug)), API_AGENT, "application/json").await {
            let s: Value = serde_json::from_slice(&summary.body).unwrap_or_default();
            if let Some(extract) = s["extract"].as_str().filter(|e| e.len() > 80) {
                out.push(Found::Source {
                    url: format!("https://en.wikipedia.org/wiki/{slug}#summary"),
                    title: format!("{title} (Wikipedia summary)"),
                    markdown: format!("# {title}\n\n{}\n\n{extract}", s["description"].as_str().unwrap_or("")),
                });
            }
        }
        // The full article is crawled too.
        out.push(Found::Link { url: format!("https://en.wikipedia.org/wiki/{slug}"), score: 0.78 });
    }
    Ok(out)
}

async fn arxiv(pacer: &mut Pacer, query: &str) -> Result<Vec<Found>, String> {
    static ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<entry>(.*?)</entry>").unwrap());
    static FIELD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<(id|title|summary|published)>(.*?)</(?:id|title|summary|published)>").unwrap());
    static AUTHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<name>(.*?)</name>").unwrap());
    pacer.wait("export.arxiv.org").await;
    let feed = fetch(
        &format!("https://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results=4&sortBy=relevance", urlencoding::encode(query)),
        API_AGENT,
        "application/atom+xml",
    )
    .await?;
    let xml = String::from_utf8_lossy(&feed.body).to_string();
    let mut out = Vec::new();
    for entry in ENTRY.captures_iter(&xml) {
        let body = &entry[1];
        let mut fields: HashMap<String, String> = HashMap::new();
        for f in FIELD.captures_iter(body) {
            fields.entry(f[1].to_string()).or_insert_with(|| collapse(&xml_unescape(&f[2])));
        }
        let authors: Vec<String> = AUTHOR.captures_iter(body).take(4).map(|a| collapse(&a[1])).collect();
        let (Some(id), Some(title)) = (fields.get("id"), fields.get("title")) else { continue };
        let url = id.replace("http://", "https://");
        out.push(Found::Source {
            url: normalize_url(&url),
            title: title.clone(),
            markdown: format!(
                "# {title}\n\n*{}{} — published {}*\n\n## Abstract\n\n{}",
                authors.join(", "),
                if authors.len() >= 4 { " et al." } else { "" },
                fields.get("published").map(|p| p.chars().take(10).collect::<String>()).unwrap_or_default(),
                fields.get("summary").cloned().unwrap_or_default()
            ),
        });
    }
    Ok(out)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&")
}

async fn hacker_news(pacer: &mut Pacer, query: &str) -> Result<Vec<Found>, String> {
    pacer.wait("hn.algolia.com").await;
    let r = fetch(&format!("https://hn.algolia.com/api/v1/search?query={}&tags=story&hitsPerPage=6", urlencoding::encode(query)), API_AGENT, "application/json").await?;
    let v: Value = serde_json::from_slice(&r.body).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut discussions = 0;
    for hit in v["hits"].as_array().into_iter().flatten() {
        let points = hit["points"].as_i64().unwrap_or(0);
        if let Some(url) = hit["url"].as_str().filter(|u| u.starts_with("http")) {
            out.push(Found::Link { url: normalize_url(url), score: 0.5 + (points as f32 / 2000.0).min(0.2) });
        }
        // The forum discussion of the best-received story, through the item API (the site's
        // own pages ask crawlers to stay away).
        let Some(id) = hit["objectID"].as_str().filter(|_| discussions == 0 && points >= 20 && hit["num_comments"].as_i64().unwrap_or(0) >= 10) else { continue };
        discussions += 1;
        pacer.wait("hn.algolia.com").await;
        let Ok(item) = fetch(&format!("https://hn.algolia.com/api/v1/items/{id}"), API_AGENT, "application/json").await else { continue };
        let tree: Value = serde_json::from_slice(&item.body).unwrap_or_default();
        let comments: Vec<String> = tree["children"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .map(|html| html_to_markdown(&format!("<html><body><div>{html}</div></body></html>"), None).markdown.replace('\n', " "))
            .filter(|t| t.len() > 60)
            .take(15)
            .collect();
        if !comments.is_empty() {
            let title = hit["title"].as_str().unwrap_or("discussion");
            out.push(Found::Source {
                url: format!("https://news.ycombinator.com/item?id={id}"),
                title: format!("HN discussion: {title}"),
                markdown: format!("# {title}\n\n*Hacker News, {points} points*\n\n{}", comments.iter().map(|c| format!("{c}\n\n")).collect::<String>()),
            });
        }
    }
    Ok(out)
}

async fn github(pacer: &mut Pacer, query: &str) -> Result<Vec<Found>, String> {
    pacer.wait("api.github.com").await;
    let r = fetch(&format!("https://api.github.com/search/repositories?q={}&sort=stars&order=desc&per_page=4", urlencoding::encode(query)), API_AGENT, "application/vnd.github+json").await?;
    if r.status == 403 || r.status == 429 {
        return Err("GitHub rate limit reached".into());
    }
    let v: Value = serde_json::from_slice(&r.body).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (rank, repo) in v["items"].as_array().into_iter().flatten().enumerate() {
        let (Some(name), Some(url)) = (repo["full_name"].as_str(), repo["html_url"].as_str()) else { continue };
        // The README of the two most-starred repositories (the API's raw media type).
        let mut readme = String::new();
        if rank < 2 {
            pacer.wait("api.github.com").await;
            if let Ok(r) = fetch(&format!("https://api.github.com/repos/{name}/readme"), API_AGENT, "application/vnd.github.raw").await {
                if r.status < 300 {
                    readme = String::from_utf8_lossy(&r.body).chars().take(40_000).collect();
                }
            }
        }
        out.push(Found::Source {
            url: url.to_string(),
            title: format!("{name} (GitHub)"),
            markdown: format!(
                "# {name}\n\n{}\n\n- Stars: {}\n- Forks: {}\n- Language: {}\n- Open issues: {}\n- Last push: {}\n\n{readme}",
                repo["description"].as_str().unwrap_or(""),
                repo["stargazers_count"].as_i64().unwrap_or(0),
                repo["forks_count"].as_i64().unwrap_or(0),
                repo["language"].as_str().unwrap_or("—"),
                repo["open_issues_count"].as_i64().unwrap_or(0),
                repo["pushed_at"].as_str().unwrap_or("").chars().take(10).collect::<String>()
            ),
        });
    }
    Ok(out)
}

/// The providers a query goes to: the web always, the encyclopedia for background, arXiv for
/// research and benchmarks, Hacker News and GitHub for practice.
fn providers_for(q: &QueryRow) -> Vec<&'static str> {
    let s = format!("{} {}", q.sub_question, q.search_query).to_lowercase();
    let mut p = vec!["duckduckgo"];
    if q.priority >= 95 || ["history", "what is", "overview", "explained", "definition"].iter().any(|w| s.contains(w)) {
        p.push("wikipedia");
    }
    if ["research", "paper", "survey", "benchmark", "academic", "data"].iter().any(|w| s.contains(w)) {
        p.push("arxiv");
    }
    if ["discussion", "experience", "practitioner", "criticism", "limitations", "problems"].iter().any(|w| s.contains(w)) {
        p.push("hackernews");
    }
    if ["github", "implementation", "architecture", "official documentation", "best practices"].iter().any(|w| s.contains(w)) {
        p.push("github");
    }
    p
}

// ---------------------------------------------------------------------- the mission ----

/// Stores a page, extracts and cross-checks its claims, queues its best links. Returns the
/// number of facts.
fn ingest_page(db: &MissionDb, index: &mut FactIndex, mission_id: i64, topic_terms: &[String], page: StoredPage, links: &[(String, String)]) -> usize {
    let Some(source_id) = db.add_source(mission_id, &page) else { return 0 };
    let emit = |kind: TelemetryKind| {
        if let Some(e) = engine() {
            e.emit(mission_id, kind);
        }
    };
    if let Some(e) = engine() {
        e.update_live(mission_id, |p| {
            p.crawl_times.push_back(Instant::now());
            while p.crawl_times.len() > 400 {
                p.crawl_times.pop_front();
            }
        });
    }
    let facts = extract_facts(&page.markdown, &page.title, topic_terms, page.reliability);
    if let Some(first) = facts.first() {
        emit(TelemetryKind::Snippet { url: page.url.clone(), text: first.claim.chars().take(220).collect() });
    }
    for f in &facts {
        let verdict = index.judge(&page.domain, &f.claim);
        let contradicting = verdict.contradicting.map(|(_, source)| source);
        let Some(fact_id) = db.add_fact(mission_id, source_id, f, verdict.agreeing.len() as i64, contradicting) else { continue };
        for other in &verdict.agreeing {
            db.corroborate(*other);
        }
        if let Some((other_fact, _)) = verdict.contradicting {
            db.contradict(other_fact, source_id);
        }
        index.insert(fact_id, source_id, page.domain.clone(), &f.claim);
        emit(TelemetryKind::Fact {
            category: f.category.clone(),
            claim: f.claim.chars().take(240).collect(),
            confidence: f.confidence,
            corroborated: !verdict.agreeing.is_empty(),
            contradiction: contradicting.is_some(),
        });
    }
    // Outbound references of good pages, up to depth 3.
    if page.depth < MAX_DEPTH && (page.reliability >= 0.55 || facts.len() >= 3) {
        let mut queued = 0;
        for (url, anchor) in links {
            if queued >= 8 {
                break;
            }
            let domain = domain_of(url);
            if domain.is_empty() || blocked_domain(&domain) || media_url(url) || url == &page.url {
                continue;
            }
            let words: HashSet<String> = terms(&format!("{anchor} {url}")).into_iter().collect();
            let hits = topic_terms.iter().filter(|t| words.contains(*t)).count();
            if hits == 0 {
                continue;
            }
            let score = 0.25 + 0.1 * hits.min(3) as f32 + 0.25 * reliability(url) + 0.1 * page.reliability - 0.08 * page.depth as f32;
            db.push_frontier(mission_id, url, page.depth + 1, score, page.query_id);
            queued += 1;
        }
    }
    facts.len()
}

/// The mission's topic and focus words.
fn mission_terms(row: &MissionRow) -> Vec<String> {
    let mut t = terms(&row.topic);
    for f in row.focus_areas.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        t.extend(terms(f));
    }
    t.sort();
    t.dedup();
    t
}

/// Adds a page JAI already has (his own knowledge, an encyclopedia summary) to a mission:
/// stored, its facts extracted and cross-checked like any crawled page. Returns the source id.
pub fn add_local_source(mission_id: i64, url: &str, title: &str, markdown: &str, reliability: f32) -> Option<i64> {
    let db = MissionDb::open(&engine()?.db_path).ok()?;
    let row = db.mission(mission_id)?;
    let mut index = FactIndex::load(&db, mission_id);
    let page = StoredPage { url: url.to_string(), domain: domain_of(url), title: title.to_string(), reliability: reliability.clamp(0.05, 0.98), markdown: markdown.to_string(), query_id: None, depth: 0 };
    ingest_page(&db, &mut index, mission_id, &mission_terms(&row), page, &[]);
    db.conn.query_row("SELECT id FROM research_sources WHERE mission_id = ?1 AND url = ?2", params![mission_id, url], |r| r.get(0)).ok()
}

/// The mission researching exactly this topic (resumed when it was interrupted), or a new one.
pub fn start_or_reuse_mission(tools: &ToolContext, topic: &str, minutes: u64) -> Result<i64, String> {
    let lower = topic.trim().to_lowercase();
    if let Some(m) = list_missions(40).into_iter().find(|m| m.topic.trim().to_lowercase() == lower) {
        let running = engine().is_some_and(|e| e.is_running(m.id));
        if !running && !matches!(m.status.as_str(), "completed" | "stopped") {
            launch(tools, m.id);
        }
        return Ok(m.id);
    }
    let id = create_mission(topic.trim(), minutes.clamp(10, 720), &[])?;
    launch(tools, id);
    Ok(id)
}

/// A Wikipedia article's summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WikiBrief {
    pub title: String,
    pub description: String,
    pub extract: String,
    pub url: String,
}

/// The best-matching English Wikipedia article and its REST summary.
pub async fn wikipedia_brief(topic: &str) -> Option<WikiBrief> {
    let search = fetch(
        &format!("https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=1&format=json&utf8=1", urlencoding::encode(topic)),
        API_AGENT,
        "application/json",
    )
    .await
    .ok()?;
    let v: Value = serde_json::from_slice(&search.body).ok()?;
    let found = v["query"]["search"][0]["title"].as_str()?.to_string();
    let slug = found.replace(' ', "_");
    let summary = fetch(&format!("https://en.wikipedia.org/api/rest_v1/page/summary/{}", urlencoding::encode(&slug)), API_AGENT, "application/json").await.ok()?;
    let s: Value = serde_json::from_slice(&summary.body).ok()?;
    let extract = s["extract"].as_str().filter(|e| e.len() > 40)?.to_string();
    Some(WikiBrief {
        title: s["title"].as_str().unwrap_or(&found).to_string(),
        description: s["description"].as_str().unwrap_or("").to_string(),
        extract,
        url: s["content_urls"]["desktop"]["page"].as_str().map(String::from).unwrap_or_else(|| format!("https://en.wikipedia.org/wiki/{slug}")),
    })
}

/// DuckDuckGo's instant answer: abstract, infobox facts and related topics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DdgBrief {
    pub heading: String,
    pub abstract_text: String,
    pub url: String,
    pub facts: Vec<(String, String)>,
    pub related: Vec<String>,
}

pub fn parse_ddg_instant(v: &Value) -> Option<DdgBrief> {
    let facts: Vec<(String, String)> = v["Infobox"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let label = c["label"].as_str()?.trim().to_string();
            let value = match &c["value"] {
                Value::String(s) => s.trim().to_string(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            (!label.is_empty() && !value.is_empty() && value.len() < 200).then_some((label, value))
        })
        .take(10)
        .collect();
    let mut related = Vec::new();
    for t in v["RelatedTopics"].as_array().into_iter().flatten() {
        let texts: Vec<&Value> = match t["Topics"].as_array() {
            Some(group) => group.iter().collect(),
            None => vec![t],
        };
        for item in texts {
            if let Some(text) = item["Text"].as_str().filter(|s| !s.is_empty()) {
                related.push(text.chars().take(160).collect());
            }
        }
    }
    related.truncate(10);
    let brief = DdgBrief {
        heading: v["Heading"].as_str().unwrap_or("").to_string(),
        abstract_text: v["AbstractText"].as_str().unwrap_or("").to_string(),
        url: v["AbstractURL"].as_str().unwrap_or("").to_string(),
        facts,
        related,
    };
    (!brief.abstract_text.is_empty() || !brief.facts.is_empty() || !brief.related.is_empty()).then_some(brief)
}

pub async fn ddg_instant(topic: &str) -> Option<DdgBrief> {
    let r = fetch(&format!("https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1&t=jai", urlencoding::encode(topic)), API_AGENT, "application/json").await.ok()?;
    let v: Value = serde_json::from_slice(&r.body).ok()?;
    parse_ddg_instant(&v)
}

struct Mission {
    tools: ToolContext,
    id: i64,
    topic: String,
    topic_terms: Vec<String>,
    db: MissionDb,
    index: FactIndex,
    pacer: Pacer,
}

impl Mission {
    fn engine(&self) -> &'static ResearchEngine {
        engine().expect("research engine started")
    }

    fn emit(&self, kind: TelemetryKind) {
        self.engine().emit(self.id, kind);
    }

    fn refresh_counts(&self, elapsed: u64, target: u64, stage: &str) {
        let (pages, facts, frontier, queries_left) = self.db.counts(self.id);
        self.engine().update_live(self.id, |p| {
            p.stage = stage.to_string();
            p.elapsed_secs = elapsed;
            p.target_secs = target;
            p.pages = pages;
            p.facts = facts;
            p.frontier = frontier;
            p.queries_left = queries_left;
        });
    }

    /// Stores a page, extracts and cross-checks its claims, queues its best links.
    fn ingest(&mut self, page: StoredPage, links: &[(String, String)]) -> usize {
        ingest_page(&self.db, &mut self.index, self.id, &self.topic_terms, page, links)
    }

    async fn run_query(&mut self, q: &QueryRow) {
        self.emit(TelemetryKind::Stage { stage: "search".into(), detail: q.sub_question.clone() });
        for provider in providers_for(q) {
            let result = match provider {
                "duckduckgo" => ddg(&mut self.pacer, &q.search_query).await,
                "wikipedia" => wikipedia(&mut self.pacer, &q.search_query).await,
                "arxiv" => arxiv(&mut self.pacer, &q.search_query).await,
                "hackernews" => hacker_news(&mut self.pacer, &q.search_query).await,
                _ => github(&mut self.pacer, &q.search_query).await,
            };
            match result {
                Ok(found) => {
                    self.emit(TelemetryKind::Query { provider: provider.into(), query: q.search_query.clone(), results: found.len() });
                    for f in found {
                        match f {
                            Found::Link { url, score } => {
                                let domain = domain_of(&url);
                                if !domain.is_empty() && !blocked_domain(&domain) && !media_url(&url) {
                                    self.db.push_frontier(self.id, &url, 1, score, Some(q.id));
                                }
                            }
                            Found::Source { url, title, markdown } => {
                                let page = StoredPage { domain: domain_of(&url), reliability: reliability(&url), url, title, markdown, query_id: Some(q.id), depth: 1 };
                                self.ingest(page, &[]);
                            }
                        }
                    }
                }
                Err(e) => self.emit(TelemetryKind::Error { url: format!("{provider}: {}", q.search_query), message: e }),
            }
        }
        self.db.finish_query(q.id, "done");
    }

    async fn crawl(&mut self, frontier_id: i64, url: String, depth: i64, query_id: Option<i64>) {
        let Ok(parsed) = Url::parse(&url) else {
            self.db.mark_frontier(frontier_id, "invalid");
            return;
        };
        let domain = domain_of(&url);
        if !self.pacer.allowed(&parsed).await {
            self.db.mark_frontier(frontier_id, "robots");
            self.emit(TelemetryKind::Error { url, message: "disallowed by robots.txt".into() });
            return;
        }
        self.pacer.wait(&domain).await;
        let fetched = fetch(&url, browser_agent(), "text/html,application/xhtml+xml,text/plain,text/markdown,application/pdf;q=0.8").await;
        let page = match fetched {
            Ok(p) => p,
            Err(e) => {
                self.db.mark_frontier(frontier_id, "failed");
                self.emit(TelemetryKind::Error { url, message: e });
                return;
            }
        };
        self.emit(TelemetryKind::Fetch { url: page.url.clone(), status: page.status, ms: page.ms, bytes: page.body.len() });
        if page.status >= 400 {
            self.db.mark_frontier(frontier_id, "failed");
            return;
        }
        let final_url = normalize_url(&page.url);
        let base = Url::parse(&final_url).ok();
        let clean = if page.content_type.contains("pdf") || final_url.to_lowercase().ends_with(".pdf") {
            let bytes = page.body;
            let text = tokio::task::spawn_blocking(move || pdf_extract::extract_text_from_mem(&bytes).ok()).await.ok().flatten().unwrap_or_default();
            CleanPage { title: final_url.rsplit('/').next().unwrap_or("document.pdf").to_string(), markdown: text.chars().take(200_000).collect(), links: Vec::new() }
        } else if page.content_type.contains("html") || page.content_type.is_empty() {
            let html = String::from_utf8_lossy(&page.body[..page.body.len().min(MAX_BODY_BYTES)]).to_string();
            tokio::task::spawn_blocking(move || html_to_markdown(&html, base.as_ref())).await.unwrap_or_default()
        } else if page.content_type.starts_with("text/") {
            let text = String::from_utf8_lossy(&page.body[..page.body.len().min(MAX_BODY_BYTES)]).to_string();
            let title = text.lines().find(|l| l.trim_start().starts_with('#')).map(|l| l.trim_start_matches('#').trim().to_string()).unwrap_or_else(|| final_url.clone());
            CleanPage { title, markdown: text, links: Vec::new() }
        } else {
            self.db.mark_frontier(frontier_id, "skipped");
            return;
        };
        self.db.mark_frontier(frontier_id, "done");
        if clean.markdown.chars().count() < 200 {
            return;
        }
        let stored = StoredPage {
            domain: domain_of(&final_url),
            reliability: reliability(&final_url),
            url: final_url,
            title: if clean.title.is_empty() { domain } else { clean.title.chars().take(200).collect() },
            markdown: clean.markdown.chars().take(80_000).collect(),
            query_id,
            depth,
        };
        self.ingest(stored, &clean.links);
    }

    /// New angles from the facts found so far: frequent content words not yet searched.
    fn expand_queries(&self) -> usize {
        if self.db.query_count(self.id) >= MAX_QUERIES {
            return 0;
        }
        let Some(snap) = self.engine().with_read(|c| read_snapshot(c, self.id, 400)).flatten() else { return 0 };
        let mut freq: HashMap<String, usize> = HashMap::new();
        for f in &snap.facts {
            for t in terms(&f.claim).into_iter().collect::<HashSet<_>>() {
                if t.len() >= 5 && !self.topic_terms.contains(&t) && !t.chars().all(|c| c.is_ascii_digit()) {
                    *freq.entry(t).or_default() += 1;
                }
            }
        }
        let mut ranked: Vec<(String, usize)> = freq.into_iter().filter(|(_, n)| *n >= 2).collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut added = 0;
        for (term, n) in ranked {
            if added >= 5 {
                break;
            }
            let q = PlannedQuery {
                question: format!("How does {term} relate to {}?", self.topic),
                search: format!("{} {term}", self.topic),
                priority: 40 + n.min(20) as i64,
            };
            if self.db.add_query(self.id, &q) {
                added += 1;
            }
        }
        added
    }

    fn write_dossier(&self, summary: Option<&str>) -> Option<String> {
        let snap = self.engine().with_read(|c| read_snapshot(c, self.id, 600)).flatten()?;
        let md = build_dossier(&snap, summary);
        self.db.save_dossier(self.id, &md, summary);
        Some(md)
    }
}

/// The multi-chapter dossier (pure Rust; the executive summary from the language model when
/// there is one, otherwise the strongest corroborated facts).
pub fn build_dossier(s: &MissionSnapshot, summary: Option<&str>) -> String {
    let m = &s.mission;
    let cite = |f: &FactRow| format!("[S{}]", f.source_id);
    let mut md = format!(
        "# Stark Research Dossier — {}\n\n**Mission #{}** · {} · {} of {} min · {} pages · {} facts · {} sources\n\n",
        m.topic,
        m.id,
        m.status,
        m.elapsed_secs / 60,
        m.target_secs / 60,
        m.pages,
        m.facts,
        s.sources.len()
    );
    md.push_str("## Executive Summary\n\n");
    let stored = summary.map(str::to_string).or_else(|| Some(m.executive_summary.clone()).filter(|e| !e.trim().is_empty()));
    match stored {
        Some(text) => md.push_str(&format!("{}\n\n", text.trim())),
        None => {
            let mut top: Vec<&FactRow> = s.facts.iter().filter(|f| f.contradicting_source.is_none()).collect();
            top.sort_by(|a, b| b.corroborating.cmp(&a.corroborating).then(b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal)));
            if top.is_empty() {
                md.push_str("_The crawler is still gathering material._\n\n");
            }
            for f in top.iter().take(6) {
                md.push_str(&format!("- {} {}\n", f.claim, cite(f)));
            }
            md.push('\n');
        }
    }
    let chapters: [(&str, &[&str]); 6] = [
        ("Chapter 1 — Overview & Definitions", &["definition"]),
        ("Chapter 2 — Architecture & Design", &["architecture"]),
        ("Chapter 3 — Benchmarks & Numbers", &["benchmark", "statistic"]),
        ("Chapter 4 — History & Timeline", &["history"]),
        ("Chapter 5 — Limitations, Risks & Criticism", &["limitation"]),
        ("Chapter 6 — Further Claims", &["claim"]),
    ];
    for (title, cats) in chapters {
        let mut facts: Vec<&FactRow> = s.facts.iter().filter(|f| cats.contains(&f.category.as_str())).collect();
        if facts.is_empty() {
            continue;
        }
        md.push_str(&format!("## {title}\n\n"));
        if title.contains("History") {
            facts.sort_by_key(|f| YEAR.find(&f.claim).map(|y| y.as_str().to_string()).unwrap_or_else(|| "9999".into()));
        }
        if title.contains("Benchmarks") {
            md.push_str("| Claim | Values | Source | Confidence |\n| --- | --- | --- | --- |\n");
            for f in facts.iter().take(25) {
                let values = numbers_of(&f.claim).iter().map(|(v, u)| format!("{v} {u}")).collect::<Vec<_>>().join(", ");
                let domain = s.source(f.source_id).map(|x| x.domain.as_str()).unwrap_or("?");
                md.push_str(&format!("| {} | {} | {domain} {} | {:.0}% |\n", f.claim.replace('|', "/"), if values.is_empty() { "—".into() } else { values }, cite(f), f.confidence * 100.0));
            }
            md.push('\n');
            continue;
        }
        for f in facts.iter().take(20) {
            let mark = if f.corroborating > 0 { format!(" [x{} sources]", f.corroborating + 1) } else { String::new() };
            md.push_str(&format!("- {} {}{mark}\n", f.claim, cite(f)));
        }
        md.push('\n');
    }
    let conflicts: Vec<&FactRow> = s.facts.iter().filter(|f| f.contradicting_source.is_some()).collect();
    if !conflicts.is_empty() {
        md.push_str("## Chapter 7 — Contradictions Between Sources\n\n| Claim | Source | Disputed by |\n| --- | --- | --- |\n");
        for f in conflicts.iter().take(20) {
            let by = f.contradicting_source.and_then(|id| s.source(id)).map(|x| format!("{} [S{}]", x.domain, x.id)).unwrap_or_default();
            let from = s.source(f.source_id).map(|x| x.domain.clone()).unwrap_or_default();
            md.push_str(&format!("| {} | {from} {} | {by} |\n", f.claim.replace('|', "/"), cite(f)));
        }
        md.push('\n');
    }
    md.push_str("## Sources\n\n| # | Title | Domain | Reliability |\n| --- | --- | --- | --- |\n");
    for src in s.sources.iter().take(80) {
        md.push_str(&format!("| S{} | [{}]({}) | {} | {:.2} |\n", src.id, src.title.replace('|', "/").replace(['[', ']'], ""), src.url, src.domain, src.reliability));
    }
    md
}

async fn run_mission(tools: ToolContext, mission_id: i64, control: Arc<AtomicU8>) {
    let Some(engine) = engine() else { return };
    let db = match MissionDb::open(&engine.db_path) {
        Ok(db) => db,
        Err(e) => {
            warn!("[RESEARCH] Mission #{mission_id} cannot open the database: {e}");
            lock(&engine.controls).remove(&mission_id);
            return;
        }
    };
    let Some(row) = db.mission(mission_id) else {
        lock(&engine.controls).remove(&mission_id);
        return;
    };
    db.set_status(mission_id, "running");
    let focus: Vec<String> = row.focus_areas.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    let topic_terms = mission_terms(&row);
    let target = row.target_secs.max(60) as u64;
    let mut elapsed_f = row.elapsed_secs.max(0) as f64;
    let mut elapsed = elapsed_f as u64;
    let mut m = Mission { index: FactIndex::load(&db, mission_id), tools, id: mission_id, topic: row.topic.clone(), topic_terms, db, pacer: Pacer::default() };

    // Stage 1: the query tree.
    if m.db.query_count(mission_id) == 0 {
        m.emit(TelemetryKind::Stage { stage: "plan".into(), detail: format!("Building the query tree for {}", row.topic) });
        let mut planned = plan_queries(&row.topic, &focus);
        if crate::vram_guard::llm_allowed() && !crate::vram_guard::is_dormant() {
            let extra = m
                .tools
                .pim
                .complete(
                    "List 8 additional, very specific web search queries for an in-depth investigation of the topic (technical details, benchmarks, papers, real-world reports, edge cases). One per line, no numbering, no quotes.",
                    &format!("Topic: {}\nFocus: {}", row.topic, focus.join(", ")),
                    220,
                )
                .await
                .unwrap_or_default();
            for line in extra.lines().map(|l| l.trim().trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | '*' | ')')).trim()).filter(|l| l.len() > 6).take(8) {
                planned.push(PlannedQuery { question: format!("Specialist angle: {line}"), search: line.to_string(), priority: 60 });
            }
        }
        for q in &planned {
            m.db.add_query(mission_id, q);
        }
        m.emit(TelemetryKind::Stage { stage: "plan".into(), detail: format!("{} sub-questions queued", m.db.query_count(mission_id)) });
    }

    // Stages 2-4: crawl, extract, report.
    let mut last_tick = Instant::now();
    let mut last_checkpoint = Instant::now();
    let mut last_dossier = Instant::now() - Duration::from_secs(3600);
    let mut pages_since_query = PAGES_PER_QUERY;
    let mut outcome = "completed";
    let mut paused = false;
    loop {
        let now = Instant::now();
        match control.load(Ordering::Acquire) {
            STOPPING => {
                outcome = "stopped";
                break;
            }
            PAUSED => {
                last_tick = now;
                if !paused {
                    paused = true;
                    m.db.set_status(mission_id, "paused");
                    m.db.checkpoint(mission_id, elapsed);
                    m.emit(TelemetryKind::Stage { stage: "paused".into(), detail: "Mission paused".into() });
                }
                m.refresh_counts(elapsed, target, "paused");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            _ => {}
        }
        if paused {
            paused = false;
            m.db.set_status(mission_id, "running");
            m.emit(TelemetryKind::Stage { stage: "resumed".into(), detail: "Mission resumed".into() });
        }
        elapsed_f += now.duration_since(last_tick).as_secs_f64();
        elapsed = elapsed_f as u64;
        last_tick = now;
        if elapsed >= target {
            break;
        }
        if last_checkpoint.elapsed() >= Duration::from_secs(5) {
            m.db.checkpoint(mission_id, elapsed);
            last_checkpoint = Instant::now();
        }
        if last_dossier.elapsed() >= Duration::from_secs(60) {
            m.write_dossier(None);
            last_dossier = Instant::now();
        }
        let frontier = m.db.next_frontier(mission_id);
        if pages_since_query < PAGES_PER_QUERY {
            if let Some((fid, url, depth, query_id, _)) = frontier {
                m.refresh_counts(elapsed, target, "crawl");
                m.crawl(fid, url, depth, query_id).await;
                pages_since_query += 1;
                continue;
            }
        }
        match m.db.next_query(mission_id) {
            Some(q) => {
                m.refresh_counts(elapsed, target, "search");
                m.run_query(&q).await;
                pages_since_query = 0;
            }
            None => {
                if let Some((fid, url, depth, query_id, _)) = m.db.next_frontier(mission_id) {
                    m.refresh_counts(elapsed, target, "crawl");
                    m.crawl(fid, url, depth, query_id).await;
                } else if m.expand_queries() > 0 {
                    m.emit(TelemetryKind::Stage { stage: "expand".into(), detail: "New angles from the facts found so far".into() });
                } else {
                    m.emit(TelemetryKind::Stage { stage: "exhausted".into(), detail: "Every lead has been followed".into() });
                    break;
                }
            }
        }
    }
    m.db.checkpoint(mission_id, elapsed);
    m.refresh_counts(elapsed, target, "synthesis");

    // Synthesis: the language model only once the GPU is free (no game running).
    let mut summary = None;
    if outcome == "completed" {
        m.db.set_status(mission_id, "synthesizing");
        m.write_dossier(None);
        let waited = Instant::now();
        while !crate::vram_guard::llm_allowed() && waited.elapsed() < Duration::from_secs(6 * 3600) && control.load(Ordering::Acquire) != STOPPING {
            m.refresh_counts(elapsed, target, "waiting for a free GPU");
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        if crate::vram_guard::llm_allowed() {
            m.emit(TelemetryKind::Stage { stage: "synthesis".into(), detail: "Writing the executive summary".into() });
            if let Some(snap) = engine.with_read(|c| read_snapshot(c, mission_id, 80)).flatten() {
                let material: String = snap
                    .facts
                    .iter()
                    .map(|f| format!("- [{}] {} [S{}]{}", f.category, f.claim, f.source_id, if f.contradicting_source.is_some() { " (disputed)" } else if f.corroborating > 0 { " (corroborated)" } else { "" }))
                    .collect::<Vec<_>>()
                    .join("\n");
                summary = m
                    .tools
                    .pim
                    .complete(
                        "Write the executive summary of a research dossier from these extracted facts: 6-9 precise sentences in English with the key numbers, what is corroborated, what is disputed, and the bottom line. Cite sources as [S<n>] exactly as given. No headings, no filler.",
                        &format!("Topic: {}\n\n{material}", row.topic),
                        700,
                    )
                    .await;
            }
        }
    }
    let final_status = if outcome == "stopped" { "stopped" } else { "completed" };
    m.db.set_status(mission_id, final_status);
    let md = m.write_dossier(summary.as_deref()).unwrap_or_default();
    let dir = user_folder("Documents").join("JAI_Research");
    let file = dir.join(format!("{} {} (deep research).md", chrono::Local::now().format("%Y-%m-%d"), safe_name(&row.topic)));
    let saved = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &md)).is_ok();
    let (pages, facts, _, _) = m.db.counts(mission_id);
    let headline = format!("Deep research on {} {final_status}: {pages} pages, {facts} facts.", row.topic);
    info!("[RESEARCH] {headline}{}", if saved { format!(" Dossier saved to {}", file.display()) } else { String::new() });
    m.emit(TelemetryKind::Finished { status: final_status.into(), summary: headline.clone() });
    lock(&engine.controls).remove(&mission_id);
    m.refresh_counts(elapsed, target, final_status);
    let event = if crate::vram_guard::llm_allowed() {
        StarkEvent::Alert(format!("Sir, the deep research on {} is {final_status}. {pages} pages read, {facts} facts extracted; the dossier is in your workspace.", row.topic))
    } else {
        StarkEvent::Notice(headline)
    };
    let _ = m.tools.stark.events.send(event);
}

// ---------------------------------------------------------------------------- the tool ----

fn insert_mission(conn: &Connection, topic: &str, minutes: u64, focus: &[String]) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO research_missions (topic, status, created_at, target_duration_secs, focus_areas, updated_at) VALUES (?1, 'running', ?2, ?3, ?4, ?2)",
        params![topic, now_text(), (minutes * 60) as i64, focus.join("; ")],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Missions a crash or shutdown interrupted (resumed at start).
fn interrupted_missions(c: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut st = c.prepare("SELECT id FROM research_missions WHERE status IN ('running', 'synthesizing') ORDER BY id")?;
    let rows = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>();
    rows
}

fn create_mission(topic: &str, minutes: u64, focus: &[String]) -> Result<i64, String> {
    let engine = engine().ok_or("The research engine is not running.")?;
    let conn = Connection::open(&engine.db_path).map_err(|e| e.to_string())?;
    insert_mission(&conn, topic, minutes, focus).map_err(|e| e.to_string())
}

fn set_control(mission_id: i64, state: u8) -> bool {
    engine().and_then(|e| lock(&e.controls).get(&mission_id).cloned()).map(|c| c.store(state, Ordering::Release)).is_some()
}

fn focus_arg(args: &Value) -> Vec<String> {
    match args.get("focus_areas").or_else(|| args.get("focus")) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(s)) => s.split([',', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    }
}

fn describe(m: &MissionRow) -> String {
    let live = engine().and_then(|e| e.live(m.id));
    let (pages, facts) = live.as_ref().map(|l| (l.pages as i64, l.facts as i64)).unwrap_or((m.pages, m.facts));
    let elapsed = live.as_ref().map(|l| l.elapsed_secs as i64).unwrap_or(m.elapsed_secs);
    format!(
        "#{} \"{}\" — {} · {} of {} min · {pages} pages · {facts} facts{}",
        m.id,
        m.topic,
        m.status,
        elapsed / 60,
        m.target_secs / 60,
        live.map(|l| format!(" · {:.0} pages/min · stage {}", l.pages_per_min(), l.stage)).unwrap_or_default()
    )
}

pub fn is_tool(name: &str) -> bool {
    name == "manage_deep_research"
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": "manage_deep_research",
        "description": "Deep Research missions (10 minutes to 12 hours) that crawl the web, Wikipedia, arXiv, Hacker News and GitHub in the background, also while Sir games (no GPU), and write a live dossier with a knowledge graph. action start (topic, duration_minutes, focus_areas), pause, resume, stop, status, open_workspace.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["start", "pause", "resume", "stop", "status", "open_workspace"]},
            "topic": {"type": "string"},
            "duration_minutes": {"type": "integer", "description": "How long to research (default 60)."},
            "focus_areas": {"type": "array", "items": {"type": "string"}},
            "mission_id": {"type": "integer", "description": "Defaults to the latest mission."}
        }, "required": ["action"]}
    }})]
}

pub fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = str_arg(args, &["action"]).unwrap_or("status").to_lowercase();
    let mission_id = args["mission_id"].as_i64().or_else(latest_mission_id);
    match action.as_str() {
        "start" | "begin" | "launch" => {
            let topic = str_arg(args, &["topic", "query", "subject"]).ok_or("What shall I research, Sir?")?.trim().to_string();
            let minutes = args["duration_minutes"].as_u64().or_else(|| args["minutes"].as_u64()).unwrap_or(60).clamp(10, 720);
            let focus = focus_arg(args);
            let id = create_mission(&topic, minutes, &focus)?;
            launch(tools, id);
            let gaming = !crate::vram_guard::llm_allowed();
            if !gaming && args["open_workspace"].as_bool() != Some(false) {
                crate::workspace_canvas::spawn_research_workspace(Some(id));
            }
            let hours = if minutes >= 120 { format!("{:.1} hours", minutes as f32 / 60.0) } else { format!("{minutes} minutes") };
            Ok(spoken(
                format!(
                    "Deep research mission #{id} started: \"{topic}\" for {hours}{}. Crawling DuckDuckGo, Wikipedia, arXiv, Hacker News and GitHub; checkpointed in jai_memory.db; dossier saved to Documents\\JAI_Research when done.",
                    if focus.is_empty() { String::new() } else { format!(", focus: {}", focus.join(", ")) }
                ),
                format!("Deep research on {topic} is under way, Sir. I'll keep at it for {hours}{}.", if gaming { " — quietly, on the CPU, while you play" } else { "" }),
            ))
        }
        "pause" | "resume" | "stop" | "cancel" => {
            let id = mission_id.ok_or("There is no research mission, Sir.")?;
            let state = match action.as_str() {
                "pause" => PAUSED,
                "resume" => RUNNING,
                _ => STOPPING,
            };
            if !set_control(id, state) {
                if action == "resume" {
                    launch(tools, id);
                    return Ok(format!("Mission #{id} resumed from its checkpoint."));
                }
                return Err(format!("Mission #{id} is not running."));
            }
            Ok(match action.as_str() {
                "pause" => format!("Mission #{id} paused; the checkpoint is kept."),
                "resume" => format!("Mission #{id} resumed."),
                _ => format!("Mission #{id} is stopping; the dossier so far is being written."),
            })
        }
        "open_workspace" | "open" | "show" => {
            crate::workspace_canvas::spawn_research_workspace(mission_id);
            Ok(match mission_id {
                Some(id) => format!("Research workspace for mission #{id} is up."),
                None => "Research workspace is up (no mission yet).".into(),
            })
        }
        _ => {
            let missions = list_missions(5);
            if missions.is_empty() {
                return Ok("No deep research missions yet.".into());
            }
            Ok(missions.iter().map(describe).collect::<Vec<_>>().join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_tree_has_10_to_30_queries() {
        let q = plan_queries("solid-state batteries", &["cost".into(), "safety".into()]);
        assert!((10..=30).contains(&q.len()), "{}", q.len());
        assert_eq!(q[0].search, "solid-state batteries cost");
        assert!(q.iter().any(|x| x.search.contains("research paper")));
    }

    #[test]
    fn html_becomes_clean_markdown() {
        let html = r#"<html><head><title>Batteries</title><script>var x=1;</script><style>p{}</style></head><body>
            <nav>Home | About</nav><div class="ad-banner">Buy now</div>
            <article><h1>Solid-state batteries</h1><p>Solid-state batteries use a solid electrolyte instead of a liquid one.</p>
            <ul><li>Energy density up to 500 Wh/kg</li></ul><pre>let x = 1;</pre>
            <table><tr><th>Cell</th><th>Wh/kg</th></tr><tr><td>A</td><td>400</td></tr></table>
            <p>See <a href="/wiki/Electrolyte">electrolyte</a>.</p></article><footer>© 2026</footer></body></html>"#;
        let base = Url::parse("https://example.org/page").unwrap();
        let page = html_to_markdown(html, Some(&base));
        assert_eq!(page.title, "Batteries");
        assert!(page.markdown.contains("# Solid-state batteries"));
        assert!(page.markdown.contains("- Energy density up to 500 Wh/kg"));
        assert!(page.markdown.contains("```\nlet x = 1;\n```"));
        assert!(page.markdown.contains("| Cell | Wh/kg |"));
        assert!(!page.markdown.contains("var x"));
        assert!(!page.markdown.contains("Buy now"));
        assert!(!page.markdown.contains("Home | About"));
        assert!(page.links.iter().any(|(u, a)| u == "https://example.org/wiki/Electrolyte" && a == "electrolyte"));
    }

    #[test]
    fn authority_boost() {
        assert!(reliability("https://arxiv.org/abs/2401.0001") > 0.75);
        assert!(reliability("https://mit.edu/x") > 0.75);
        assert!(reliability("https://docs.rs/tokio") > 0.75);
        assert!((reliability("https://randomblog.net/post") - 0.5).abs() < 0.01);
        assert!(reliability("https://www.quora.com/q") < 0.3);
    }

    #[test]
    fn sentences_and_facts() {
        let s = split_sentences("Solid-state cells reach 500 Wh/kg in lab tests, e.g. at Toyota. They were first introduced in 2017 by a startup. Costs remain high.");
        assert_eq!(s.len(), 3, "{s:?}");
        let md = "Solid-state batteries reach an energy density of 500 Wh/kg in laboratory tests according to the 2024 study.\n\nCookie settings and privacy policy for this website are available below for all visitors.";
        let facts = extract_facts(md, "Solid-state batteries", &terms("solid-state batteries"), 0.8);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "statistic");
        assert!(facts[0].confidence > 0.6);
    }

    #[test]
    fn corroboration_and_contradiction() {
        let mut index = FactIndex::default();
        index.insert(1, 10, "a.org".into(), "Solid-state batteries reach an energy density of 500 Wh/kg in laboratory cells.");
        let agree = index.judge("b.org", "In laboratory cells, solid-state batteries reach an energy density of 500 Wh/kg.");
        assert_eq!(agree.agreeing, vec![1]);
        let conflict = index.judge("c.org", "Solid-state batteries reach an energy density of 250 Wh/kg in laboratory cells.");
        assert_eq!(conflict.contradicting, Some((1, 10)));
        let same_site = index.judge("a.org", "Solid-state batteries reach an energy density of 500 Wh/kg in laboratory cells.");
        assert!(same_site.agreeing.is_empty());
    }

    #[test]
    fn schema_checkpoints_and_resumes_missions() {
        let path = std::env::temp_dir().join(format!("jai_research_{}.db", uuid::Uuid::new_v4()));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for table in ["research_missions", "research_queries", "research_sources", "research_facts", "research_frontier"] {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |r| r.get(0)).unwrap();
            assert_eq!(n, 1, "{table}");
        }
        let focus = vec!["cost".to_string()];
        let id = insert_mission(&conn, "solid-state batteries", 120, &focus).unwrap();
        let db = MissionDb::open(path.to_str().unwrap()).unwrap();
        // Stage 1: the decomposed sub-questions, each once.
        let plan = plan_queries("solid-state batteries", &focus);
        for q in &plan {
            assert!(db.add_query(id, q));
        }
        assert!(!db.add_query(id, &plan[0]), "a duplicate query is refused");
        // Highest priority first: the overview (100) before the focus area (99).
        let q = db.next_query(id).unwrap();
        assert_eq!((q.search_query.as_str(), q.priority), ("solid-state batteries overview explained", 100));
        db.finish_query(q.id, "done");
        // The frontier deduplicates.
        db.push_frontier(id, "https://example.org/ssb", 1, 0.7, Some(q.id));
        db.push_frontier(id, "https://example.org/ssb", 1, 0.9, Some(q.id));
        assert_eq!(db.next_frontier(id).map(|f| f.1).as_deref(), Some("https://example.org/ssb"));
        // Stage 3 on a cleaned page.
        let page = html_to_markdown(
            "<html><body><nav>menu</nav><article><h1>Solid-state batteries</h1><p>Solid-state batteries reach an energy density of 500 Wh/kg in laboratory tests according to the 2024 study.</p></article></body></html>",
            None,
        );
        assert!(!page.markdown.contains("menu"));
        let stored = StoredPage { url: "https://example.org/ssb".into(), domain: "example.org".into(), title: page.title, reliability: 0.6, markdown: page.markdown, query_id: Some(q.id), depth: 1 };
        let mut index = FactIndex::default();
        assert_eq!(ingest_page(&db, &mut index, id, &mission_terms(&db.mission(id).unwrap()), stored, &[]), 1);
        db.mark_frontier(db.next_frontier(id).unwrap().0, "done");
        db.checkpoint(id, 4321);
        drop(db);
        // A crash: the mission is still "running" and resumes from its checkpoint.
        assert_eq!(interrupted_missions(&conn).unwrap(), vec![id]);
        let reopened = MissionDb::open(path.to_str().unwrap()).unwrap();
        let m = reopened.mission(id).unwrap();
        assert_eq!((m.elapsed_secs, m.pages, m.facts, m.target_secs, m.focus_areas.as_str()), (4321, 1, 1, 7200, "cost"));
        assert!(reopened.next_frontier(id).is_none(), "crawled pages are not crawled again");
        let snap = read_snapshot(&conn, id, 50).unwrap().unwrap();
        assert_eq!((snap.sources.len(), snap.facts.len()), (1, 1));
        assert!(snap.queries.iter().any(|q| q.status == "done"));
        let md = build_dossier(&snap, None);
        assert!(md.contains("## Chapter 3 — Benchmarks & Numbers") && md.contains("500 Wh/kg"), "{md}");
        reopened.set_status(id, "completed");
        assert!(interrupted_missions(&conn).unwrap().is_empty());
        drop((conn, reopened));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duckduckgo_instant_answer_parses() {
        let v = json!({
            "Heading": "Spotify",
            "AbstractText": "Spotify is a Swedish audio streaming service.",
            "AbstractURL": "https://en.wikipedia.org/wiki/Spotify",
            "Infobox": {"content": [{"label": "Founded", "value": "23 April 2006"}, {"label": "Owner", "value": {"nested": true}}]},
            "RelatedTopics": [{"Text": "Daniel Ek - founder of Spotify"}, {"Name": "Streaming", "Topics": [{"Text": "Music streaming service"}]}]
        });
        let b = parse_ddg_instant(&v).unwrap();
        assert_eq!(b.heading, "Spotify");
        assert_eq!(b.facts, vec![("Founded".to_string(), "23 April 2006".to_string())]);
        assert_eq!(b.related, vec!["Daniel Ek - founder of Spotify".to_string(), "Music streaming service".to_string()]);
        assert!(parse_ddg_instant(&json!({"Heading": "", "AbstractText": ""})).is_none());
    }

    #[test]
    fn urls_and_robots() {
        assert_eq!(normalize_url("https://x.org/a/?utm_source=t&id=3#frag"), "https://x.org/a?id=3");
        assert_eq!(normalize_url("https://x.org/"), "https://x.org/");
        let rules = parse_robots("User-agent: Googlebot\nDisallow: /g\n\nUser-agent: *\nDisallow: /private\nDisallow: /tmp*\n");
        assert_eq!(rules, vec!["/private".to_string(), "/tmp".to_string()]);
    }
}
