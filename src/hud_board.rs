//! The HUD board: when JAI presents something — a research report, a place, a schedule — the
//! screen fills with one to thirteen sharp-edged technical "blocks" on a bento grid, and the
//! emblem docks in the bottom-right corner while he walks Sir through them.
//!
//! * `ScreenState`: signed off (screen down, gyroscope), awake and empty (front emblem), or a
//!   board on screen (`BoardActive`).
//! * Six block kinds: the live research progress block, stacked notes, the white vertical bar
//!   chart, the hero figure, media (photos, galleries) and the dark vector map.
//! * Voice workflows (the `hud_board` tool, fast-path intents in `intents::board_command`): wake,
//!   research → chime → "want me to put it on screen?", present, recall a saved report (SQLite),
//!   maximise a block into the spotlight (everything else dims to 12 %), minimise, flip a note
//!   stack, show a place on the map, sign off (blocks collapse, the emblem glides home and folds
//!   into the gyroscope, VRAM is flushed).
//! * Board Q&A: `live_context` lists every block's exact contents for the language model and
//!   `focus_for_question` pulses the blocks a question is about.
//!
//! The research agent and the map / photo fetchers live in `board_research`.

use crate::jai_voice_dsp::Earcon;
use crate::tools::ToolContext;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, Rounding, Sense, Shape, Stroke, Ui, Vec2};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::{info, warn};

// ------------------------------------------------------------------------------ colours ----

/// A frame's hairline: structure, in ink (the theme is re-read every frame).
pub fn border_colour() -> Color32 {
    crate::holo::a(crate::holo_theme::current().ink(), 0.20)
}

/// What is live, selected or measured: the accent.
pub fn active_colour() -> Color32 {
    crate::holo_theme::current().accent()
}

/// Corner ticks.
pub fn bracket_colour() -> Color32 {
    crate::holo_theme::current().ink()
}

/// Opacity of every other block while one is in the spotlight.
pub const DIM_OPACITY: f32 = 0.12;
/// Delay between two blocks of the cascade.
pub const CASCADE_STEP: Duration = Duration::from_millis(30);
pub const SPOTLIGHT_SIZE: [f32; 2] = [780.0, 480.0];
pub const RESEARCH_BLOCK_SIZE: [f32; 2] = [320.0, 130.0];
/// The research block's thin progress cells.
pub const RESEARCH_CELLS: usize = 30;
/// The research block first sits in the middle of the screen, larger, for this long ("starting
/// research agent"), then glides into its corner slot.
pub const RESEARCH_INTRO_SECS: f32 = 3.0;
pub const RESEARCH_GLIDE_SECS: f32 = 0.25;
/// The footer line under every block: "ADDED JUST NOW" left, "SOURCE: META · 0x04A3" right.
pub const FOOTER_H: f32 = 13.0;
/// A key of the suggestion bar under the spotlight (width, height).
pub const SUGGESTION_KEY: [f32; 2] = [150.0, 24.0];
const APPEAR_SECS: f32 = crate::holo_motion::ARRIVE;
const LEAVE_SECS: f32 = crate::holo_motion::LEAVE;
const HIGHLIGHT_SECS: f32 = 4.5;

// -------------------------------------------------------------------------------- model ----

/// What the board shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ScreenState {
    /// "Screen down": signed off, the gyroscope spins in the centre.
    SignedOff,
    /// Awake, nothing on the board: the front emblem.
    AwakeEmpty,
    BoardActive { topic: String, block_count: usize, source_count: usize },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub headline: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub label: String,
    pub value: f32,
}

/// A road of the vector map; `class` 0 = motorway/trunk … 3 = residential/service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Road {
    pub class: u8,
    /// [lat, lon] pairs.
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockKind {
    /// One or more notes in one slot; the others are stacked at the back.
    NoteStack { notes: Vec<Note> },
    /// Solid white vertical bars over dashed grid lines.
    BarChart { unit: String, bars: Vec<Bar>, caption: String },
    /// A big figure (≈20m, 100+, 63.7 %) with its header and a source note.
    HeroFigure { header: String, figure: String, note: String },
    /// Photos: one in the grid, all of them as a gallery in the spotlight.
    MediaCard { images: Vec<String>, caption: String },
    /// Pitch-black map: white roads, dark water, the red pin in the centre.
    DarkVectorMap {
        place: String,
        lat: f64,
        lon: f64,
        span: f64,
        roads: Vec<Road>,
        water: Vec<Vec<[f64; 2]>>,
        /// The streets are still on their way (`fill_map`).
        #[serde(default)]
        pending: bool,
    },
    /// `RESEARCH // ACTIVE` (bottom-left while an agent runs): the topic, its phase, the 16-cell
    /// segmented progress bar and the live block / source counters.
    ResearchProgress { topic: String, phase: String, elapsed_secs: u64, progress: f32, blocks: usize, sources: usize },
    /// `COMMS // LIVE RESPONSE`: what Sir said and JAI's answer, while it streams and after, so
    /// every reply is visible on the board as well as heard.
    DialogueFeed { heard: String, reply: String, streaming: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// "NOTE // 01 — HARDWARE & PRICING" (top-left micro header).
    pub label: String,
    /// "FACT", "SOURCE: STEAM" (top-right).
    pub tag: String,
    /// "SOURCE: META REALITY LABS" (bottom-left).
    pub source: String,
    pub kind: BlockKind,
}

impl Block {
    pub fn new(label: impl Into<String>, tag: impl Into<String>, source: impl Into<String>, kind: BlockKind) -> Self {
        Self { label: label.into().to_uppercase(), tag: tag.into().to_uppercase(), source: source.into().to_uppercase(), kind }
    }

    fn type_word(&self) -> &'static str {
        match self.kind {
            BlockKind::NoteStack { .. } => "note",
            BlockKind::BarChart { .. } => "chart",
            BlockKind::HeroFigure { .. } => "figure",
            BlockKind::MediaCard { .. } => "photo",
            BlockKind::DarkVectorMap { .. } => "map",
            BlockKind::ResearchProgress { .. } => "research",
            BlockKind::DialogueFeed { .. } => "comms",
        }
    }

    /// Grid cells (columns, rows) the block takes.
    fn cells(&self) -> (usize, usize) {
        match &self.kind {
            BlockKind::BarChart { .. } | BlockKind::DarkVectorMap { .. } => (2, 2),
            // A video plays wide.
            BlockKind::MediaCard { images, .. } if images.first().is_some_and(|u| youtube_id(u).is_some()) => (2, 1),
            _ => (1, 1),
        }
    }

    /// Everything the block says, as plain text (Q&A, search, the live context).
    pub fn text(&self) -> String {
        let body = match &self.kind {
            BlockKind::NoteStack { notes } => notes.iter().map(|n| format!("{}: {}", n.headline, n.body)).collect::<Vec<_>>().join(" | "),
            BlockKind::BarChart { unit, bars, caption } => {
                let values = bars.iter().map(|b| format!("{} {}{}", b.label, trim_number(b.value), unit)).collect::<Vec<_>>().join(", ");
                format!("{values}. {caption}")
            }
            BlockKind::HeroFigure { header, figure, note } => format!("{header}: {figure}. {note}"),
            BlockKind::MediaCard { images, caption } => format!("{caption} ({} image{})", images.len(), if images.len() == 1 { "" } else { "s" }),
            BlockKind::DarkVectorMap { place, lat, lon, roads, .. } => format!("{place} at {lat:.5}, {lon:.5} ({} roads drawn)", roads.len()),
            BlockKind::ResearchProgress { topic, phase, progress, blocks, sources, .. } => {
                format!("researching {topic}: {}, {:.0} % done, {blocks} blocks, {sources} sources", phase.to_lowercase(), progress * 100.0)
            }
            BlockKind::DialogueFeed { heard, reply, .. } => format!("Sir: {heard} | JAI: {reply}"),
        };
        format!("{} — {body} ({})", self.label, self.source)
    }

    /// What the block says, as one spoken sentence (the board's own answer to a question).
    pub fn spoken_answer(&self) -> String {
        let clip = |s: &str, max: usize| -> String {
            let s = s.trim();
            if s.chars().count() <= max {
                return s.to_string();
            }
            let cut: String = s.chars().take(max).collect();
            match cut.rfind(['.', '!', '?']) {
                Some(end) if end > max / 3 => cut[..=end].to_string(),
                _ => format!("{}…", cut.trim_end()),
            }
        };
        let header = self.label.split(" — ").nth(1).or_else(|| self.label.split(" // ").nth(1)).unwrap_or(&self.label).to_lowercase();
        match &self.kind {
            BlockKind::NoteStack { notes } => notes.first().map(|n| clip(&format!("{}. {}", n.headline.trim_end_matches('.'), n.body), 260)).unwrap_or_default(),
            BlockKind::BarChart { unit, bars, .. } => {
                let values = bars.iter().take(5).map(|b| format!("{} at {}{}", b.label, trim_number(b.value), unit)).collect::<Vec<_>>().join(", ");
                format!("the {header} chart has {values}.")
            }
            BlockKind::HeroFigure { header, figure, note } => clip(&format!("{header}: {figure}. {note}"), 220),
            BlockKind::MediaCard { caption, .. } => clip(caption, 200),
            BlockKind::DarkVectorMap { place, .. } => format!("{place} is on the map."),
            BlockKind::ResearchProgress { .. } | BlockKind::DialogueFeed { .. } => clip(&self.text(), 220),
        }
    }
}

fn trim_number(v: f32) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// A finished board: what is saved, recalled and presented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub topic: String,
    pub created_at: String,
    pub blocks: Vec<Block>,
    pub sources: Vec<String>,
    #[serde(skip)]
    pub id: Option<i64>,
}

impl Report {
    #[cfg(test)]
    pub fn note_count(&self) -> usize {
        self.blocks.iter().map(|b| if let BlockKind::NoteStack { notes } = &b.kind { notes.len() } else { 0 }).sum()
    }

    fn stacked(&self) -> bool {
        self.blocks.iter().any(|b| matches!(&b.kind, BlockKind::NoteStack { notes } if notes.len() > 1))
    }
}

/// The live research agent, painted as the bottom-left progress block.
#[derive(Debug, Clone)]
pub struct ResearchStatus {
    pub topic: String,
    pub phase: String,
    pub started: Instant,
    pub blocks: usize,
    pub sources: usize,
    /// 0-1.
    pub progress: f32,
}

impl ResearchStatus {
    /// The status as its `RESEARCH // ACTIVE` block.
    pub fn block(&self, now: Instant) -> Block {
        Block::new(
            "RESEARCH //",
            "",
            "",
            BlockKind::ResearchProgress {
                topic: self.topic.clone(),
                phase: self.phase.clone(),
                elapsed_secs: now.saturating_duration_since(self.started).as_secs(),
                progress: self.progress.clamp(0.0, 1.0),
                blocks: self.blocks,
                sources: self.sources,
            },
        )
    }
}

/// The `COMMS // LIVE RESPONSE` block for the latest exchange (None when there is nothing to
/// show). `at`: the time of the reply ("14:02:11").
pub fn dialogue_block(heard: &str, reply: &str, streaming: bool, at: &str) -> Option<Block> {
    let (heard, reply) = (heard.trim(), reply.trim());
    if heard.is_empty() && reply.is_empty() {
        return None;
    }
    let tag = if streaming { "LIVE".to_string() } else { at.to_string() };
    Some(Block::new("COMMS // LIVE RESPONSE", tag, "J.A.I. // VOICE + TEXT", BlockKind::DialogueFeed { heard: heard.to_string(), reply: reply.to_string(), streaming }))
}

/// A question about the board answered straight from its blocks: the best-matching block is
/// highlighted and its contents read out. What JAI says when the language model cannot answer
/// in time (None: no board, or nothing on it matches).
pub fn answer_from_board(question: &str) -> Option<String> {
    let picked = focus_for_question(question);
    let first = *picked.first()?;
    let block = board().shown.as_ref()?.report.blocks.get(first)?.clone();
    let answer = block.spoken_answer();
    (!answer.trim().is_empty()).then(|| {
        let mut answer = answer.trim().to_string();
        if let Some(c) = answer.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        format!("From the board, Sir: {answer}")
    })
}

#[derive(Clone)]
struct Shown {
    report: Arc<Report>,
    at: Instant,
    /// The front note of each stack.
    fronts: Vec<usize>,
    /// When each block was last pulsed by a question about it.
    highlights: Vec<Option<Instant>>,
}

struct Board {
    signed_off: bool,
    shown: Option<Shown>,
    pending: Option<Arc<Report>>,
    research: Option<ResearchStatus>,
    spotlight: Option<(usize, Instant)>,
    /// The previous board collapsing off the screen.
    leaving: Option<(Arc<Report>, Instant)>,
    /// Bumped by every sign-off, so a delayed VRAM flush knows whether Sir woke JAI since.
    sign_off_epoch: u64,
    /// The board the last sign-off took down ("bring back the screen" restores it).
    last: Option<Arc<Report>>,
    /// `Pin Block`: these blocks lead every report that is presented until they are unpinned.
    pinned: Vec<Block>,
}

static BOARD: LazyLock<Mutex<Board>> = LazyLock::new(|| {
    Mutex::new(Board { signed_off: false, shown: None, pending: None, research: None, spotlight: None, leaving: None, sign_off_epoch: 0, last: None, pinned: Vec::new() })
});

fn board() -> MutexGuard<'static, Board> {
    BOARD.lock().unwrap_or_else(|e| e.into_inner())
}

fn changed() {
    crate::gui::refresh();
}

pub fn screen_state() -> ScreenState {
    let b = board();
    match &b.shown {
        Some(s) => ScreenState::BoardActive { topic: s.report.topic.clone(), block_count: s.report.blocks.len(), source_count: s.report.sources.len() },
        None if b.signed_off => ScreenState::SignedOff,
        None => ScreenState::AwakeEmpty,
    }
}

/// A board is on screen.
pub fn is_active() -> bool {
    board().shown.is_some()
}

/// A block is maximised.
pub fn spotlight_active() -> bool {
    let b = board();
    b.shown.is_some() && b.spotlight.is_some()
}

/// What the reflex router needs to read "go back" or "next page", under one lock: (a board is
/// up, a block is maximised, the board has a stack of pages to turn).
pub fn reflex_flags() -> (bool, bool, bool) {
    let b = board();
    match &b.shown {
        Some(s) => (true, b.spotlight.is_some(), s.report.stacked()),
        None => (false, false, false),
    }
}

/// A research agent is running.
pub fn researching() -> bool {
    board().research.is_some()
}

/// Puts a report on the board (the cascade starts now); the emblem docks.
pub fn present(mut report: Report) -> (usize, usize) {
    crate::mind_activity::pulse("hud_board.rs");
    let counts = (report.blocks.len(), report.sources.len());
    let mut b = board();
    // Pinned blocks lead the new report (once each).
    for pinned in b.pinned.iter().rev() {
        if !report.blocks.contains(pinned) {
            report.blocks.insert(0, pinned.clone());
        }
    }
    if let Some(old) = b.shown.take() {
        b.leaving = Some((old.report, Instant::now()));
    }
    let n = report.blocks.len();
    b.shown = Some(Shown { report: Arc::new(report), at: Instant::now() + Duration::from_millis(120), fronts: vec![0; n], highlights: vec![None; n] });
    b.spotlight = None;
    b.signed_off = false;
    drop(b);
    crate::tts::play_cue(Earcon::Materialize);
    changed();
    counts
}

/// `Pin Block` on block `index` of the board: flips its pin. Some(true) when it is pinned now,
/// None when there is no such block.
pub fn toggle_pin(index: usize) -> Option<bool> {
    let mut b = board();
    let block = b.shown.as_ref()?.report.blocks.get(index)?.clone();
    let pinned = match b.pinned.iter().position(|p| *p == block) {
        Some(at) => {
            b.pinned.remove(at);
            false
        }
        None => {
            b.pinned.push(block);
            true
        }
    };
    drop(b);
    changed();
    Some(pinned)
}

/// Whether this block is pinned.
pub fn is_pinned(block: &Block) -> bool {
    board().pinned.contains(block)
}

/// `Close` on one block of the board: it leaves (and loses its pin); the last block takes the
/// board with it. The block's header, or None when there is no such block.
pub fn remove_block(index: usize) -> Option<String> {
    let mut b = board();
    let (block, last) = {
        let shown = b.shown.as_ref()?;
        (shown.report.blocks.get(index)?.clone(), shown.report.blocks.len() == 1)
    };
    b.pinned.retain(|p| *p != block);
    if last {
        drop(b);
        close_board();
        return Some(block.label);
    }
    let shown = b.shown.as_mut()?;
    let mut report = (*shown.report).clone();
    report.blocks.remove(index);
    shown.report = Arc::new(report);
    shown.fronts.remove(index);
    shown.highlights.remove(index);
    // The spotlight follows its block (or closes with it).
    b.spotlight = match b.spotlight {
        Some((spot, _)) if spot == index => None,
        Some((spot, at)) if spot > index => Some((spot - 1, at)),
        other => other,
    };
    drop(b);
    changed();
    Some(block.label)
}

/// The blocks of the board a search lights up ("find voltage"): their indices.
pub fn search_hits(needle: &str) -> Vec<usize> {
    let b = board();
    let Some(shown) = b.shown.as_ref().filter(|_| !needle.trim().is_empty()) else { return Vec::new() };
    shown.report.blocks.iter().enumerate().filter(|(_, block)| crate::hud_dock::search_matches(needle, &block.label, &block.text())).map(|(i, _)| i).collect()
}

/// The waiting report on the board, if any.
pub fn present_pending() -> Option<(String, usize, usize)> {
    let report = board().pending.take()?;
    let topic = report.topic.clone();
    let (blocks, sources) = present((*report).clone());
    Some((topic, blocks, sources))
}

/// Screen down: the blocks collapse, the emblem glides home into the gyroscope.
pub fn sign_off() -> u64 {
    let mut b = board();
    if let Some(old) = b.shown.take() {
        b.last = Some(Arc::clone(&old.report));
        b.leaving = Some((old.report, Instant::now()));
    }
    b.spotlight = None;
    b.signed_off = true;
    b.sign_off_epoch += 1;
    let epoch = b.sign_off_epoch;
    drop(b);
    changed();
    epoch
}

/// "Close the HUD board": the blocks leave, the screen stays up (no sign-off, no GPU flush).
/// False when no board was up.
pub fn close_board() -> bool {
    let mut b = board();
    let Some(old) = b.shown.take() else { return false };
    b.last = Some(Arc::clone(&old.report));
    b.leaving = Some((old.report, Instant::now()));
    b.spotlight = None;
    drop(b);
    changed();
    true
}

/// "Close the windows": every block of the board that is not pinned leaves (and the board with
/// them when none is pinned). How many left.
pub fn close_unpinned() -> usize {
    let (total, pinned): (usize, Vec<usize>) = {
        let b = board();
        let Some(shown) = b.shown.as_ref() else { return 0 };
        let blocks = &shown.report.blocks;
        (blocks.len(), (0..blocks.len()).filter(|i| b.pinned.contains(&blocks[*i])).collect())
    };
    if pinned.is_empty() {
        close_board();
        return total;
    }
    // (From the back, so the indices before a closed one stay true.)
    (0..total).rev().filter(|i| !pinned.contains(i)).filter_map(remove_block).count()
}

/// Closes what a spoken target names on the board by its header: "the briefing" (every block
/// of the briefing, or the whole report when that is its topic), "the timetable" (that one).
/// The headers of what left.
pub fn close_matching(target: &str) -> Vec<String> {
    let words = crate::deep_research_engine::terms(&target.to_lowercase());
    let Some(report) = board().shown.as_ref().map(|s| Arc::clone(&s.report)) else { return Vec::new() };
    if words.is_empty() {
        return Vec::new();
    }
    let named = |text: &str| {
        let text = text.to_lowercase();
        words.iter().all(|w| text.contains(w.as_str()))
    };
    if named(&report.topic) {
        close_board();
        return report.blocks.iter().map(|b| b.label.clone()).collect();
    }
    let which: Vec<usize> = report.blocks.iter().enumerate().filter(|(_, b)| named(&format!("{} {} {}", b.label, b.tag, b.source))).map(|(i, _)| i).collect();
    which.into_iter().rev().filter_map(remove_block).collect()
}

/// "Are you there?": the state before waking (the gyroscope unfolds into the front emblem).
pub fn wake() -> ScreenState {
    let before = screen_state();
    board().signed_off = false;
    changed();
    before
}

/// "Bring back the screen": the board the last sign-off took down cascades back in (the screen
/// wakes either way). Returns its topic and block count, or None when there was none.
pub fn restore() -> Option<(String, usize)> {
    let last = {
        let mut b = board();
        b.signed_off = false;
        if b.shown.is_some() {
            let s = b.shown.as_ref().map(|s| (s.report.topic.clone(), s.report.blocks.len()));
            drop(b);
            changed();
            return s;
        }
        b.last.clone()
    };
    match last {
        Some(report) => {
            let topic = report.topic.clone();
            let (blocks, _) = present((*report).clone());
            Some((topic, blocks))
        }
        None => {
            changed();
            None
        }
    }
}

/// Where the board's blocks are inside `area` right now (the shown report, the spotlight and the
/// research block): what the emblem must not cover.
pub fn visible_rects(area: Rect) -> Vec<Rect> {
    let b = board();
    let mut out = Vec::new();
    if let Some(shown) = &b.shown {
        out.extend(layout(&shown.report, area, b.research.is_some()).into_iter().flatten());
        if b.spotlight.is_some() {
            out.push(spotlight_rect(area));
        }
        if b.research.is_some() {
            // Beside a report the agent goes straight to its cell (no intro over the board).
            out.push(research_rect_for(Some(&shown.report), area));
        }
    } else if let Some(r) = &b.research {
        out.push(research_rect_at(research_rect(area), area, r.started.elapsed().as_secs_f32()));
    }
    out
}

/// Whether JAI is still signed off since sign-off `epoch`.
pub fn still_signed_off(epoch: u64) -> bool {
    let b = board();
    b.signed_off && b.sign_off_epoch == epoch && b.shown.is_none()
}

pub fn set_research(status: Option<ResearchStatus>) {
    board().research = status;
    changed();
}

pub fn update_research(f: impl FnOnce(&mut ResearchStatus)) {
    if let Some(r) = board().research.as_mut() {
        f(r);
    }
    changed();
}

pub fn set_pending(report: Report) {
    let mut b = board();
    b.pending = Some(Arc::new(report));
    b.research = None;
    drop(b);
    changed();
}

/// The block a spoken target names: "chart", "the map", "photos", "the second note",
/// "headset share" (words of a block's header). None when nothing fits.
pub fn find_block(report: &Report, target: &str) -> Option<usize> {
    let t = target.to_lowercase();
    let words: Vec<String> = crate::deep_research_engine::terms(&t);
    let wants = |kind: &str| match kind {
        "chart" => ["chart", "graph", "diagram", "bars", "balken"].iter().any(|w| t.contains(w)),
        "map" => ["map", "karte", "location", "where"].iter().any(|w| t.contains(w)),
        "photo" => ["photo", "picture", "image", "gallery", "bild", "foto", "video"].iter().any(|w| t.contains(w)),
        "figure" => ["figure", "stat", "number", "kpi", "zahl"].iter().any(|w| t.contains(w)),
        "note" => ["note", "notiz", "text", "insight"].iter().any(|w| t.contains(w)),
        _ => false,
    };
    let ordinal = ["first", "second", "third", "fourth", "fifth"].iter().position(|o| t.contains(o));
    for kind in ["chart", "map", "photo", "figure", "note"] {
        if wants(kind) {
            let of_kind: Vec<usize> = report.blocks.iter().enumerate().filter(|(_, b)| b.type_word() == kind).map(|(i, _)| i).collect();
            if let Some(i) = ordinal.and_then(|o| of_kind.get(o)).or(of_kind.first()) {
                return Some(*i);
            }
        }
    }
    // Header words ("maximise the headset share").
    report
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            // (Its header and what it is tagged as: "the briefing" names a block tagged BRIEFING.)
            let label = format!("{} {}", b.label, b.tag).to_lowercase();
            (i, words.iter().filter(|w| label.contains(w.as_str())).count())
        })
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .map(|(i, _)| i)
}

/// Whether a spoken target names a block on the board ("headset share").
pub fn names_block(target: &str) -> bool {
    let shown = board().shown.as_ref().map(|s| Arc::clone(&s.report));
    let words = crate::deep_research_engine::terms(&target.to_lowercase());
    shown.is_some_and(|r| !target.trim().is_empty() && (find_block(&r, target).is_some() || (!words.is_empty() && words.iter().all(|w| r.topic.to_lowercase().contains(w.as_str())))))
}

/// Maximises a block (None target: the block Sir last asked about, else the first photo of a
/// place board, else the chart, else the first block). Returns its header.
pub fn maximize(target: Option<&str>) -> Result<String, String> {
    let mut b = board();
    let shown = b.shown.as_ref().ok_or("There's nothing on the board to maximise, Sir.")?;
    let report = Arc::clone(&shown.report);
    let recent = shown
        .highlights
        .iter()
        .enumerate()
        .filter_map(|(i, h)| h.map(|at| (i, at)))
        .filter(|(_, at)| at.elapsed() < Duration::from_secs(90))
        .max_by_key(|(_, at)| *at)
        .map(|(i, _)| i);
    let index = match target.map(str::trim).filter(|t| !t.is_empty() && !matches!(*t, "it" | "that" | "this")) {
        Some(t) => find_block(&report, t).ok_or_else(|| format!("I can't see a block like '{t}' on the board, Sir."))?,
        None => recent
            .or_else(|| report.blocks.iter().position(|b| matches!(b.kind, BlockKind::MediaCard { .. })).filter(|_| report.blocks.iter().any(|b| matches!(b.kind, BlockKind::DarkVectorMap { .. }))))
            .or_else(|| report.blocks.iter().position(|b| matches!(b.kind, BlockKind::BarChart { .. })))
            .unwrap_or(0),
    };
    b.spotlight = Some((index, Instant::now()));
    drop(b);
    crate::tts::play_cue(Earcon::Spotlight);
    changed();
    Ok(report.blocks[index].label.clone())
}

/// Back to the board (all blocks at full brightness). False when nothing was maximised.
pub fn minimize() -> bool {
    let was = board().spotlight.take().is_some();
    if was {
        changed();
    }
    was
}

/// Flips a note stack (the maximised one, else the first with more than one note).
pub fn flip_note(forward: bool) -> Result<String, String> {
    turn_page(if forward { Page::Next } else { Page::Previous })
}

/// A page of a stacked block: the next, the previous or page N (1-based, "page 3").
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Page {
    Next,
    Previous,
    Number(usize),
}

/// Turns the maximised stack (else the first one) to `page`; returns the page's headline.
pub fn turn_page(page: Page) -> Result<String, String> {
    let mut b = board();
    let spot = b.spotlight.map(|(i, _)| i);
    let shown = b.shown.as_mut().ok_or("There's no board up, Sir.")?;
    let stacks: Vec<usize> = shown.report.blocks.iter().enumerate().filter(|(_, b)| matches!(&b.kind, BlockKind::NoteStack { notes } if notes.len() > 1)).map(|(i, _)| i).collect();
    let index = spot.filter(|i| stacks.contains(i)).or(stacks.first().copied()).ok_or("There are no stacked notes on the board, Sir.")?;
    let BlockKind::NoteStack { notes } = &shown.report.blocks[index].kind else { unreachable!() };
    let n = notes.len();
    let front = &mut shown.fronts[index];
    *front = match page {
        Page::Next => (*front + 1) % n,
        Page::Previous => (*front + n - 1) % n,
        Page::Number(p) if (1..=n).contains(&p) => p - 1,
        Page::Number(p) => return Err(format!("That stack only has {n} pages, Sir; there is no page {p}.")),
    };
    let headline = notes[*front].headline.clone();
    drop(b);
    changed();
    Ok(headline)
}

/// A place map's streets and building outlines.
pub type MapData = (Vec<Road>, Vec<Vec<[f64; 2]>>);

/// The streets of a place map arrived (or did not: `data` None). Returns the updated report
/// when that map is on the board.
pub fn fill_map(lat: f64, lon: f64, data: Option<MapData>) -> Option<Report> {
    let mut b = board();
    let shown = b.shown.as_mut()?;
    let mut report = (*shown.report).clone();
    let block = report.blocks.iter_mut().find(|bl| matches!(&bl.kind, BlockKind::DarkVectorMap { lat: a, lon: o, .. } if *a == lat && *o == lon))?;
    if let BlockKind::DarkVectorMap { roads, water, pending, .. } = &mut block.kind {
        if let Some((r, w)) = data {
            *roads = r;
            *water = w;
        }
        *pending = false;
    }
    shown.report = Arc::new(report.clone());
    drop(b);
    changed();
    Some(report)
}

/// Pulses the blocks a question is about (Q&A against the board). Returns their indices.
pub fn focus_for_question(question: &str) -> Vec<usize> {
    let mut b = board();
    let Some(shown) = b.shown.as_mut() else { return Vec::new() };
    let terms: Vec<String> = crate::deep_research_engine::terms(question);
    if terms.is_empty() {
        return Vec::new();
    }
    let scored: Vec<(usize, usize)> = shown
        .report
        .blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let text = block.text().to_lowercase();
            (i, terms.iter().filter(|t| text.contains(t.as_str())).count())
        })
        .filter(|(_, s)| *s >= 2 || (terms.len() <= 2 && *s >= 1))
        .collect();
    let best = scored.iter().map(|(_, s)| *s).max().unwrap_or(0);
    let picked: Vec<usize> = scored.iter().filter(|(_, s)| *s + 1 >= best).map(|(i, _)| *i).take(3).collect();
    let now = Instant::now();
    for i in &picked {
        shown.highlights[*i] = Some(now);
    }
    drop(b);
    if !picked.is_empty() {
        changed();
    }
    picked
}

/// The place on the shown board's map: (place, lat, lon).
/// How many blocks a spoken walk-through covers (about 40 s of speech).
pub const WALKTHROUGH_BLOCKS: usize = 7;
/// JAI's speaking pace for the cue timing (words per second) and the TTS lead-in.
const WORDS_PER_SEC: f32 = 2.6;
const SPEECH_LEAD_SECS: f32 = 0.9;

/// The spoken walk-through of `script` (at most `max` blocks) and, per block, when its sentence
/// starts (seconds after the reply begins) so the block can pulse with it.
pub fn walkthrough_schedule(intro: &str, script: &[(usize, String)], max: usize) -> (String, Vec<(usize, f32)>) {
    let mut line = intro.to_string();
    let mut at = SPEECH_LEAD_SECS + intro.split_whitespace().count() as f32 / WORDS_PER_SEC;
    let mut cues = Vec::new();
    for (index, sentence) in script.iter().filter(|(_, s)| !s.trim().is_empty()).take(max) {
        let sentence = sentence.trim();
        cues.push((*index, at));
        line.push(' ');
        line.push_str(sentence);
        if !sentence.ends_with(['.', '!', '?']) {
            line.push('.');
        }
        at += sentence.split_whitespace().count() as f32 / WORDS_PER_SEC + 0.35;
    }
    (line, cues)
}

pub fn current_place() -> Option<(String, f64, f64)> {
    let b = board();
    let shown = b.shown.as_ref()?;
    shown.report.blocks.iter().find_map(|block| match &block.kind {
        BlockKind::DarkVectorMap { place, lat, lon, .. } => Some((place.clone(), *lat, *lon)),
        _ => None,
    })
}

/// A narrated walk-through of the shown board: every block with something to say, in the
/// report's order, as (block index, one spoken sentence).
pub fn walkthrough() -> Option<Vec<(usize, String)>> {
    let b = board();
    let shown = b.shown.as_ref()?;
    Some(
        shown
            .report
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| !matches!(block.kind, BlockKind::ResearchProgress { .. } | BlockKind::DialogueFeed { .. }))
            .map(|(i, block)| (i, block.spoken_answer()))
            .filter(|(_, line)| !line.trim().is_empty())
            .collect(),
    )
}

/// Pulses block `index` of the shown board (as a board answer does); false when there is no such
/// block.
pub fn highlight(index: usize) -> bool {
    let mut b = board();
    let Some(shown) = b.shown.as_mut() else { return false };
    let Some(slot) = shown.highlights.get_mut(index) else { return false };
    *slot = Some(Instant::now());
    drop(b);
    changed();
    true
}

/// The board for the language model (empty when nothing is up): every block's exact contents.
pub fn live_context() -> String {
    let b = board();
    let mut out = String::new();
    if let Some(r) = &b.research {
        out.push_str(&format!("Research agent: running on \"{}\" ({}, {} sources so far).\n", r.topic, r.phase.to_lowercase(), r.sources));
    }
    if let Some(p) = &b.pending {
        out.push_str(&format!("Finished report waiting to be presented: \"{}\" ({} blocks, {} sources).\n", p.topic, p.blocks.len(), p.sources.len()));
    }
    match &b.shown {
        Some(s) => {
            out.push_str(&format!(
                "HUD board on screen: the \"{}\" report, {} blocks, {} sources. Answer questions about it from these exact figures and name the block you used:\n",
                s.report.topic,
                s.report.blocks.len(),
                s.report.sources.len()
            ));
            for (i, block) in s.report.blocks.iter().enumerate() {
                let text: String = block.text().chars().take(420).collect();
                out.push_str(&format!("[{:02}] {text}\n", i + 1));
            }
        }
        None if b.signed_off => out.push_str("HUD board: signed off (screen down).\n"),
        None => {}
    }
    out
}

// ------------------------------------------------------------------------- persistence ----

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hud_board_reports (
    id INTEGER PRIMARY KEY,
    topic TEXT NOT NULL,
    created_at TEXT NOT NULL,
    block_count INTEGER NOT NULL,
    source_count INTEGER NOT NULL,
    report_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS hud_board_reports_topic ON hud_board_reports(topic);
";

fn open_db(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    let _ = conn.busy_timeout(Duration::from_secs(3));
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

pub fn save_report(path: &str, report: &Report) -> Option<i64> {
    let conn = open_db(path).map_err(|e| warn!("[BOARD] Report store unavailable: {e}")).ok()?;
    let json = serde_json::to_string(report).ok()?;
    conn.execute(
        "INSERT INTO hud_board_reports (topic, created_at, block_count, source_count, report_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![report.topic, report.created_at, report.blocks.len() as i64, report.sources.len() as i64, json],
    )
    .map_err(|e| warn!("[BOARD] Could not save the report: {e}"))
    .ok()?;
    Some(conn.last_insert_rowid())
}

/// The newest saved report whose topic shares the most words with `topic` (or the newest one).
pub fn load_report(path: &str, topic: Option<&str>) -> Option<Report> {
    let conn = open_db(path).ok()?;
    let rows = list_reports_in(&conn, 200);
    let wanted: Vec<String> = topic.map(topic_words).unwrap_or_default();
    let id = if wanted.is_empty() {
        rows.first().map(|r| r.0)?
    } else {
        rows.iter()
            .map(|r| {
                let have = topic_words(&r.1);
                (r.0, wanted.iter().filter(|w| have.contains(w)).count())
            })
            .filter(|(_, n)| *n > 0)
            .max_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)))
            .map(|(id, _)| id)?
    };
    let json: String = conn.query_row("SELECT report_json FROM hud_board_reports WHERE id = ?1", [id], |r| r.get(0)).optional().ok()??;
    let mut report: Report = serde_json::from_str(&json).ok()?;
    report.id = Some(id);
    Some(report)
}

/// The words of a topic that tell reports apart ("that VR report you gave me" → ["vr"]).
pub fn topic_words(text: &str) -> Vec<String> {
    const SKIP: [&str; 24] = [
        "the", "that", "this", "my", "your", "a", "an", "of", "on", "about", "report", "reports", "you", "gave", "me", "earlier", "before",
        "yesterday", "made", "did", "showed", "again", "from", "one",
    ];
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 2 && !SKIP.contains(w)).map(str::to_string).collect()
}

/// (id, topic, created_at, blocks, sources), newest first.
pub fn list_reports(path: &str, limit: usize) -> Vec<(i64, String, String, usize, usize)> {
    open_db(path).map(|c| list_reports_in(&c, limit)).unwrap_or_default()
}

fn list_reports_in(conn: &Connection, limit: usize) -> Vec<(i64, String, String, usize, usize)> {
    let Ok(mut st) = conn.prepare("SELECT id, topic, created_at, block_count, source_count FROM hud_board_reports ORDER BY id DESC LIMIT ?1") else { return Vec::new() };
    st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as usize, r.get::<_, i64>(4)? as usize)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

pub fn load_report_by_id(path: &str, id: i64) -> Option<Report> {
    let conn = open_db(path).ok()?;
    let json: String = conn.query_row("SELECT report_json FROM hud_board_reports WHERE id = ?1", [id], |r| r.get(0)).optional().ok()??;
    let mut report: Report = serde_json::from_str(&json).ok()?;
    report.id = Some(id);
    Some(report)
}

// ------------------------------------------------------------------------------- speech ----

/// "thirteen", "forty-two" (the counts JAI says aloud).
pub fn number_words(n: usize) -> String {
    const ONES: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
        "sixteen", "seventeen", "eighteen", "nineteen",
    ];
    const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
    match n {
        0..=19 => ONES[n].to_string(),
        20..=99 if n.is_multiple_of(10) => TENS[n / 10].to_string(),
        20..=99 => format!("{}-{}", TENS[n / 10], ONES[n % 10]),
        _ => n.to_string(),
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{} {word}{}", number_words(n), if n == 1 { "" } else { "s" })
}

/// "Screen's awake. Report's up — thirteen blocks on the HUD, notes stacked at the back, and
/// forty-two sources behind it."
pub fn presented_line(blocks: usize, sources: usize, stacked: bool) -> String {
    let stack = if stacked { ", notes stacked at the back," } else { "" };
    format!("Screen's awake. Report's up — {} on the HUD{stack} and {} behind it.", plural(blocks, "block"), plural(sources, "source"))
}

/// "Sir, state of VR report's done. Thirteen blocks, forty-two sources. Want me to put it on
/// screen and walk you through it?"
pub fn finished_line(topic: &str, blocks: usize, sources: usize) -> String {
    let mut counts = format!("{}, {}.", plural(blocks, "block"), plural(sources, "source"));
    if let Some(first) = counts.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    format!("Sir, {topic} report's done. {counts} Want me to put it on screen and walk you through it?")
}

fn wake_line(before: &ScreenState) -> String {
    match before {
        ScreenState::SignedOff => "I'm here, Sir. Screen's still down from the sign-off, so nothing's in front of you.".into(),
        ScreenState::AwakeEmpty => "At your service, Sir.".into(),
        ScreenState::BoardActive { topic, .. } => format!("Right here, Sir. The {topic} report is still on the board."),
    }
}

fn spoken(display: impl AsRef<str>, line: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref().trim_end(), line.as_ref().trim())
}

// -------------------------------------------------------------------------------- tool ----

pub const TOOL: &str = "hud_board";

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": TOOL,
            "description": "Your HUD board of blocks (notes, charts, figures, photos, maps). research (topic): a research agent reads 20-50 web sources into a report, announced when done; present: shows it; recall (topic): an earlier report. show_map (place): a dark map with photos, for a venue you picked when Sir asks where to go. maximize / minimize / pin / copy / export (target: a block or words of its header; export format markdown or json), flip_note, find (target: words in every block; empty clears), tidy (windows in a grid around the orb), walkthrough ('walk me through it'), tour ('show me around': the HUD's own parts, one by one), close (target: an in-app block such as 'feature lab'; none: the board), remove (one block), clear (everything leaves, screen stays), sign_off (screen down), restore ('bring back the screen'), wake, status. Questions about the board are answered from the live context.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["research", "present", "walkthrough", "tour", "recall", "show_map", "maximize", "minimize", "flip_note", "pin", "copy", "export", "find", "tidy", "close", "remove", "clear", "sign_off", "wake", "restore", "status"]},
                    "topic": {"type": "string", "description": "research / recall: the subject."},
                    "place": {"type": "string", "description": "show_map: the place or venue."},
                    "target": {"type": "string", "description": "maximize / close / remove / pin / copy / export: which block. find: the words to look for."},
                    "format": {"type": "string", "enum": ["markdown", "json"], "description": "export: the file format (default markdown)."},
                    "direction": {"type": "string", "enum": ["next", "previous"]},
                    "page": {"type": "integer", "description": "flip_note: go straight to this page (1-based)."}
                },
                "required": ["action"]
            }
        }
    })]
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().map(str::trim).unwrap_or("")
}

/// The names Sir uses for the board itself ("the HUD board", "web search", "the report"): closing
/// one of them takes the board's report blocks down.
pub fn is_board_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let n = lower.trim().trim_start_matches("the ").trim_start_matches("your ").trim_start_matches("that ").trim();
    matches!(
        n,
        "hud board" | "hudboard" | "board" | "hud" | "report" | "reports" | "web search" | "websearch" | "search" | "search result" | "search results" | "research" | "results" | "blocks"
            | "hud blocks" | "websuche" | "bericht"
    )
}

/// The kind of the block in the spotlight ("note", "map" …).
fn spotlight_kind() -> Option<&'static str> {
    let b = board();
    let (index, _) = b.spotlight?;
    b.shown.as_ref()?.report.blocks.get(index).map(Block::type_word)
}

/// The suggestion-bar key a board action stands for ("next page" said aloud is the bar's
/// NEXT PAGE), so the bar learns from what Sir says as well as from what he clicks.
fn habit_label(action: &str, args: &Value) -> Option<&'static str> {
    let board_itself = arg(args, "target").is_empty() || is_board_name(arg(args, "target"));
    Some(match action {
        "flip_note" | "flip" | "next_note" if arg(args, "direction") == "previous" => "PREV PAGE",
        "flip_note" | "flip" | "next_note" if args.get("page").is_none() => "NEXT PAGE",
        "walkthrough" | "walk_through" | "walk" => "WALK ME THROUGH",
        "minimize" | "minimise" | "back" => "BACK TO BOARD",
        "close" | "close_board" if board_itself => "CLOSE BOARD",
        "research" => "DIG DEEPER",
        _ => return None,
    })
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = arg(args, "action").to_lowercase();
    // What Sir does with the block in the spotlight is what the suggestion bar learns from.
    let habit = spotlight_kind().zip(habit_label(&action, args));
    let done = run_action(tools, args, &action).await;
    if let (Ok(_), Some((kind, label))) = (&done, habit) {
        crate::hud_suggestions::learned(kind, label);
    }
    done
}

async fn run_action(tools: &ToolContext, args: &Value, action: &str) -> Result<String, String> {
    let db = tools.config.read(|c| c.memory_db_path.clone());
    match action {
        "wake" => {
            if crate::vram_guard::is_dormant() {
                crate::vram_guard::wake("are you there");
            }
            let before = wake();
            Ok(spoken(format!("Board: {}", describe(&screen_state())), wake_line(&before)))
        }
        "research" => {
            let topic = arg(args, "topic");
            if topic.is_empty() {
                return Err("Which topic should I research, Sir?".into());
            }
            if researching() {
                return Ok(spoken("A research agent is already running.", "I'm still on the last one, Sir. I'll chime when it's done."));
            }
            crate::board_research::start(tools.clone(), topic.to_string());
            Ok(spoken(format!("Research agent started: {topic}"), format!("Checking. Research agent's on {topic}, Sir. I'll chime when the report's assembled.")))
        }
        "tour" => Ok(spoken(format!("HUD tour: {} parts", crate::gui::TOUR.len()), crate::gui::start_tour())),
        "walkthrough" | "walk_through" | "walk" => {
            // On the board first (the waiting report, else the newest saved one).
            if !is_active() && present_pending().is_none() {
                if let Some(report) = load_report(&db, None) {
                    present(report);
                }
            }
            let script = walkthrough().filter(|s| !s.is_empty()).ok_or("There's nothing on the board to walk you through yet, Sir.")?;
            let (line, cues) = walkthrough_schedule("Here's the walk-through, Sir.", &script, WALKTHROUGH_BLOCKS);
            // Each block pulses as his sentence about it begins.
            tokio::spawn(async move {
                let t0 = tokio::time::Instant::now();
                for (index, at) in cues {
                    tokio::time::sleep_until(t0 + Duration::from_secs_f32(at)).await;
                    if !highlight(index) {
                        break;
                    }
                    changed();
                }
            });
            Ok(spoken(format!("Walk-through: {} blocks", script.len().min(WALKTHROUGH_BLOCKS)), line))
        }
        "present" => {
            if let Some((topic, blocks, sources)) = present_pending() {
                let stacked = board().shown.as_ref().is_some_and(|s| s.report.stacked());
                return Ok(spoken(format!("Presented: {topic} ({blocks} blocks, {sources} sources)"), presented_line(blocks, sources, stacked)));
            }
            if let ScreenState::BoardActive { topic, .. } = screen_state() {
                return Ok(spoken(format!("Already on the board: {topic}"), format!("The {topic} report is already up, Sir.")));
            }
            if researching() {
                return Ok(spoken("Still researching.", "Still assembling it, Sir. I'll chime the moment it's ready."));
            }
            recall(&db, None)
        }
        "recall" => recall(&db, Some(arg(args, "topic")).filter(|t| !t.is_empty())),
        "show_map" => {
            let place = arg(args, "place");
            let place = if place.is_empty() { arg(args, "topic") } else { place };
            if place.is_empty() {
                return Err("Which place should I put on the map, Sir?".into());
            }
            let report = crate::board_research::place_board(tools, place).await?;
            // The first photo block holds the whole gallery.
            let photos = report.blocks.iter().find_map(|b| if let BlockKind::MediaCard { images, .. } = &b.kind { Some(images.len()) } else { None }).unwrap_or(0);
            let title = report.topic.clone();
            let at = report.blocks.iter().find_map(|b| if let BlockKind::DarkVectorMap { lat, lon, .. } = b.kind { Some((lat, lon)) } else { None });
            present(report.clone());
            // The streets fill in when OpenStreetMap answers; the board is saved with them.
            if let Some((lat, lon)) = at {
                tokio::spawn(async move {
                    let data = crate::board_research::street_data(lat, lon).await;
                    let filled = fill_map(lat, lon, data).unwrap_or(report);
                    save_report(&db, &filled);
                });
            }
            let extra = if photos > 0 { format!(", with {} of it beside the map", plural(photos, "photo")) } else { String::new() };
            Ok(spoken(format!("Map: {title}"), format!("I've put {title} on the board, Sir{extra}.")))
        }
        "maximize" | "maximise" | "spotlight" => {
            let label = maximize(Some(arg(args, "target")))?;
            Ok(spoken(format!("Maximised: {label}"), ""))
        }
        "minimize" | "minimise" | "back" => {
            minimize();
            Ok(spoken("Back to the board.", ""))
        }
        // "Close the note" with a board up: that one block leaves it (the last one takes the
        // board with it).
        "remove" | "remove_block" => {
            let target = arg(args, "target");
            let index = board().shown.as_ref().and_then(|s| find_block(&s.report, target));
            match index.and_then(remove_block) {
                Some(label) => {
                    crate::tts::play_earcon(Earcon::Confirm);
                    Ok(spoken(format!("Removed from the board: {label}"), "Done, Sir. It's off the board."))
                }
                None => Ok(spoken(format!("No block '{target}' is open."), "I can't see a block like that, Sir.")),
            }
        }
        // The block menu's entries by voice: "pin the chart", "copy the first note", "export the
        // map as JSON". A board block first, else an in-app window of that name.
        "pin" | "unpin" | "copy" | "export" => {
            let target = arg(args, "target");
            let (report, spot) = {
                let b = board();
                (b.shown.as_ref().map(|s| Arc::clone(&s.report)), b.spotlight.map(|(i, _)| i))
            };
            let on_board = report.and_then(|report| {
                let index = if target.is_empty() || matches!(target, "it" | "that" | "this" | "block" | "this block" | "that block") { spot.or(Some(0)) } else { find_block(&report, target) };
                index.filter(|i| *i < report.blocks.len()).map(|i| (i, report.blocks[i].clone()))
            });
            let window = if on_board.is_some() { None } else { crate::hud_dock::blocks_named(target).into_iter().next() };
            let as_json = arg(args, "format").eq_ignore_ascii_case("json");
            let format = if as_json { crate::block_actions::Format::Json } else { crate::block_actions::Format::Markdown };
            let (title, text, detail) = match (&on_board, &window) {
                (Some((_, block)), _) => (block.label.clone(), block.text(), serde_json::to_value(block).ok()),
                (None, Some((id, title))) => (title.clone(), crate::hud_dock::block_content(id).unwrap_or_else(|| title.clone()), None),
                (None, None) => return Ok(spoken(format!("No block '{target}' is open."), "I can't see a block like that, Sir.")),
            };
            match action {
                "copy" => {
                    crate::clipboard::write(&text).await?;
                    Ok(spoken(format!("Copied: {title} ({} characters)", text.chars().count()), "Copied, Sir."))
                }
                "export" => {
                    let path = crate::block_actions::export(&title, &text, detail.as_ref(), format)?;
                    Ok(spoken(format!("Exported: {}", path.display()), format!("Exported as {}, Sir. It's in your JAI Exports folder.", if as_json { "JSON" } else { "Markdown" })))
                }
                _ => {
                    let pinned = match (&on_board, &window) {
                        (Some((index, _)), _) => toggle_pin(*index).unwrap_or(false),
                        (None, Some((id, _))) => crate::hud_dock::toggle_pin(id),
                        (None, None) => false,
                    };
                    Ok(spoken(format!("{}: {title}", if pinned { "Pinned" } else { "Unpinned" }), if pinned { "Pinned, Sir. It stays." } else { "Unpinned, Sir." }))
                }
            }
        }
        // "Find voltage on the board": every open block that carries the words stays lit.
        "find" | "search" | "filter" => {
            let target = arg(args, "target");
            let needle = if target.is_empty() { arg(args, "topic") } else { target };
            crate::hud_dock::set_search(needle);
            if needle.is_empty() {
                return Ok(spoken("Search cleared.", ""));
            }
            let hits = search_hits(&crate::hud_dock::search()).len() + crate::hud_dock::search_hits().len();
            Ok(spoken(
                format!("Search '{needle}': {hits} block(s)"),
                match hits {
                    0 => format!("Nothing on the board mentions {needle}, Sir."),
                    1 => "One block, Sir. It's the one that's lit.".to_string(),
                    n => format!("{} blocks, Sir. They're the ones that are lit.", number_words(n)),
                },
            ))
        }
        // "Tidy the windows": one grid around the orb.
        "tidy" | "arrange" | "tile" => {
            let moved = crate::hud_dock::retile_all(crate::gui::block_area());
            Ok(spoken(format!("Tidied: {moved} window(s)"), if moved == 0 { "There's nothing open to tidy, Sir." } else { "" }))
        }
        "flip_note" | "flip" | "next_note" => {
            let page = match args.get("page").and_then(Value::as_u64) {
                Some(p) => Page::Number(p as usize),
                None if arg(args, "direction") == "previous" => Page::Previous,
                None => Page::Next,
            };
            let headline = turn_page(page)?;
            Ok(spoken(format!("Note: {headline}"), ""))
        }
        "close" | "close_board" => {
            // A named in-app block ("feature lab", "telemetry", "the plotter", "console"): that
            // block fades out. JAI's own views are never looked up among Windows' programs.
            let target = arg(args, "target");
            if !target.is_empty() && !is_board_name(target) {
                let lower = target.to_lowercase();
                if lower.contains("console") {
                    crate::gui::set_text_console(false);
                    return Ok(spoken("Text console closed.", ""));
                }
                let mut closed = crate::hud_dock::close_named(target);
                if closed.is_empty() {
                    // The network radar, the disk map or the Earth in its own OS window.
                    if let Some(title) = crate::jai_views::close_panel(target) {
                        closed.push(title.to_string());
                    }
                }
                if closed.is_empty() {
                    // A window of JAI's own that is detached (its own OS window).
                    if let Ok(out) = tools.widgets.close_window(target) {
                        tools.focus.forget_jai_window(target);
                        closed.push(out);
                    }
                }
                // What is called that on the board itself: "the briefing", "the timetable".
                closed.extend(close_matching(target));
                if closed.is_empty() {
                    return Ok(spoken(format!("No block '{target}' is open."), format!("The {target} isn't open, Sir.")));
                }
                crate::tts::play_earcon(Earcon::Confirm);
                return Ok(spoken(format!("Closed: {}", closed.join(", ")), ""));
            }
            if close_board() {
                crate::tts::play_earcon(Earcon::Confirm);
                Ok(spoken("Board closed.", "Board closed, Sir."))
            } else {
                Ok(spoken("No board up.", "There's no board up, Sir."))
            }
        }
        // "Clear the board": the board, every in-app block and every secondary window leave at
        // once. The screen stays up and the model stays loaded (no sign-off, no GPU flush).
        "clear" | "clear_view" | "clear_board" => {
            let board = close_board();
            let windows = crate::intent_engine::close_secondary_windows(tools);
            if board || windows > 0 {
                crate::tts::play_earcon(Earcon::Confirm);
                Ok(spoken(format!("Cleared: {windows} block(s) and window(s){}.", if board { ", and the board" } else { "" }), ""))
            } else {
                Ok(spoken("Nothing to clear.", "The board is already clear, Sir."))
            }
        }
        "sign_off" | "sleep" => {
            let epoch = sign_off();
            crate::tts::play_earcon(Earcon::PowerDown);
            // After his last words: the GPU is emptied once he has said goodbye (and only if Sir
            // has not woken him in the meantime).
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if still_signed_off(epoch) {
                    crate::vram_guard::flush_with_reason("HUD sign-off");
                }
            });
            Ok(spoken("Board cleared; screen down.", "Very good, Sir. Screen's going down."))
        }
        "restore" | "bring_back" => {
            if crate::vram_guard::is_dormant() {
                crate::vram_guard::wake("bring back the screen");
            }
            if restore().is_some() {
                return Ok(spoken(format!("Board restored: {}", describe(&screen_state())), "Screen restored, Sir."));
            }
            // Nothing taken down this session: the newest saved report, else just the screen.
            match load_report(&db, None) {
                Some(report) => {
                    present(report);
                    Ok(spoken(format!("Board restored: {}", describe(&screen_state())), "Screen restored, Sir."))
                }
                None => Ok(spoken("Screen awake; no board to restore.", "Screen's back, Sir. There's nothing on the board yet.")),
            }
        }
        "status" => Ok(describe(&screen_state())),
        other => Err(format!("Unknown board action '{other}'.")),
    }
}

fn recall(db: &str, topic: Option<&str>) -> Result<String, String> {
    let Some(report) = load_report(db, topic) else {
        return Ok(match topic {
            Some(t) => spoken(format!("No saved report about {t}."), format!("I don't have a {t} report on file, Sir. Shall I research it?")),
            None => spoken("No saved reports.", "There's no finished report waiting, Sir."),
        });
    };
    let when = report.created_at.get(..10).map(spoken_day).unwrap_or_default();
    let topic = report.topic.clone();
    let stacked = report.stacked();
    let (blocks, sources) = present(report);
    let line = format!("Here's the {topic} report from {when}, Sir. {}", presented_line(blocks, sources, stacked).trim_start_matches("Screen's awake. "));
    Ok(spoken(format!("Recalled: {topic} ({blocks} blocks, {sources} sources)"), line))
}

/// "today", "yesterday", "the 2nd of November".
fn spoken_day(date: &str) -> String {
    let Ok(day) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { return "earlier".into() };
    let today = chrono::Local::now().date_naive();
    match (today - day).num_days() {
        0 => "earlier today".into(),
        1 => "yesterday".into(),
        2..=6 => day.format("%A").to_string(),
        _ => day.format("%-d %B").to_string(),
    }
}

fn describe(state: &ScreenState) -> String {
    match state {
        ScreenState::SignedOff => "signed off (screen down)".into(),
        ScreenState::AwakeEmpty => "awake, nothing on the board".into(),
        ScreenState::BoardActive { topic, block_count, source_count } => format!("'{topic}' on the board: {block_count} blocks, {source_count} sources"),
    }
}

// ----------------------------------------------------------------------------- layout ----

/// A block's place on the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub col: usize,
    pub row: usize,
    pub w: usize,
    pub h: usize,
}

/// First-fit bento packing (row-major, top-left first) of block sizes into `cols × rows`, with
/// `reserved` cells left free. A block that does not fit gets None.
pub fn bento(sizes: &[(usize, usize)], cols: usize, rows: usize, reserved: &[(usize, usize)]) -> Vec<Option<Cell>> {
    let mut used = vec![vec![false; cols]; rows];
    for (c, r) in reserved {
        if *r < rows && *c < cols {
            used[*r][*c] = true;
        }
    }
    sizes
        .iter()
        .map(|&(w, h)| {
            for row in 0..rows.saturating_sub(h - 1) {
                for col in 0..cols.saturating_sub(w - 1) {
                    let free = (row..row + h).all(|r| (col..col + w).all(|c| !used[r][c]));
                    if free {
                        for cells in &mut used[row..row + h] {
                            cells[col..col + w].fill(true);
                        }
                        return Some(Cell { col, row, w, h });
                    }
                }
            }
            None
        })
        .collect()
}

/// The gap between two blocks of the grid: room to breathe, as panels float in Stark's workshop.
const GRID_GAP: f32 = 32.0;
/// The smallest a grid cell is made (a narrower note could not hold a headline).
const MIN_CELL: [f32; 2] = [250.0, 150.0];

/// A report's grid on a board area: its columns and rows, and each block's cells (in block
/// order; None for a block the screen has no room for).
struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Option<Cell>>,
}

/// The grid for `report` on `area` with `reserve` cells kept free (the emblem's corner, the
/// research block's). Three rows and three to six columns whenever that is enough — a wide
/// screen with a normal report. A tall screen gets fewer columns and more rows; a report with
/// more than the screen can take as it is has its last large blocks (charts, maps, videos) made
/// ordinary ones. Every block gets a place as long as the screen has one cell for each.
fn grid(report: &Report, area: Rect, research: bool) -> Grid {
    let reserve = 1 + usize::from(research);
    let max_cols = (((area.width() + GRID_GAP) / (MIN_CELL[0] + GRID_GAP)).floor() as usize).clamp(2, 6);
    let max_rows = (((area.height() + GRID_GAP) / (MIN_CELL[1] + GRID_GAP)).floor() as usize).clamp(3, 8);
    let mut sizes: Vec<(usize, usize)> = report.blocks.iter().map(Block::cells).collect();
    let demand = |sizes: &[(usize, usize)]| sizes.iter().map(|(w, h)| w * h).sum::<usize>() + reserve;
    // More than the screen holds: the last large blocks give up their extra cells.
    while demand(&sizes) > max_cols * max_rows {
        match sizes.iter().rposition(|(w, h)| w * h > 1) {
            Some(i) => sizes[i] = (1, 1),
            None => break,
        }
    }
    let mut rows = demand(&sizes).div_ceil(max_cols).clamp(3, max_rows);
    loop {
        let cols = demand(&sizes).div_ceil(rows).clamp(3.min(max_cols), max_cols);
        let mut reserved = vec![(cols - 1, rows - 1)];
        if research {
            reserved.push((0, rows - 1));
        }
        // Big blocks first, so the chart and the map anchor the top-left.
        let mut order: Vec<usize> = (0..sizes.len()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(sizes[*i].0 * sizes[*i].1));
        let packed = bento(&order.iter().map(|i| sizes[*i]).collect::<Vec<_>>(), cols, rows, &reserved);
        let mut cells = vec![None; sizes.len()];
        for (k, cell) in packed.into_iter().enumerate() {
            cells[order[k]] = cell;
        }
        // Large blocks can leave holes no other block fits: one more row, then smaller blocks.
        if cells.iter().all(Option::is_some) {
            return Grid { cols, rows, cells };
        }
        if rows < max_rows {
            rows += 1;
        } else if let Some(i) = sizes.iter().rposition(|(w, h)| w * h > 1) {
            sizes[i] = (1, 1);
        } else {
            // More blocks than the screen has cells: the ones that fit are shown.
            return Grid { cols, rows, cells };
        }
    }
}

/// The rectangles of a report's blocks inside `area` (None: the screen has no cell left for
/// it). The bottom-right cell is left for the docked emblem, the bottom-left one for a running
/// research block.
/// Each block is as big as what it shows (`natural_size`) inside its cell(s), drawn towards the
/// middle of the board.
pub fn layout(report: &Report, area: Rect, research: bool) -> Vec<Option<Rect>> {
    cells(report, area, research)
        .into_iter()
        .zip(&report.blocks)
        .map(|(slot, block)| slot.map(|slot| towards_centre(slot, natural_size(block, slot.size(), picture_size(block)), area)))
        .collect()
}

/// The grid cell(s) of each block: the room it may take.
pub fn cells(report: &Report, area: Rect, research: bool) -> Vec<Option<Rect>> {
    let grid = grid(report, area, research);
    let cw = (area.width() - GRID_GAP * (grid.cols as f32 - 1.0)) / grid.cols as f32;
    let rh = (area.height() - GRID_GAP * (grid.rows as f32 - 1.0)) / grid.rows as f32;
    grid.cells
        .into_iter()
        .map(|cell| {
            cell.map(|c| {
                let min = area.min + vec2(c.col as f32 * (cw + GRID_GAP), c.row as f32 * (rh + GRID_GAP));
                Rect::from_min_size(min, vec2(cw * c.w as f32 + GRID_GAP * (c.w as f32 - 1.0), rh * c.h as f32 + GRID_GAP * (c.h as f32 - 1.0)))
            })
        })
        .collect()
}

/// The smallest a content-sized block is drawn.
const MIN_FIT: [f32; 2] = [190.0, 120.0];

/// What a block needs of its cell for what it shows. In the reference video no two blocks are
/// the same size: a price is a small tile, a long note a tall one, a photo keeps its shape.
/// Reckoned from the length of the text with the painter's fonts in mind, a little generous
/// (text is never cut by it: what does not fit a cell scrolls, as before); never more than the
/// cell. `picture`: the pixel size of the block's first image once it is here.
pub fn natural_size(block: &Block, cell: Vec2, picture: Option<[usize; 2]>) -> Vec2 {
    // The frame round the content: the label row, the source row and the footer, the sides.
    const TOP: f32 = 30.0;
    const BOTTOM: f32 = 30.0 + FOOTER_H;
    const SIDES: f32 = 32.0;
    let rows = |chars: usize, per_char: f32, width: f32| ((chars as f32 * per_char) / width.max(40.0)).ceil().max(1.0);
    let size = match &block.kind {
        BlockKind::NoteStack { notes } => {
            // The longest page decides, so turning the pages never resizes the block.
            let paged = if notes.len() > 1 { 22.0 } else { 0.0 };
            let chars = notes.iter().map(|n| n.body.chars().count()).max().unwrap_or(0);
            let head = notes.iter().map(|n| n.headline.chars().count()).max().unwrap_or(0);
            // A few words need no full cell's width.
            let w = if chars < 110 { (cell.x * 0.8).max(MIN_CELL[0]) } else { cell.x };
            let inner = w.min(cell.x) - SIDES;
            let text = if chars == 0 { 0.0 } else { (rows(chars, 7.0, inner - 8.0) + 1.0) * 18.5 };
            vec2(w, TOP + BOTTOM + paged + rows(head, 12.5, inner).min(3.0) * 24.0 + 8.0 + text)
        }
        BlockKind::HeroFigure { header, figure, note } => {
            let figure_w = figure.chars().count() as f32 * 28.0;
            let header_w = (header.chars().count() as f32 * 8.0).min(230.0);
            let w = figure_w.max(header_w).max(170.0) + SIDES + 8.0;
            let inner = w.min(cell.x) - SIDES;
            let note_h = if note.trim().is_empty() { 0.0 } else { 8.0 + rows(note.chars().count(), 6.2, inner).min(3.0) * 15.5 };
            vec2(w, TOP + BOTTOM + rows(header.chars().count(), 8.0, inner).min(2.0) * 16.0 + 6.0 + 56.0 + note_h)
        }
        BlockKind::MediaCard { images, .. } => {
            // A video is 16:9; a photo has its own shape once it is here.
            let video = images.first().is_some_and(|u| youtube_id(u).is_some());
            let aspect = if video { Some(16.0 / 9.0) } else { picture.filter(|s| s[0] > 0 && s[1] > 0).map(|s| (s[0] as f32 / s[1] as f32).clamp(0.45, 2.4)) };
            match aspect {
                Some(a) => {
                    let (frame_w, frame_h) = (SIDES - 8.0, TOP + BOTTOM + 12.0);
                    let by_width = vec2(cell.x, (cell.x - frame_w) / a + frame_h);
                    if by_width.y <= cell.y {
                        by_width
                    } else {
                        vec2((cell.y - frame_h) * a + frame_w, cell.y)
                    }
                }
                None => cell,
            }
        }
        // A chart, a map, the research and the dialogue blocks fill their room.
        _ => cell,
    };
    vec2(size.x.clamp(MIN_FIT[0].min(cell.x), cell.x), size.y.clamp(MIN_FIT[1].min(cell.y), cell.y))
}

/// The pixel size of a photo block's first picture, once it has loaded.
fn picture_size(block: &Block) -> Option<[usize; 2]> {
    let BlockKind::MediaCard { images, .. } = &block.kind else { return None };
    match media(images.first()?) {
        Some(MediaSlot::Ready(image)) => Some(image.size),
        _ => None,
    }
}

/// `slot` cut to `size` and drawn towards the middle of the board: the blocks gather round its
/// centre and the board's outer edge stays ragged, as in the reference.
fn towards_centre(slot: Rect, size: Vec2, area: Rect) -> Rect {
    let pull = |lo: f32, hi: f32, len: f32, mid: f32| {
        let (centre, quarter) = ((lo + hi) * 0.5, (hi - lo) * 0.25);
        if centre < mid - quarter {
            hi - len
        } else if centre > mid + quarter {
            lo
        } else {
            lo + (hi - lo - len) * 0.5
        }
    };
    let mid = area.center();
    Rect::from_min_size(pos2(pull(slot.left(), slot.right(), size.x, mid.x), pull(slot.top(), slot.bottom(), size.y, mid.y)), size)
}

/// The spotlight frame: 780 × 480 in the middle of the board (smaller on small screens).
pub fn spotlight_rect(area: Rect) -> Rect {
    let size = vec2(SPOTLIGHT_SIZE[0].min(area.width() * 0.92), SPOTLIGHT_SIZE[1].min(area.height() * 0.9));
    Rect::from_center_size(area.center(), size)
}

/// The research block's rectangle beside a report: its reserved bottom-left grid cell (no wider
/// than the column, no taller than the row), else the full-size block.
pub fn research_rect_for(report: Option<&Report>, area: Rect) -> Rect {
    let Some(report) = report else { return research_rect(area) };
    let grid = grid(report, area, true);
    let cw = (area.width() - GRID_GAP * (grid.cols as f32 - 1.0)) / grid.cols as f32;
    let rh = (area.height() - GRID_GAP * (grid.rows as f32 - 1.0)) / grid.rows as f32;
    let size = vec2(RESEARCH_BLOCK_SIZE[0].min(cw), RESEARCH_BLOCK_SIZE[1].min(rh));
    Rect::from_min_size(pos2(area.left(), area.bottom() - size.y), size)
}

/// The research block's rectangle (bottom-left of the board).
pub fn research_rect(area: Rect) -> Rect {
    Rect::from_min_size(pos2(area.left(), area.bottom() - RESEARCH_BLOCK_SIZE[1]), vec2(RESEARCH_BLOCK_SIZE[0], RESEARCH_BLOCK_SIZE[1]))
}

/// Where the research block is `age` seconds after the agent started: it flies in from the left
/// (0.35 s) to the middle of the board at 1.6× its size, holds there while the agent starts
/// (`RESEARCH_INTRO_SECS`), then glides into `slot`.
pub fn research_rect_at(slot: Rect, area: Rect, age: f32) -> Rect {
    let size = vec2((slot.width() * 1.6).min(area.width() * 0.9), (slot.height() * 1.6).min(area.height() * 0.6));
    let centre = Rect::from_center_size(area.center(), size);
    let fly = ease_out(age / 0.35);
    let intro = centre.translate(vec2(-(centre.right() - area.left() + 40.0) * (1.0 - fly), 0.0));
    let t = ease_out(((age - RESEARCH_INTRO_SECS) / RESEARCH_GLIDE_SECS).clamp(0.0, 1.0));
    lerp_rect(intro, slot, t)
}

/// A block's footer code: "0x04A3", stable for its label.
pub fn block_code(label: &str) -> String {
    let h = label.bytes().fold(0x811C_9DC5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    format!("0x{:04X}", h & 0xFFFF)
}

/// "ADDED JUST NOW", "ADDED 4 MIN AGO", "ADDED 2 H AGO".
pub fn added_label(secs: u64) -> String {
    match secs {
        0..=59 => "ADDED JUST NOW".into(),
        60..=3599 => format!("ADDED {} MIN AGO", secs / 60),
        _ => format!("ADDED {} H AGO", secs / 3600),
    }
}

/// A YouTube video's id from a watch link, a short link or one of its thumbnail URLs.
pub fn youtube_id(url: &str) -> Option<String> {
    let id = if let Some(rest) = url.split("ytimg.com/vi/").nth(1) {
        rest.split('/').next()?
    } else if let Some(rest) = url.split("watch?v=").nth(1) {
        rest.split(['&', '#']).next()?
    } else {
        let rest = url.split("youtu.be/").nth(1)?;
        rest.split(['?', '&', '#']).next()?
    };
    (id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then(|| id.to_string())
}

/// The frames a video block cycles through: the cover and YouTube's three automatic stills.
/// How far a photo card is turned: its far edge is this share of the height shorter.
pub const PHOTO_TILT: f32 = 0.08;

/// The texture rect that crops an image of `size` to fill `r` (centred).
fn cover_uv(size: [usize; 2], r: Rect) -> Rect {
    let (iw, ih) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let (rw, rh) = (r.width().max(1.0), r.height().max(1.0));
    let (mut u, mut v) = (1.0, 1.0);
    if iw / ih > rw / rh {
        u = (rw / rh) / (iw / ih);
    } else {
        v = (iw / ih) / (rw / rh);
    }
    Rect::from_min_max(pos2((1.0 - u) / 2.0, (1.0 - v) / 2.0), pos2((1.0 + u) / 2.0, (1.0 + v) / 2.0))
}

/// The corners (top-left, top-right, bottom-right, bottom-left) of `r` seen turned about its
/// vertical axis: `lean` > 0 sends the right edge away (shorter, a little in), < 0 the left.
pub fn tilted_quad(r: Rect, lean: f32, amount: f32) -> [Pos2; 4] {
    let dy = r.height() * amount / 2.0;
    let dx = r.width() * amount * 0.5;
    if lean > 0.0 {
        [r.left_top(), pos2(r.right() - dx, r.top() + dy), pos2(r.right() - dx, r.bottom() - dy), r.left_bottom()]
    } else {
        [pos2(r.left() + dx, r.top() + dy), r.right_top(), r.right_bottom(), pos2(r.left() + dx, r.bottom() - dy)]
    }
}

/// A video's title and its running time, which the research keeps at the end of its caption
/// (" · 12:34").
pub fn video_length(caption: &str) -> (&str, Option<&str>) {
    match caption.rsplit_once(" · ") {
        Some((title, len)) if len.contains(':') && len.bytes().all(|b| b.is_ascii_digit() || b == b':') => (title, Some(len)),
        _ => (caption, None),
    }
}

pub fn video_frames(id: &str) -> Vec<String> {
    let mut out = vec![format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")];
    out.extend((1..=3).map(|k| format!("https://i.ytimg.com/vi/{id}/{k}.jpg")));
    out
}


fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// A slight overshoot at the end (a spring settling).
pub fn ease_out_back(t: f32) -> f32 {
    let (c, x) = (1.70158, t.clamp(0.0, 1.0) - 1.0);
    1.0 + (c + 1.0) * x.powi(3) + c * x.powi(2)
}

/// The open/close animation of a drawer or block: `t` springs 0 (closed) … 1 (open), frame-rate
/// independent; a closing one keeps rendering until `t` is back at 0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UiAnimState {
    pub t: f32,
    pub target_open: bool,
}

impl UiAnimState {
    /// Advances one frame and returns `t`.
    pub fn step(&mut self, dt: f32) -> f32 {
        let dt = dt.clamp(0.0, 0.1);
        if self.target_open {
            self.t += (1.0 - self.t) * (1.0 - (-22.0 * dt).exp());
            if self.t > 0.995 {
                self.t = 1.0;
            }
        } else {
            self.t -= self.t * (1.0 - (-24.0 * dt).exp());
            if self.t <= 0.005 {
                self.t = 0.0;
            }
        }
        self.t
    }

    /// Still on screen (open, opening or closing).
    pub fn visible(&self) -> bool {
        self.t > 0.0
    }

    /// Mid-transition: the HUD repaints at 60 FPS meanwhile.
    pub fn moving(&self) -> bool {
        self.t > 0.0 && self.t < 1.0 || self.target_open && self.t < 1.0
    }
}

/// `r` scaled by `s` about its centre.
fn scale_rect(r: Rect, s: f32) -> Rect {
    Rect::from_center_size(r.center(), r.size() * s)
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t))
}

// ------------------------------------------------------------------------------ media ----

/// Decoded photos for the media blocks, by URL.
#[derive(Clone)]
pub enum MediaSlot {
    Loading,
    Ready(Arc<egui::ColorImage>),
    Failed,
}

/// The pictures by URL, and when they were last drawn.
static MEDIA: LazyLock<Mutex<HashMap<String, (MediaSlot, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A picture (asking for it counts as drawing it).
pub fn media(url: &str) -> Option<MediaSlot> {
    MEDIA.lock().unwrap_or_else(|e| e.into_inner()).get_mut(url).map(|(slot, used)| {
        *used = Instant::now();
        slot.clone()
    })
}

/// A picture as if it had been fetched (pictures of the board without the network).
#[cfg(test)]
pub fn put_media(url: &str, slot: MediaSlot) {
    MEDIA.lock().unwrap_or_else(|e| e.into_inner()).insert(url.to_string(), (slot, Instant::now()));
}

/// Pictures nobody drew for `unused` are let go: research after research, his memory stays flat.
pub fn forget_unused_media(unused: Duration) {
    MEDIA.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, (slot, used)| matches!(slot, MediaSlot::Loading) || used.elapsed() < unused);
}

/// Fetches and decodes a photo in the background (once).
pub fn request_media(url: &str, rt: &tokio::runtime::Handle) {
    {
        let mut m = MEDIA.lock().unwrap_or_else(|e| e.into_inner());
        if m.contains_key(url) {
            return;
        }
        m.insert(url.to_string(), (MediaSlot::Loading, Instant::now()));
    }
    let url = url.to_string();
    rt.spawn(async move {
        let slot = match crate::board_research::fetch_image(&url).await {
            Some(image) => MediaSlot::Ready(Arc::new(image)),
            None => MediaSlot::Failed,
        };
        MEDIA.lock().unwrap_or_else(|e| e.into_inner()).insert(url, (slot, Instant::now()));
        changed();
    });
}

// ---------------------------------------------------------------------------- painting ----

/// What a click on the board did.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardClick {
    Maximized(String),
    Minimized,
    Flipped,
    /// A video block in the spotlight was clicked: it opens on YouTube.
    Watched(String),
    /// A key of the suggestion bar under the spotlight: (the block's kind, the suggestion).
    Suggested(&'static str, crate::hud_suggestions::Suggestion),
    /// An entry of a block's right-click menu.
    Menu(crate::block_actions::BlockAction),
}

/// How far a block that does not answer the search steps back.
const SEARCH_DIM: f32 = 0.22;
thread_local! {
    /// How far the title of the block being painted has decoded (1: whole).
    static DECODING: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}
/// Opacity of the other blocks while the pointer rests on one.
const HOVER_DIM: f32 = 0.85;

/// Carries out an entry of the right-click menu on block `index` of the board.
fn block_menu(ctx: &egui::Context, index: usize, block: &Block, chosen: crate::block_actions::BlockAction) {
    use crate::block_actions::{BlockAction, Format};
    let export = |format| {
        let detail = serde_json::to_value(block).ok();
        match crate::block_actions::export(&block.label, &block.text(), detail.as_ref(), format) {
            Ok(path) => info!("[BOARD] Block '{}' exported to {}", block.label, path.display()),
            Err(e) => warn!("[BOARD] Could not export '{}': {e}", block.label),
        }
    };
    match chosen {
        BlockAction::Copy => crate::block_actions::copy(ctx, &block.text()),
        BlockAction::Pin => {
            toggle_pin(index);
        }
        BlockAction::Maximize => {
            board().spotlight = Some((index, Instant::now()));
            crate::tts::play_cue(Earcon::Spotlight);
            changed();
        }
        BlockAction::ExportMarkdown => export(Format::Markdown),
        BlockAction::ExportJson => export(Format::Json),
        BlockAction::Close => {
            if remove_block(index).is_some() {
                crate::tts::play_cue(Earcon::Dismiss);
            }
        }
    }
}

/// The board's painter state (textures live with the window; `added`: this frame's "ADDED …"
/// footer).
#[derive(Default)]
pub struct BoardView {
    textures: HashMap<String, egui::TextureHandle>,
    added: String,
    /// The spotlight's grow/shrink, the block it shows and the board that block belongs to.
    spot: UiAnimState,
    spot_index: usize,
    spot_board: Option<Instant>,
    /// Where Sir dragged each block (by index) on the board shown at `moved_board`.
    moved: HashMap<usize, Vec2>,
    moved_board: Option<Instant>,
    /// The block being dragged and where it was when the drag began.
    drag_from: Option<(usize, Vec2)>,
    /// How far a long note is scrolled, per (block, page).
    scroll: HashMap<(usize, usize), f32>,
    /// The block the pointer rests on (the last one, while the others come back) and how far
    /// the others have stepped back from it (0..1).
    hover: (Option<usize>, f32),
    /// The suggestion bar's keys for the block in the spotlight, and the generation of Sir's
    /// habits they were ranked at (asked again only when the block or the habits change).
    suggested: Option<(crate::hud_suggestions::Subject, u64, Vec<crate::hud_suggestions::Suggestion>)>,
    /// The blocks that stand in windows of their own (he is the desktop's background).
    cards: HashMap<egui::ViewportId, Card>,
    /// A block's window was made in this frame: the next block's waits for the next frame.
    /// (Making a window takes a quarter of a second; five in one frame froze the board for
    /// more than two. One a frame, they come up one after the other.)
    card_made: bool,
}

/// A block of the board in a window of its own.
struct Card {
    title: String,
    /// Where its window came up (screen points). Never changed after: it is Sir's to move.
    place: Rect,
    /// When its window showed itself (its entrance counts from there).
    drawn: Option<Instant>,
    /// Where its window stood before it grew into the spotlight (x, y, w, h in pixels).
    small: Option<[i32; 4]>,
    /// Asked for in this frame.
    seen: bool,
}

/// The room round a block in a window of its own: its glow, and the grips its window is sized by.
const CARD_MARGIN: f32 = 10.0;
/// The room under a block in the spotlight for the keys of what Sir may want next.
const CARD_KEYS: f32 = SUGGESTION_KEY[1] + 14.0;

/// Where a block's window stands grown into the spotlight (x, y, w, h in pixels): in the middle
/// of the work area `work` (left, top, right, bottom) of the monitor it is on, the spotlight's
/// size with the room for its keys under it (a window of `size` that is larger already stays
/// as large: the spotlight never makes a block smaller), never larger than that monitor has
/// room for.
fn spotlight_window(work: [i32; 4], ppp: f32, size: [i32; 2]) -> [i32; 4] {
    let (room_w, room_h) = ((work[2] - work[0]) as f32, (work[3] - work[1]) as f32);
    let w = ((SPOTLIGHT_SIZE[0] + 2.0 * CARD_MARGIN) * ppp).max(size[0] as f32).min(room_w * 0.92).round();
    let h = ((SPOTLIGHT_SIZE[1] + 2.0 * CARD_MARGIN) * ppp).max(size[1] as f32).min(room_h * 0.9 - CARD_KEYS * ppp).round() + (CARD_KEYS * ppp).round();
    [work[0] + ((room_w - w) / 2.0).round() as i32, work[1] + ((room_h - h) / 2.0).round() as i32, w as i32, h as i32]
}

/// A click on the block in the open spotlight: a video opens in the browser, a stack of notes
/// turns its page, anything else goes back to its place.
fn spotlight_click(block: &Block, rt: &tokio::runtime::Handle) -> BoardClick {
    let video = match &block.kind {
        BlockKind::MediaCard { images, .. } => images.first().and_then(|u| youtube_id(u)),
        _ => None,
    };
    if let Some(id) = video {
        let url = format!("https://www.youtube.com/watch?v={id}");
        let open = url.clone();
        rt.spawn(async move {
            if let Err(e) = crate::apps::open_default_browser(&open).await {
                warn!("[BOARD] Could not open the video: {e}");
            }
        });
        crate::tts::play_earcon(Earcon::Confirm);
        BoardClick::Watched(url)
    } else if matches!(&block.kind, BlockKind::NoteStack { notes } if notes.len() > 1) {
        let _ = flip_note(true);
        BoardClick::Flipped
    } else {
        minimize();
        BoardClick::Minimized
    }
}

/// What the suggestion bar needs to know about block `index` of a report (`front`: the page of
/// a note stack that is in front).
fn subject_of(report: &Report, index: usize, front: usize) -> crate::hud_suggestions::Subject {
    let block = &report.blocks[index];
    let (pages, headline) = match &block.kind {
        BlockKind::NoteStack { notes } => (notes.len(), notes.get(front).map(|n| n.headline.clone()).unwrap_or_default()),
        BlockKind::HeroFigure { header, .. } => (1, header.clone()),
        BlockKind::BarChart { caption, .. } | BlockKind::MediaCard { caption, .. } => (1, caption.clone()),
        BlockKind::DarkVectorMap { place, .. } => (1, place.clone()),
        BlockKind::ResearchProgress { .. } | BlockKind::DialogueFeed { .. } => (1, String::new()),
    };
    crate::hud_suggestions::Subject {
        kind: block.type_word(),
        topic: report.topic.clone(),
        headline,
        page: front.min(pages.saturating_sub(1)),
        pages,
        blocks: report.blocks.len(),
        source: report.sources.iter().find(|s| s.starts_with("http")).cloned(),
    }
}

/// The `[ < PREV ]` / `[ NEXT > ]` keys of a stacked block (88 × 26 px in its bottom corners).
fn nav_keys(front: Rect) -> [Rect; 2] {
    let (w, h) = (PAGE_KEY[0], PAGE_KEY[1]);
    let y = front.bottom() - 8.0 - h;
    [Rect::from_min_size(pos2(front.left() + 8.0, y), vec2(w, h)), Rect::from_min_size(pos2(front.right() - 8.0 - w, y), vec2(w, h))]
}

/// The page keys' size (the least a `[ < PREV ]` / `[ NEXT > ]` hitbox may be is 84 × 24 px).
pub const PAGE_KEY: [f32; 2] = [88.0, 26.0];

/// Where the `[ < PREV ]` and `[ NEXT > ]` keys of the board's first stacked block are inside
/// `area` right now (the spotlight's when that block is maximised). None without a stack.
pub fn page_keys(area: Rect) -> Option<[Rect; 2]> {
    let b = board();
    let shown = b.shown.as_ref()?;
    let (index, pages) = shown.report.blocks.iter().enumerate().find_map(|(i, block)| match &block.kind {
        BlockKind::NoteStack { notes } if notes.len() > 1 => Some((i, notes.len())),
        _ => None,
    })?;
    let slot = match b.spotlight {
        Some((spot, _)) if spot == index => spotlight_rect(area),
        _ => layout(&shown.report, area, b.research.is_some()).get(index).copied().flatten()?,
    };
    Some(nav_keys(front_rect(slot, Some((shown.fronts[index], pages)))))
}

/// The page in front of the board's first stacked block (0-based) and how many it has.
pub fn front_page() -> Option<(usize, usize)> {
    let b = board();
    let shown = b.shown.as_ref()?;
    shown.report.blocks.iter().enumerate().find_map(|(i, block)| match &block.kind {
        BlockKind::NoteStack { notes } if notes.len() > 1 => Some((shown.fronts[i].min(notes.len() - 1), notes.len())),
        _ => None,
    })
}

/// Which page key (if any) of a stacked `block` in `slot` the pointer is on.
fn nav_key_hit(block: &Block, front: usize, slot: Rect, pointer: Option<Pos2>) -> Option<Page> {
    let BlockKind::NoteStack { notes } = &block.kind else { return None };
    if notes.len() < 2 {
        return None;
    }
    let [prev, next] = nav_keys(front_rect(slot, Some((front, notes.len()))));
    let at = pointer?;
    if prev.expand(2.0).contains(at) {
        Some(Page::Previous)
    } else if next.expand(2.0).contains(at) {
        Some(Page::Next)
    } else {
        None
    }
}

/// A report block's header keys in its front frame `r`: `CLOSE`, then `PIN` left of it.
fn header_keys(p: &Painter, r: Rect) -> [Rect; 2] {
    let mid = r.top() + holo_frame::HEADER / 2.0;
    let mut right = r.right() - 8.0;
    ["CLOSE", "PIN"].map(|label| {
        let w = holo_kit::key_width(p, label);
        let key = Rect::from_min_size(pos2(right - w, mid - holo_kit::KEY_H / 2.0), vec2(w, holo_kit::KEY_H));
        right -= w + 2.0;
        key
    })
}

/// A report block shows its `[ CLOSE ]` from this width on, and its `[ PIN ]` too from the next.
const KEYS_FROM: f32 = 200.0;
const PIN_FROM: f32 = 330.0;

/// A header key of a report block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderKey {
    Close,
    Pin,
}

/// Which header key (if any) of `block` in `slot` the pointer is on.
fn header_key_hit(p: &Painter, block: &Block, front: usize, slot: Rect, pointer: Option<Pos2>) -> Option<HeaderKey> {
    let stack = match &block.kind {
        BlockKind::NoteStack { notes } => Some((front.min(notes.len().saturating_sub(1)), notes.len())),
        _ => None,
    };
    let r = front_rect(slot, stack);
    if r.width() < KEYS_FROM {
        return None;
    }
    let [close, pin] = header_keys(p, r);
    let at = pointer?;
    if close.contains(at) {
        Some(HeaderKey::Close)
    } else if r.width() >= PIN_FROM && pin.contains(at) {
        Some(HeaderKey::Pin)
    } else {
        None
    }
}

/// The front frame of a block in `slot` (stacked notes give up 4 px per back frame).
fn front_rect(slot: Rect, stack: Option<(usize, usize)>) -> Rect {
    let body = Rect::from_min_max(slot.min, pos2(slot.right(), slot.bottom() - FOOTER_H));
    let depth = stack.filter(|(_, n)| *n > 1).map(|(_, n)| n.min(3) - 1).unwrap_or(0) as f32;
    Rect::from_min_max(pos2(body.left(), body.top() + 4.0 * depth), pos2(body.right() - 4.0 * depth, body.bottom()))
}

use crate::holo::{self, a, lit, mix, stroke, Face, Light};
use crate::{holo_frame, holo_kit};

fn ink() -> Color32 {
    holo::theme().ink()
}

fn muted() -> Color32 {
    holo::theme().muted()
}

fn micro() -> FontId {
    FontId::monospace(11.0)
}

/// The pointer is on `r` of this painter's layer (not on a block in front of it).
fn hovered(p: &Painter, r: Rect) -> bool {
    p.ctx().pointer_hover_pos().is_some_and(|at| r.contains(at) && on_top_at(p, at))
}

/// Nothing of another layer lies over `at`. (egui names only Areas there: the home board paints
/// on the background layer, under no Area at all.)
fn on_top_at(p: &Painter, at: Pos2) -> bool {
    match p.ctx().layer_id_at(at) {
        Some(layer) => layer == p.layer_id(),
        None => p.layer_id().order == egui::Order::Background,
    }
}

/// A tooltip at the pointer for what the board paints (it has no widgets of its own).
fn tip(p: &Painter, text: &str) {
    egui::show_tooltip_text(p.ctx(), p.layer_id(), egui::Id::new(("jai_board_tip", text)), text);
}

/// How many whole rows of `font` fit into `height` (none when not even one does).
fn rows_that_fit(p: &Painter, font: &FontId, height: f32) -> usize {
    let row = p.fonts(|f| f.row_height(font)).max(1.0);
    (height / row).floor().max(0.0) as usize
}

/// A key of the kit in the middle of its hitbox `r` (the hitbox may be taller than a key).
fn key_in(p: &Painter, r: Rect, label: &str, on: bool, alpha: f32) -> bool {
    let (hover, down) = p.ctx().input(|i| (i.pointer.hover_pos(), i.pointer.primary_down()));
    let hot = hover.is_some() && hovered(p, r);
    holo_kit::key(p, Rect::from_center_size(r.center(), vec2(r.width(), holo_kit::KEY_H.min(r.height()))), label, hot as u8 as f32, hot && down, on, alpha);
    hot
}

/// Corner ticks on `r` (`holo::corner_ticks`).
pub fn brackets(p: &Painter, r: Rect, colour: Color32) {
    holo::corner_ticks(p, r, holo_frame::TICK, colour);
}

/// The block JAI is talking about: its frame as a line of light, beating at `strength` (0-1).
fn pulse_light(p: &Painter, r: Rect, strength: f32) {
    let mut outline = holo::plate(r, [0.0, 0.0, holo_frame::CUT, 0.0]);
    outline.push(outline[0]);
    let mut light = Light::new(p);
    light.neon(&outline, false, 1.6, a(holo::theme().accent(), strength), 1.2);
    light.paint(p);
}

/// The block's plate (`holo_frame::plate`): its number on the board as the index tab, its title,
/// a lamp while it is loading or live, its tag and its keys in the header, a note's page keys at
/// its foot, and under it the serial line (`footer`: "ADDED JUST NOW"; None for blocks outside
/// the grid). `open` (0-1) is its entrance: the plate unfolds, then the content resolves.
/// Returns the content rect and how visible the content is.
#[allow(clippy::too_many_arguments)]
fn frame(p: &Painter, slot: Rect, block: &Block, index: Option<usize>, active: bool, stack: Option<(usize, usize)>, footer: Option<&str>, keys: Option<bool>, open: f32) -> (Rect, f32) {
    let t = holo::theme();
    let body = if footer.is_some() { Rect::from_min_max(slot.min, pos2(slot.right(), slot.bottom() - FOOTER_H)) } else { slot };
    // Notes stacked at the back: the top and right edges of each sheet, all inside the block's
    // slot (the front frame gives up 4 px per sheet at its top and right).
    let depth = stack.filter(|(_, n)| *n > 1).map(|(_, n)| n.min(3) - 1).unwrap_or(0);
    let r = if footer.is_some() { front_rect(slot, stack) } else { Rect::from_min_max(pos2(body.left(), body.top() + 4.0 * depth as f32), pos2(body.right() - 4.0 * depth as f32, body.bottom())) };
    let clock = p.ctx().input(|i| i.time);
    let focus = p.ctx().animate_bool_with_time(egui::Id::new(("jai_board_focus", &block.label, index)), active, crate::holo_motion::TOUCH);
    // (Its keys' room is kept free of the title.)
    let shown_keys = keys.filter(|_| r.width() >= KEYS_FROM).map_or(0, |_| if r.width() >= PIN_FROM { 2 } else { 1 });
    let key_rects = header_keys(p, r);
    let keys_left = if shown_keys > 0 { key_rects[shown_keys - 1].left() - 8.0 } else { r.right() - holo_frame::PAD };
    let tab = index.map(|i| format!("{:02}", i + 1)).unwrap_or_default();
    let laid = holo_frame::plate(p, r, &holo_frame::Plate { index: &tab, title: &block.label, focus, open, alpha: 1.0, clock, reserve: r.right() - holo_frame::PAD - keys_left + 18.0 });
    let shown = laid.content;
    let inner = if stack.is_some_and(|(_, n)| n > 1) { Rect::from_min_max(laid.body.min, laid.body.max - vec2(0.0, PAGE_KEY[1] + 2.0)) } else { laid.body };
    if shown <= 0.003 {
        return (inner, 0.0);
    }
    for k in 1..=depth {
        let back = r.translate(vec2(4.0 * k as f32, -4.0 * k as f32));
        holo::path(p, &[back.left_top(), back.right_top(), pos2(back.right(), back.bottom() - holo_frame::CUT)], a(t.ink(), 0.26 * shown / k as f32));
    }
    let mid = r.top() + holo_frame::HEADER / 2.0;
    if let Some(pinned) = keys.filter(|_| shown_keys > 0) {
        // A narrow block keeps its CLOSE alone (the pin is in its menu), so its title has room.
        for (key, (label, on)) in key_rects.into_iter().zip([("CLOSE", false), ("PIN", pinned)]).take(shown_keys) {
            if key_in(p, key, label, on, shown * if active { 1.0 } else { 0.7 }) {
                tip(p, if label == "CLOSE" {
                    "Close this block · say: \"close the board\" for all of them"
                } else if on {
                    "Pinned: it leads the next report too (a click lets it go)"
                } else {
                    "Pin it: it leads the next report too"
                });
            }
        }
    }
    // A title cut short shows whole under the pointer.
    let natural = holo::galley(p, Face::Label, &block.label, 10.5, Color32::WHITE).size().x;
    if natural > laid.title_end - laid.title_start + 0.5 && hovered(p, Rect::from_min_max(pos2(laid.title_start, mid - 9.0), pos2(laid.title_end, mid + 9.0))) {
        tip(p, &block.label);
    }
    // The lamp: only while there is something to wait for (a map loading, an answer coming).
    let busy = matches!(&block.kind, BlockKind::DarkVectorMap { pending: true, .. }) || (keys.is_none() && active);
    let mut after_title = laid.title_end + 9.0;
    if busy {
        let beat = 0.45 + 0.55 * (0.5 + 0.5 * (clock * 5.0).sin() as f32);
        p.rect_filled(Rect::from_center_size(pos2(after_title + 2.5, mid), vec2(5.0, 5.0)), Rounding::ZERO, a(t.accent(), beat * shown));
        after_title += 14.0;
    }
    // The tag ("FACT", "SOURCE: STEAM") only where it fits beside the whole title.
    if !block.tag.trim().is_empty() {
        let tag = holo::galley(p, Face::Mono, block.tag.trim(), 10.0, a(t.faint(), shown));
        if after_title + tag.size().x + 4.0 <= keys_left {
            p.galley(pos2(keys_left - tag.size().x, holo::snap(mid - tag.size().y / 2.0, holo::ppp(p))), tag, t.faint());
        }
    }
    if let Some((front, n)) = stack.filter(|(_, n)| *n > 1) {
        // More than one page: PREV and NEXT at its foot, the page between them.
        let keys = nav_keys(r);
        for (key, label) in keys.into_iter().zip(["PREV", "NEXT"]) {
            key_in(p, key, label, false, shown);
        }
        holo::text(p, pos2(r.center().x, keys[0].center().y), Align2::CENTER_CENTER, Face::Mono, &format!("{:02} / {:02}", front + 1, n), 11.0, a(t.muted(), shown));
    }
    // Under the block: when it was added, and where it came from with its code.
    if let Some(added) = footer {
        let y = r.bottom() + 3.0;
        let code = block_code(&block.label);
        let from = if block.source.trim().is_empty() { code } else { format!("{} · {code}", block.source.trim()) };
        let from = holo::text(p, pos2(r.right(), y), Align2::RIGHT_TOP, Face::Mono, &from, 9.5, a(t.faint(), shown));
        let room = from.left() - r.left() - 10.0;
        if room > 60.0 {
            let added = holo::galley_within(p, Face::Mono, added, 9.5, a(t.faint(), shown), room);
            p.galley(pos2(r.left() + 1.0, y), added, t.faint());
        }
    } else if !block.source.trim().is_empty() {
        let source = holo::galley_within(p, Face::Mono, block.source.trim(), 9.5, a(t.faint(), shown), r.width() * 0.55);
        p.galley(pos2(r.left() + holo_frame::PAD, r.bottom() - 15.0), source, t.faint());
    }
    (inner, shown)
}

impl BoardView {
    /// How far Sir has dragged block `index` from its slot.
    pub fn moved_by(&self, index: usize) -> Vec2 {
        self.moved.get(&index).copied().unwrap_or_default()
    }

    /// The suggestion bar under the spotlight frame `r`: the three things Sir most likely wants
    /// next, as keys; returns the one he clicked.
    fn suggestion_bar(&mut self, ui: &mut Ui, p: &Painter, r: Rect, area: Rect, subject: crate::hud_suggestions::Subject) -> Option<crate::hud_suggestions::Suggestion> {
        let generation = crate::hud_suggestions::generation();
        if self.suggested.as_ref().is_none_or(|(known, at, _)| *known != subject || *at != generation) {
            let keys = crate::hud_suggestions::for_subject(&subject);
            self.suggested = Some((subject, generation, keys));
        }
        let keys = &self.suggested.as_ref()?.2;
        let (w, h, gap) = (SUGGESTION_KEY[0], SUGGESTION_KEY[1], 10.0);
        let total = keys.len() as f32 * w + keys.len().saturating_sub(1) as f32 * gap;
        let y = (r.bottom() + FOOTER_H + 8.0).min(area.bottom() - h);
        let mut chosen = None;
        for (k, key) in keys.iter().enumerate() {
            let rect = Rect::from_min_size(pos2(r.center().x - total / 2.0 + k as f32 * (w + gap), y), vec2(w, h));
            let response = ui.interact(rect, ui.id().with(("hud_suggestion", k)), Sense::click());
            holo_kit::key(p, rect, &key.label, response.hovered() as u8 as f32, response.is_pointer_button_down_on(), false, 1.0);
            if response.clicked() {
                chosen = Some(key.clone());
            }
        }
        chosen
    }

    /// Block `i` of the shown report in a window of its own (he is the desktop's background;
    /// Sir, 2026-11-14: "real Windows windows and not only in-app windows"): the same frame,
    /// keys and picture as on the board, where the board would have put it. A drag moves its
    /// window, and the spotlight is its window grown large on the monitor it stands on.
    /// Returns what a click did and whether it is still moving.
    #[allow(clippy::too_many_arguments)]
    fn card(&mut self, ui: &Ui, i: usize, block: &Block, slot: Rect, shown: &Shown, spot: bool, needle: &str, rt: &tokio::runtime::Handle) -> (Option<BoardClick>, bool) {
        let ctx = ui.ctx();
        let now = Instant::now();
        let nth = shown.report.blocks[..i].iter().filter(|b| b.label == block.label).count();
        let viewport = egui::ViewportId::from_hash_of(("jai_board_block", shown.at, &block.label, nth));
        if !self.cards.contains_key(&viewport) && std::mem::replace(&mut self.card_made, true) {
            return (None, true);
        }
        let card = self.cards.entry(viewport).or_insert_with(|| Card {
            title: format!("{}{} \u{2014} J.A.I.", block.label, if nth > 0 { format!(" ({})", nth + 1) } else { String::new() }),
            place: Rect::from_min_size(crate::gui::board_to_screen(ctx, slot.min), slot.size()).expand(CARD_MARGIN),
            drawn: None,
            small: None,
            seen: true,
        });
        card.seen = true;
        // The spotlight: its window grows on the monitor it stands on, and goes back to where
        // it stood.
        #[cfg(windows)]
        if spot != card.small.is_some() {
            if let Some(window) = crate::own_windows::list().into_iter().find(|w| w.title == card.title) {
                let [l, t, r, b] = window.rect;
                let here = [l, t, r - l, b - t];
                let to = card.small.take().or_else(|| {
                    let monitors = crate::monitors::recent();
                    let on = monitors.iter().find(|m| m.contains((l + r) / 2, (t + b) / 2)).or_else(|| crate::monitors::primary(&monitors))?;
                    card.small = Some(here);
                    Some(spotlight_window(on.work, ctx.pixels_per_point(), [r - l, b - t]))
                });
                // (Grown large it is not where it lives: that place is not the one to keep.)
                crate::own_windows::hold(&card.title, card.small.is_some());
                if let Some(to) = to {
                    crate::own_windows::glide(window.hwnd, here, to);
                }
            }
        }
        let (title, place, mut drawn) = (card.title.clone(), card.place, card.drawn);
        let front = shown.fronts[i];
        let pulse = shown.highlights[i].map(|h| h.elapsed().as_secs_f32()).filter(|s| *s < HIGHLIGHT_SECS);
        let found = !needle.is_empty() && crate::hud_dock::search_matches(needle, &block.label, &block.text());
        let (mut click, mut animating) = (None, pulse.is_some());
        let closed = crate::gui::piece_window(ctx, viewport, &title, place, |ui| {
            let ctx = ui.ctx().clone();
            // Its entrance (its plate unfolds, as on the board) counts from the frame its
            // window shows itself in.
            let t = if ctx.cumulative_pass_nr() < 1 { 0.0 } else { (now.saturating_duration_since(*drawn.get_or_insert(now)).as_secs_f32() / APPEAR_SECS).min(1.0) };
            animating |= t < 1.0;
            let slot = ui.max_rect().shrink(CARD_MARGIN);
            // (In the spotlight the keys of what Sir may want next stand under it.)
            let slot = if spot { Rect::from_min_max(slot.min, slot.max - vec2(0.0, CARD_KEYS)) } else { slot };
            let mut p = ui.painter().clone();
            p.set_opacity(if needle.is_empty() || found { 1.0 } else { SEARCH_DIM });
            let r = slot;
            let response = ui.interact(r, ui.id().with(("hud_block", i)), Sense::click_and_drag());
            if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if response.hovered() {
                let dy = ui.input(|inp| inp.smooth_scroll_delta.y);
                if dy != 0.0 {
                    *self.scroll.entry((i, front)).or_default() -= dy;
                    animating = true;
                }
            }
            let active = pulse.is_some() || response.hovered() || found || spot;
            self.paint_block(&p, r, block, (i, front), active, spot, t, rt);
            if let Some(s) = pulse {
                pulse_light(&p, r, (0.5 + 0.5 * (s * 7.0).cos()) * (1.0 - s / HIGHLIGHT_SECS));
            }
            // Its keys, its menu and a click, as on the board.
            let mut chosen = None;
            let pinned = is_pinned(block);
            response.context_menu(|ui| chosen = crate::block_actions::menu(ui, pinned, false));
            let at = response.interact_pointer_pos();
            let header = header_key_hit(&p, block, front, r, at).filter(|_| response.clicked());
            let header = header.map(|key| if key == HeaderKey::Close { crate::block_actions::BlockAction::Close } else { crate::block_actions::BlockAction::Pin });
            if let Some(chosen) = chosen.or(header) {
                block_menu(&ctx, i, block, chosen);
                animating = true;
                click = Some(BoardClick::Menu(chosen));
            } else if let Some(page) = nav_key_hit(block, front, r, at).filter(|_| response.clicked()) {
                let _ = turn_page(page);
                animating = true;
                click = Some(BoardClick::Flipped);
            } else if response.clicked() && spot {
                click = Some(spotlight_click(block, rt));
            } else if response.clicked() {
                board().spotlight = Some((i, Instant::now()));
                crate::tts::play_cue(Earcon::Spotlight);
                click = Some(BoardClick::Maximized(block.label.clone()));
            }
            if spot && t >= 1.0 {
                let subject = subject_of(&shown.report, i, front);
                let (kind, all) = (subject.kind, ui.max_rect());
                if let Some(chosen) = self.suggestion_bar(ui, &p, r, all, subject) {
                    click = Some(BoardClick::Suggested(kind, chosen));
                }
            }
        });
        // (Closed through Windows itself, Alt+F4 or its key in the taskbar: as by its own key.)
        if closed {
            block_menu(ctx, i, block, crate::block_actions::BlockAction::Close);
            (click, animating) = (Some(BoardClick::Menu(crate::block_actions::BlockAction::Close)), true);
        }
        if let Some(card) = self.cards.get_mut(&viewport) {
            card.drawn = drawn;
        }
        (click, animating)
    }

    /// Paints the research block, the board (cascade, spotlight, collapse) and handles clicks.
    /// Returns what a click did and whether something is still animating.
    pub fn paint(&mut self, ui: &mut Ui, area: Rect, rt: &tokio::runtime::Handle) -> (Option<BoardClick>, bool) {
        let now = Instant::now();
        // The textures of pictures let go (`forget_unused_media`) leave the GPU too.
        {
            let kept = MEDIA.lock().unwrap_or_else(|e| e.into_inner());
            self.textures.retain(|url, _| kept.contains_key(url));
        }
        let (shown, spotlight, leaving, research) = {
            let mut b = board();
            if b.leaving.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() > LEAVE_SECS + 0.6) {
                b.leaving = None;
            }
            (b.shown.clone(), b.spotlight, b.leaving.clone(), b.research.clone())
        };
        let mut animating = research.is_some();
        let painter = ui.painter_at(area.expand(8.0));
        let mut click = None;
        // As the desktop's background every block stands in a window of its own (`card`).
        let own = crate::gui::pieces_in_windows(ui.ctx());
        self.cards.values_mut().for_each(|card| card.seen = false);
        self.card_made = false;

        // The previous board collapsing: fading and sinking, last block first. (Windows of
        // their own simply close.)
        if let Some((report, at)) = leaving.as_ref().filter(|_| !own) {
            let rects = layout(report, area, false);
            let n = report.blocks.len();
            self.added = added_label(0);
            for (i, (block, rect)) in report.blocks.iter().zip(rects).enumerate() {
                let Some(rect) = rect else { continue };
                let t = ((now.duration_since(*at).as_secs_f32() - (n - 1 - i) as f32 * 0.02) / LEAVE_SECS).clamp(0.0, 1.0);
                if t >= 1.0 {
                    continue;
                }
                // (Its plate folds back into its top edge.)
                self.paint_block(&painter, rect, block, (usize::MAX, 0), false, false, 1.0 - t, rt);
            }
            animating = true;
        }

        if let Some(shown) = &shown {
            self.added = added_label(now.saturating_duration_since(shown.at).as_secs());
            let rects = layout(&shown.report, area, research.is_some());
            let spot = spotlight.filter(|(i, _)| *i < shown.report.blocks.len());
            // The spotlight grows out of its slot and, minimised, shrinks back into it (a new
            // board never inherits the old one's spotlight).
            if self.spot_board != Some(shown.at) {
                self.spot = UiAnimState::default();
                self.spot_board = Some(shown.at);
            }
            // A new board starts from its own layout (blocks Sir dragged belong to the old one).
            if self.moved_board != Some(shown.at) {
                self.moved.clear();
                self.scroll.clear();
                self.moved_board = Some(shown.at);
            }
            self.spot.target_open = spot.is_some();
            if let Some((i, _)) = spot {
                self.spot_index = i;
            }
            let spot_t = self.spot.step(ui.input(|i| i.stable_dt));
            let spot_shown = (self.spot.visible() && self.spot_index < shown.report.blocks.len()).then_some(self.spot_index);
            if self.spot.moving() {
                animating = true;
            }
            let needle = crate::hud_dock::search();
            // The pointer resting on a block: the others step back a little (to 85 %).
            let pointer = ui.input(|inp| inp.pointer.hover_pos()).filter(|_| !own && spot_shown.is_none() && self.drag_from.is_none());
            let over = pointer.and_then(|at| rects.iter().enumerate().find_map(|(i, r)| r.filter(|r| r.translate(self.moved.get(&i).copied().unwrap_or_default()).contains(at)).map(|_| i)));
            let (last, eased) = self.hover;
            let eased = crate::holo_motion::approach(eased, if over.is_some() { 1.0 } else { 0.0 }, ui.input(|inp| inp.stable_dt), crate::holo_motion::LEAVE);
            self.hover = (over.or(if eased > 0.0 { last } else { None }), eased);
            animating |= eased > 0.0 && eased < 1.0;
            for (i, (block, rect)) in shown.report.blocks.iter().zip(&rects).enumerate() {
                let Some(rect) = *rect else { continue };
                if own {
                    // (One after the other, as the cascade on the board.)
                    if now >= shown.at + CASCADE_STEP * i as u32 {
                        let (did, moving) = self.card(ui, i, block, rect, shown, spot.is_some_and(|(s, _)| s == i), &needle, rt);
                        click = did.or(click);
                        animating |= moving;
                    } else {
                        animating = true;
                    }
                    continue;
                }
                if spot_shown == Some(i) {
                    continue;
                }
                // Cascade: 30 ms per block, each with its entrance.
                let start = shown.at + CASCADE_STEP * i as u32;
                let t = if now < start { 0.0 } else { (now.duration_since(start).as_secs_f32() / APPEAR_SECS).min(1.0) };
                if t < 1.0 {
                    animating = true;
                }
                if t <= 0.0 {
                    continue;
                }
                let dim = if spot_shown.is_some() { 1.0 - (1.0 - DIM_OPACITY) * spot_t } else { 1.0 };
                // The search across the blocks: what answers it stays lit, the rest steps back.
                let found = !needle.is_empty() && crate::hud_dock::search_matches(&needle, &block.label, &block.text());
                let dim = if needle.is_empty() || found { dim } else { dim * SEARCH_DIM };
                let dim = if self.hover.0.is_some_and(|h| h != i) { dim * (1.0 - (1.0 - HOVER_DIM) * self.hover.1) } else { dim };
                // (Its entrance is its plate's: a line along its top edge, the frame unfolding
                // from it, the title decoding as the content resolves; `paint_block` takes `t`.)
                let moved = self.moved.get(&i).copied().unwrap_or_default();
                let r = rect.translate(moved);
                let mut p = painter.clone();
                p.set_opacity(dim);
                let pulse = shown.highlights[i].map(|h| h.elapsed().as_secs_f32()).filter(|s| *s < HIGHLIGHT_SECS);
                if pulse.is_some() {
                    animating = true;
                }
                // Every block moves with the mouse (drag anywhere on it); a click still maximises.
                let response = ui.interact(r, ui.id().with(("hud_block", i)), Sense::click_and_drag());
                if response.drag_started() {
                    self.drag_from = Some((i, self.moved.get(&i).copied().unwrap_or_default()));
                }
                if response.dragged() {
                    // The whole way the pointer has come since it was pressed: the block stays
                    // exactly under it, the first pixels before the drag was a drag included.
                    let way = ui.input(|inp| inp.pointer.press_origin().zip(inp.pointer.interact_pos()).map(|(from, at)| at - from));
                    match (self.drag_from, way) {
                        (Some((index, base)), Some(way)) if index == i => {
                            self.moved.insert(i, base + way);
                        }
                        _ => *self.moved.entry(i).or_default() += response.drag_delta(),
                    }
                    animating = true;
                }
                if response.drag_stopped() {
                    self.drag_from = None;
                }
                if response.hovered() {
                    let dy = ui.input(|inp| inp.smooth_scroll_delta.y);
                    if dy != 0.0 {
                        *self.scroll.entry((i, shown.fronts[i])).or_default() -= dy;
                        animating = true;
                    }
                }
                let active = pulse.is_some() || response.hovered() || found;
                self.paint_block(&p, r, block, (i, shown.fronts[i]), active, false, t, rt);
                if let Some(s) = pulse {
                    pulse_light(&p, r, (0.5 + 0.5 * (s * 7.0).cos()) * (1.0 - s / HIGHLIGHT_SECS));
                }
                let pinned = is_pinned(block);
                // Right-click: copy, pin, maximize, export, close (the same menu as a window's).
                let mut chosen = None;
                response.context_menu(|ui| chosen = crate::block_actions::menu(ui, pinned, false));
                if let Some(chosen) = chosen {
                    block_menu(ui.ctx(), i, block, chosen);
                    animating = true;
                    click = Some(BoardClick::Menu(chosen));
                }
                let key = nav_key_hit(block, shown.fronts[i], r, response.interact_pointer_pos());
                let header = header_key_hit(&p, block, shown.fronts[i], r, response.interact_pointer_pos());
                if let Some(header) = header.filter(|_| response.clicked()) {
                    // Its [ CLOSE ] and [ PIN ]: the same as the menu's.
                    let chosen = if header == HeaderKey::Close { crate::block_actions::BlockAction::Close } else { crate::block_actions::BlockAction::Pin };
                    block_menu(ui.ctx(), i, block, chosen);
                    animating = true;
                    click = Some(BoardClick::Menu(chosen));
                } else if response.clicked() && key.is_some() {
                    let _ = turn_page(key.unwrap_or(Page::Next));
                    animating = true;
                    click = Some(BoardClick::Flipped);
                } else if response.clicked() && spot.is_none() {
                    let mut b = board();
                    b.spotlight = Some((i, Instant::now()));
                    drop(b);
                    crate::tts::play_cue(Earcon::Spotlight);
                    click = Some(BoardClick::Maximized(block.label.clone()));
                } else if response.clicked() {
                    minimize();
                    click = Some(BoardClick::Minimized);
                }
            }
            // The spotlight frame, gliding between the block's grid slot and the centre. (A
            // block in a window of its own grows where it stands.)
            if let Some(i) = spot_shown.filter(|_| !own) {
                let from = rects[i].unwrap_or_else(|| spotlight_rect(area).shrink(120.0));
                let r = lerp_rect(from, spotlight_rect(area), spot_t);
                let backdrop = painter.clone();
                backdrop.rect_filled(area.expand(8.0), Rounding::ZERO, Color32::from_black_alpha((90.0 * spot_t) as u8));
                let response = ui.interact(r, ui.id().with("hud_spotlight"), Sense::click());
                if response.hovered() {
                    let dy = ui.input(|inp| inp.smooth_scroll_delta.y);
                    if dy != 0.0 {
                        *self.scroll.entry((i, shown.fronts[i])).or_default() -= dy;
                        animating = true;
                    }
                }
                self.paint_block(&painter, r, &shown.report.blocks[i], (i, shown.fronts[i]), true, spot_t > 0.6, 1.0, rt);
                // Under the open spotlight: the three things Sir most likely wants next.
                if spot.is_some() && spot_t > 0.6 {
                    let subject = subject_of(&shown.report, i, shown.fronts[i]);
                    let kind = subject.kind;
                    if let Some(chosen) = self.suggestion_bar(ui, &painter, r, area, subject) {
                        click = Some(BoardClick::Suggested(kind, chosen));
                    }
                }
                let key = nav_key_hit(&shown.report.blocks[i], shown.fronts[i], r, response.interact_pointer_pos());
                // Only the open spotlight takes clicks (not one shrinking back).
                if response.clicked() && spot.is_some() && key.is_some() {
                    let _ = turn_page(key.unwrap_or(Page::Next));
                    animating = true;
                    click = Some(BoardClick::Flipped);
                } else if response.clicked() && spot.is_some() {
                    click = Some(spotlight_click(&shown.report.blocks[i], rt));
                }
            }
        }

        if let Some(r) = &research {
            let slot = research_rect_for(shown.as_ref().map(|s| s.report.as_ref()), area);
            let age = now.saturating_duration_since(r.started).as_secs_f32();
            // The intro in the middle only on an empty board (never over a report Sir is reading).
            let rect = if shown.is_some() { slot } else { research_rect_at(slot, area, age) };
            // Bigger text while it sits in the middle (it scales with its rectangle).
            let scale = (rect.width() / slot.width().max(1.0)).clamp(1.0, 1.6);
            if own {
                // (In a window of its own, where its slot is.)
                let ctx = ui.ctx();
                let viewport = egui::ViewportId::from_hash_of(("jai_board_research", r.started));
                let place = Rect::from_min_size(crate::gui::board_to_screen(ctx, slot.min), slot.size()).expand(CARD_MARGIN);
                let card = self.cards.entry(viewport).or_insert_with(|| Card { title: "RESEARCH \u{2014} J.A.I.".into(), place, drawn: None, small: None, seen: true });
                card.seen = true;
                let (block, added) = (r.block(now), added_label(age as u64));
                crate::gui::piece_window(ctx, viewport, &card.title, card.place, |ui| {
                    let inner = ui.max_rect().shrink(CARD_MARGIN);
                    if ui.interact(inner, ui.id().with("hud_research"), Sense::drag()).drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    paint_research(ui.painter(), inner, &block, 1.0, Some(&added));
                });
            } else {
                paint_research(&painter, rect, &r.block(now), scale, Some(&added_label(age as u64)));
            }
        }
        // A block that left the board takes its window with it.
        self.cards.retain(|viewport, card| {
            if !card.seen {
                crate::gui::piece_window_gone(*viewport);
                // (Gone while grown into the spotlight: its title's place is kept again.)
                crate::own_windows::hold(&card.title, false);
            }
            card.seen
        });
        let loading = shown.as_ref().is_some_and(|s| s.report.blocks.iter().any(|b| matches!(b.kind, BlockKind::DarkVectorMap { pending: true, .. })));
        (click, animating || loading)
    }

    /// Block `index` of the report in `r`: its plate, then what it shows. `open` (0-1) is its
    /// entrance (the plate unfolds, the content resolves) and, run backwards, its exit.
    #[allow(clippy::too_many_arguments)]
    fn paint_block(&mut self, p: &Painter, r: Rect, block: &Block, (index, front): (usize, usize), active: bool, spotlit: bool, open: f32, rt: &tokio::runtime::Handle) {
        let t = holo::theme();
        let stack = match &block.kind {
            BlockKind::NoteStack { notes } => Some((front.min(notes.len().saturating_sub(1)), notes.len())),
            _ => None,
        };
        if matches!(block.kind, BlockKind::ResearchProgress { .. }) {
            return paint_research(p, r, block, 1.0, None);
        }
        let added = std::mem::take(&mut self.added);
        // (A block of a board that is leaving has no index: it takes no number and no keys.)
        let on_board = index != usize::MAX;
        let (inner, shown) = frame(p, r, block, on_board.then_some(index), active, stack, Some(&added), on_board.then(|| is_pinned(block)), open);
        self.added = added;
        if shown <= 0.003 {
            return;
        }
        let mut p = p.with_clip_rect(inner.expand(2.0).intersect(p.clip_rect()));
        p.set_opacity(shown);
        let scale = if spotlit { 1.25 } else { 1.0 };
        match &block.kind {
            BlockKind::NoteStack { notes } => {
                let page = front.min(notes.len() - 1);
                let note = &notes[page];
                // The headline takes up to three rows, but never the room three rows of the text
                // need: a short block shows less of its headline and more of what it says.
                let (head_font, body_font) = (holo::font(Face::Head, 19.0 * scale), FontId::proportional(14.5 * scale));
                let (head_row, body_row) = p.fonts(|f| (f.row_height(&head_font), f.row_height(&body_font)));
                // (epaint lays rows out whole pixels apart: a step of the unrounded height drifts.)
                let body_row = p.round_to_pixel(body_row);
                let head_rows = if note.body.trim().is_empty() { 3 } else { (((inner.height() - 8.0 - 3.0 * body_row) / head_row).floor().max(1.0) as usize).min(3) };
                let h = holo::wrapped(&p, inner.min, inner.width(), Face::Head, &note.headline.to_uppercase(), 19.0 * scale, t.ink(), head_rows);
                // The whole text, never clipped mid-sentence: what does not fit scrolls (wheel).
                let top = inner.min.y + h + 8.0;
                let colour = a(t.ink(), 0.86);
                let room = (inner.bottom() - top).max(0.0);
                // (A block too short for even one row of text under its headline shows none.)
                if room >= body_row {
                    // Whole rows only (Sir's choice of 2026-11-15): turned a row at a time, and
                    // unturned what does not fit ends in "…", fading into the block.
                    let rows = (room / body_row).floor();
                    let room = rows * body_row;
                    let width = (inner.width() - 8.0).max(10.0);
                    let mut galley = p.layout_job(egui::text::LayoutJob::simple(note.body.clone(), body_font.clone(), colour, width));
                    let max = ((galley.size().y - room) / body_row).ceil().max(0.0) * body_row;
                    let turned = self.scroll.get(&(index, page)).copied().unwrap_or(0.0).clamp(0.0, max);
                    self.scroll.insert((index, page), turned);
                    let offset = (turned / body_row).round() * body_row;
                    if offset == 0.0 && max > 0.0 {
                        let mut job = egui::text::LayoutJob::simple(note.body.clone(), body_font, colour, width);
                        job.wrap.max_rows = rows as usize;
                        galley = p.layout_job(job);
                    }
                    let view = Rect::from_min_max(pos2(inner.left(), top), pos2(inner.right(), top + room));
                    let view_p = p.with_clip_rect(view.intersect(p.clip_rect()));
                    view_p.galley(pos2(inner.left(), top - offset), galley, colour);
                    let glass = t.glass();
                    if offset < max {
                        holo::fade(&view_p, view.left(), view.right() - 4.0, view.bottom(), view.bottom() - body_row * 0.6, glass);
                    }
                    if offset > 0.0 {
                        // Turned: a faint edge on top says there is more above (the top row stays readable).
                        holo::fade(&view_p, view.left(), view.right() - 4.0, view.top(), view.top() + body_row * 0.35, glass);
                    }
                    if max > 0.0 {
                        // The track, a hairline, and on it how far the text has been turned.
                        let x = inner.right() - 1.0;
                        holo::rule(&p, pos2(x, top), pos2(x, view.bottom()), a(t.ink(), 0.14));
                        let len = ((room / (room + max)) * room).max(room.min(12.0));
                        let y = top + offset / max * (room - len);
                        p.rect_filled(Rect::from_min_size(pos2(x - 1.0, y), vec2(2.0, len)), Rounding::ZERO, t.accent());
                    }
                }
            }
            BlockKind::HeroFigure { header, figure, note } => {
                // What is measured in small capitals, the measurement thin and large on a rule
                // that starts in the accent, what it means under it.
                let h = holo::wrapped(&p, inner.min, inner.width(), Face::Label, header, 8.5 * scale, t.muted(), 2);
                let fig = holo::text(&p, inner.min + vec2(0.0, h + 4.0), Align2::LEFT_TOP, Face::Figure, figure, 46.0 * scale, t.ink());
                let y = fig.bottom() + 2.0;
                holo::rule(&p, pos2(inner.left(), y), pos2(inner.right(), y), a(t.ink(), 0.14));
                holo::rule(&p, pos2(inner.left(), y), pos2(inner.left() + 24.0, y), a(t.accent(), 0.85));
                // As many rows of the note as really fit under the figure (three at most).
                let rows = rows_that_fit(&p, &holo::font(Face::Body, 12.5 * scale), inner.bottom() - y - 8.0).min(3);
                if rows > 0 {
                    holo::wrapped(&p, pos2(inner.left(), y + 8.0), inner.width(), Face::Body, note, 12.5 * scale, t.muted(), rows);
                }
            }
            BlockKind::BarChart { unit, bars, caption } => paint_chart(&p, inner, unit, bars, caption, scale),
            BlockKind::MediaCard { images, caption } if images.first().and_then(|u| youtube_id(u)).is_some() => {
                let id = images.first().and_then(|u| youtube_id(u)).unwrap_or_default();
                self.paint_video(&p, inner, &id, caption, spotlit, rt);
            }
            BlockKind::MediaCard { images, caption } => {
                if spotlit && images.len() > 1 {
                    self.paint_gallery(&p, inner, images, rt);
                } else if let Some(url) = images.first() {
                    let photo = Rect::from_min_max(inner.min, inner.max - vec2(0.0, 18.0));
                    if spotlit {
                        self.paint_image(&p, photo, url, rt);
                    } else {
                        // On the board a photo is a card turned a little in depth.
                        self.paint_image_tilted(&p, photo, url, rt);
                    }
                    if images.len() > 1 {
                        holo_kit::chip(&p, photo.right_bottom() - vec2(74.0, 20.0), &format!("{} PHOTOS", images.len()), t.ink(), true, 1.0);
                    }
                }
                holo::wrapped(&p, pos2(inner.left(), inner.bottom() - 13.0), inner.width(), Face::Label, caption, 8.0, a(t.ink(), 0.85), 1);
            }
            BlockKind::DarkVectorMap { place, lat, lon, span, roads, water, pending } => paint_map(&p, inner.expand(6.0), place, *lat, *lon, *span, roads, water, *pending),
            BlockKind::ResearchProgress { .. } => {}
            BlockKind::DialogueFeed { heard, reply, streaming } => {
                paint_dialogue_body(&p, inner, heard, reply, *streaming);
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context, url: &str, rt: &tokio::runtime::Handle) -> Option<egui::TextureHandle> {
        // (Asked every frame it is drawn: a picture on the board is never forgotten.)
        match media(url) {
            Some(MediaSlot::Ready(image)) => Some(self.textures.entry(url.to_string()).or_insert_with(|| ctx.load_texture(format!("board:{url}"), (*image).clone(), egui::TextureOptions::LINEAR)).clone()),
            Some(_) => None,
            None => {
                request_media(url, rt);
                None
            }
        }
    }

    /// A photo covering `r` (cropped to fill), or a scanning placeholder while it loads.
    fn paint_image(&mut self, p: &Painter, r: Rect, url: &str, rt: &tokio::runtime::Handle) {
        let t = holo::theme();
        match self.texture(p.ctx(), url, rt) {
            Some(texture) => {
                p.image(texture.id(), r, cover_uv(texture.size(), r), Color32::WHITE);
                holo::outline(p, &holo::plate(r, [0.0; 4]), a(t.ink(), 0.20));
                holo::corner_ticks(p, r, 6.0, a(t.ink(), 0.75));
            }
            // The slot of the picture while it loads; one sentence when it will not come.
            None if matches!(media(url), Some(MediaSlot::Failed)) => holo_kit::empty(p, r, "This picture could not be loaded.", 1.0),
            None => {
                holo::outline(p, &holo::plate(r, [0.0; 4]), a(t.ink(), 0.14));
                holo::slot(p, Rect::from_center_size(r.center(), vec2(26.0, 18.0)), a(t.ink(), 0.40));
            }
        }
    }

    /// A photo as a card turned a few degrees about its vertical axis: the far edge shorter, the
    /// card's edge catching the light along it, the picture cropped to fill like `paint_image`
    /// (drawn in eight strips so the texture does not kink along a diagonal).
    fn paint_image_tilted(&mut self, p: &Painter, r: Rect, url: &str, rt: &tokio::runtime::Handle) {
        let Some(texture) = self.texture(p.ctx(), url, rt) else {
            return self.paint_image(p, r, url, rt);
        };
        let t = holo::theme();
        let uv = cover_uv(texture.size(), r);
        let lean = if url.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)) % 2 == 0 { 1.0 } else { -1.0 };
        let q = tilted_quad(r.shrink2(vec2(2.0, 0.0)), lean, PHOTO_TILT);
        let mut mesh = egui::Mesh::with_texture(texture.id());
        const STRIPS: usize = 8;
        for k in 0..=STRIPS {
            let f = k as f32 / STRIPS as f32;
            let top = q[0].lerp(q[1], f);
            let bottom = q[3].lerp(q[2], f);
            let u = uv.left() + uv.width() * f;
            mesh.vertices.push(egui::epaint::Vertex { pos: top, uv: pos2(u, uv.top()), color: Color32::WHITE });
            mesh.vertices.push(egui::epaint::Vertex { pos: bottom, uv: pos2(u, uv.bottom()), color: Color32::WHITE });
        }
        for k in 0..STRIPS as u32 {
            let (a, b) = (2 * k, 2 * k + 2);
            mesh.add_triangle(a, b, b + 1);
            mesh.add_triangle(a, b + 1, a + 1);
        }
        p.add(Shape::mesh(mesh));
        holo::outline(p, &q, a(t.ink(), 0.24));
        // The card's edge on the far side catches the light.
        let (from, to) = if lean > 0.0 { (q[1], q[2]) } else { (q[0], q[3]) };
        let mut light = Light::new(p);
        light.neon(&[from, to], false, 1.4, a(t.ink(), 0.9), 0.5);
        light.paint(p);
    }

    /// A video block: its cover and YouTube's automatic stills cross-fading every 1.6 s (a moving
    /// preview), a play mark, and in the spotlight the hint that a click opens it.
    fn paint_video(&mut self, p: &Painter, r: Rect, id: &str, caption: &str, spotlit: bool, rt: &tokio::runtime::Handle) {
        let t = holo::theme();
        // (Stills YouTube does not have are left out: one would flash an error between the others.)
        let mut frames: Vec<String> = video_frames(id).into_iter().filter(|u| !matches!(media(u), Some(MediaSlot::Failed))).collect();
        if frames.is_empty() {
            frames = video_frames(id);
            frames.truncate(1);
        }
        let screen = Rect::from_min_max(r.min, r.max - vec2(0.0, 18.0));
        let clock = p.ctx().input(|i| i.time) as f32 / 1.6;
        let (k, fade) = ((clock.floor() as usize) % frames.len(), (clock.fract() - 0.8).max(0.0) / 0.2);
        self.paint_image(p, screen, &frames[k], rt);
        if fade > 0.0 {
            let mut next = p.clone();
            next.set_opacity(fade);
            self.paint_image(&next, screen, &frames[(k + 1) % frames.len()], rt);
        }
        // The play mark in the middle (a ring of light on a dark disc), its running time in
        // the corner, and how far the preview has run along the bottom of the picture.
        let c = screen.center();
        p.circle_filled(c, 18.0, Color32::from_black_alpha(170));
        let mut light = Light::new(p);
        light.ring(c, 18.0, 1.2, a(t.ink(), 0.9));
        let run = screen.width() * (clock / frames.len() as f32).fract();
        light.neon(&[pos2(screen.left(), screen.bottom() - 1.5), pos2(screen.left() + run, screen.bottom() - 1.5)], false, 1.6, t.accent(), 0.8);
        light.paint(p);
        p.add(Shape::convex_polygon(vec![c + vec2(-5.0, -8.0), c + vec2(8.0, 0.0), c + vec2(-5.0, 8.0)], t.ink(), Stroke::NONE));
        let (title, length) = video_length(caption);
        if let Some(length) = length {
            let g = holo::galley(p, Face::Mono, length, 10.5, t.ink());
            let plate = Rect::from_min_size(screen.right_bottom() - g.size() - vec2(14.0, 12.0), g.size() + vec2(8.0, 4.0));
            p.rect_filled(plate, Rounding::ZERO, Color32::from_black_alpha(200));
            p.galley(plate.min + vec2(4.0, 2.0), g, t.ink());
        }
        let line = if spotlit { "Click to watch on YouTube" } else { title };
        holo::wrapped(p, pos2(r.left(), r.bottom() - 13.0), r.width(), Face::Label, line, 8.0, a(t.ink(), 0.85), 1);
    }

    /// The spotlight's multi-photo grid.
    fn paint_gallery(&mut self, p: &Painter, r: Rect, images: &[String], rt: &tokio::runtime::Handle) {
        let n = images.len().min(12);
        let cols = if n <= 4 { 2 } else if n <= 9 { 3 } else { 4 };
        let rows = n.div_ceil(cols);
        let gap = 8.0;
        let cw = (r.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let rh = (r.height() - 18.0 - gap * (rows as f32 - 1.0)) / rows as f32;
        for (k, url) in images.iter().take(n).enumerate() {
            let (c, row) = (k % cols, k / cols);
            let cell = Rect::from_min_size(r.min + vec2(c as f32 * (cw + gap), row as f32 * (rh + gap)), vec2(cw, rh));
            self.paint_image(p, cell, url, rt);
        }
    }
}

/// The `COMMS` block on its own (the HUD paints it in the band above the telemetry strip,
/// outside the report grid). Returns the row of the heard words, if shown (a click there opens
/// the hearing log to put them right).
pub fn paint_dialogue(p: &Painter, r: Rect, block: &Block) -> Option<Rect> {
    if let BlockKind::DialogueFeed { heard, reply, streaming } = &block.kind {
        let (inner, shown) = frame(p, r, block, None, *streaming, None, None, None, 1.0);
        // While he speaks his voice runs along the plate's left edge as a filament.
        let voice = (crate::tts::output_level() * 6.0).clamp(0.0, 1.0);
        if voice > 0.02 {
            let t = holo::theme();
            let half = r.height() / 2.0 * (0.25 + 0.75 * voice);
            let mut light = Light::new(p);
            light.neon(&[pos2(r.left(), r.center().y - half), pos2(r.left(), r.center().y + half)], false, 1.2 + voice, a(mix(t.accent(), t.hot(), voice), 0.5 + 0.5 * voice), 0.6 + 0.6 * voice);
            light.paint(p);
        }
        let mut p = p.with_clip_rect(inner.expand(2.0).intersect(p.clip_rect()));
        p.set_opacity(shown);
        return paint_dialogue_body(&p, inner, heard, reply, *streaming);
    }
    None
}

fn paint_dialogue_body(p: &Painter, inner: Rect, heard: &str, reply: &str, streaming: bool) -> Option<Rect> {
    let t = holo::theme();
    // Who said it stands in a column of its own, so both texts start on one line.
    let column = holo::galley(p, Face::Label, "HEARD", 8.0, Color32::WHITE).size().x + 12.0;
    let width = (inner.width() - column).max(40.0);
    let mut y = inner.top();
    let mut heard_row = None;
    if !heard.trim().is_empty() {
        holo::text(p, pos2(inner.left(), y + 3.0), Align2::LEFT_TOP, Face::Label, "HEARD", 8.0, t.faint());
        let h = holo::wrapped(p, pos2(inner.left() + column, y), width, Face::Body, heard.trim(), 13.0, t.muted(), 1).max(12.0);
        heard_row = Some(Rect::from_min_max(pos2(inner.left(), y - 1.0), pos2(inner.right(), y + h)));
        y += h + 5.0;
    }
    if !reply.trim().is_empty() {
        // The rows that really fit (measured with the font, so the last one is never cut).
        let rows = rows_that_fit(p, &holo::font(Face::Body, 15.0), inner.bottom() - y).max(1);
        holo::text(p, pos2(inner.left(), y + 5.0), Align2::LEFT_TOP, Face::Label, "J.A.I.", 8.0, t.accent());
        let text = if streaming { format!("{} ▌", reply.trim()) } else { reply.trim().to_string() };
        holo::wrapped(p, pos2(inner.left() + column, y), width, Face::Body, &text, 15.0, t.ink(), rows);
    }
    heard_row
}

/// Lit cells of the research bar at `clock`: the filled share of the progress, plus a run of
/// seven cells sweeping back and forth across the rest while the agent works (`running`).
pub fn research_cells(progress: f32, clock: f32, running: bool) -> Vec<f32> {
    let filled = ((progress.clamp(0.0, 1.0) * RESEARCH_CELLS as f32).floor() as usize).min(RESEARCH_CELLS);
    let mut cells = vec![0.18f32; RESEARCH_CELLS];
    for c in cells.iter_mut().take(filled) {
        *c = 0.55;
    }
    if running && filled < RESEARCH_CELLS {
        let span = (RESEARCH_CELLS - filled) as f32;
        let phase = (clock * 0.45).fract();
        let head = filled as f32 + span * if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 };
        for (k, c) in cells.iter_mut().enumerate().skip(filled) {
            let d = (k as f32 - head).abs();
            if d < 3.5 {
                *c = c.max(1.0 - d / 4.5);
            }
        }
    }
    cells
}

/// The research block: what he is looking into, the stage he is at, how far he is (cells: the
/// share done in the accent, a white run sweeping the rest while he works), and what he has
/// found so far.
fn paint_research(p: &Painter, r: Rect, block: &Block, scale: f32, footer: Option<&str>) {
    let BlockKind::ResearchProgress { topic, phase, elapsed_secs, progress, blocks, sources } = &block.kind else { return };
    let t = holo::theme();
    let body = if footer.is_some() { Rect::from_min_max(r.min, pos2(r.right(), r.bottom() - FOOTER_H)) } else { r };
    let secs = *elapsed_secs;
    let clock = p.ctx().input(|i| i.time);
    // (It is live: the plate of the panel in use.)
    let laid = holo_frame::plate(p, body, &holo_frame::Plate { index: "R", title: &block.label, focus: 1.0, open: 1.0, alpha: 1.0, clock, reserve: 54.0 });
    holo::text(p, pos2(body.right() - holo_frame::PAD, body.top() + holo_frame::HEADER / 2.0), Align2::RIGHT_CENTER, Face::Mono, &format!("{:02}:{:02}", secs / 60, secs % 60), 11.5, t.ink());
    let inner = laid.body;
    let title = p.with_clip_rect(inner.expand(2.0).intersect(p.clip_rect()));
    let title_h = holo::wrapped(&title, inner.min, inner.width(), Face::Head, &topic.to_uppercase(), 19.0 * scale, t.ink(), 1);
    if hovered(p, Rect::from_min_size(inner.min, vec2(inner.width(), title_h))) && holo::galley(p, Face::Head, &topic.to_uppercase(), 19.0 * scale, t.ink()).size().x > inner.width() {
        tip(p, topic);
    }
    // Thin cells: the share done, and a bright run sweeping over the rest while it works; how
    // far he is stands at their end.
    let cells = research_cells(*progress, clock as f32, *progress < 1.0);
    let bar_y = inner.top() + title_h + 6.0 * scale;
    let gap = 2.0 * scale;
    let bar_w = inner.width() - 46.0 * scale;
    let cell_w = (bar_w - (RESEARCH_CELLS as f32 - 1.0) * gap) / RESEARCH_CELLS as f32;
    let mut mesh = egui::Mesh::default();
    for (k, light) in cells.iter().enumerate() {
        let cell = Rect::from_min_size(pos2(inner.left() + k as f32 * (cell_w + gap), bar_y), vec2(cell_w, 9.0 * scale));
        // (Done: the accent. The sweep: towards white. The rest: faint ink.)
        let colour = if *light <= 0.18 { a(t.ink(), 0.11) } else if *light <= 0.55 { t.accent() } else { mix(a(t.ink(), 0.11), t.hot(), *light) };
        mesh.add_colored_rect(cell, colour);
    }
    p.add(Shape::mesh(mesh));
    holo::text(p, pos2(inner.right(), bar_y + 4.5 * scale), Align2::RIGHT_CENTER, Face::Figure, &format!("{:.0}%", progress.clamp(0.0, 1.0) * 100.0), 15.0 * scale, t.accent());
    // (Under the cells, never on them: the stage he is at, and what he has found.)
    let foot = bar_y + 15.0 * scale;
    let found = holo::text(p, pos2(inner.right(), foot), Align2::RIGHT_TOP, Face::Label, &format!("{blocks} BLOCKS · {sources} SOURCES"), 8.0 * scale, t.ink());
    let stage = holo::galley_within(p, Face::Label, phase, 8.0 * scale, t.muted(), (found.left() - inner.left() - 10.0).max(10.0));
    p.galley(pos2(inner.left(), foot), stage, t.muted());
    if let Some(added) = footer {
        let y = body.bottom() + 3.0;
        holo::text(p, pos2(body.left() + 1.0, y), Align2::LEFT_TOP, Face::Mono, added, 9.5, t.faint());
        holo::text(p, pos2(body.right(), y), Align2::RIGHT_TOP, Face::Mono, &format!("{} · {:02}.{}", block_code(topic), secs / 60, (secs % 60) / 6), 9.5, t.faint());
    }
}

/// "Nice" grid steps for an axis up to `max` (0, 10, 20, 30, 40).
pub fn axis_ticks(max: f32) -> Vec<f32> {
    let max = max.max(1e-3);
    let raw = max / 4.0;
    let mag = 10f32.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(mag * 10.0);
    let count = (max / step).ceil() as usize;
    (0..=count).map(|k| k as f32 * step).collect()
}

/// A bar chart: panes of glass lit from a bright cap, over a scale of hairlines; the bar under
/// the pointer brightens and says its whole name and exact value; its caption under a rule.
fn paint_chart(p: &Painter, r: Rect, unit: &str, bars: &[Bar], caption: &str, scale: f32) {
    if bars.is_empty() {
        return;
    }
    let t = holo::theme();
    let caption_h = 36.0 * scale;
    let label_h = 18.0;
    let max = bars.iter().map(|b| b.value).fold(0.0f32, f32::max);
    let ticks = axis_ticks(max);
    let top = ticks.last().copied().unwrap_or(max).max(1e-3);
    // 18 px of headroom: the tallest bar's value stands above the top line of the scale.
    let plot = Rect::from_min_max(r.min + vec2(32.0, 18.0), pos2(r.right(), r.bottom() - caption_h - label_h - 8.0));
    for tick in &ticks {
        let y = plot.bottom() - plot.height() * (tick / top);
        holo::rule(p, pos2(plot.left(), y), pos2(plot.right(), y), a(t.ink(), if *tick == 0.0 { 0.26 } else { 0.07 }));
        holo::rule(p, pos2(plot.left() - 4.0, y), pos2(plot.left(), y), a(t.ink(), 0.40));
        holo::text(p, pos2(plot.left() - 7.0, y), Align2::RIGHT_CENTER, Face::Mono, &format!("{}{}", trim_number(*tick), if *tick == top { unit } else { "" }), 10.0, t.muted());
    }
    let slot = plot.width() / bars.len() as f32;
    let width = (slot * 0.52).min(64.0 * scale);
    // The pointer over this block (not over a block in front of it).
    let hover = p.ctx().pointer_hover_pos().filter(|at| p.clip_rect().contains(*at) && on_top_at(p, *at));
    let mut tip = None;
    let mut mesh = egui::Mesh::default();
    let mut light = Light::new(p);
    for (k, bar) in bars.iter().enumerate() {
        let x = plot.left() + slot * (k as f32 + 0.5);
        let h = plot.height() * (bar.value / top).clamp(0.0, 1.0);
        let rect = Rect::from_min_max(pos2(holo::snap(x - width / 2.0, holo::ppp(p)), plot.bottom() - h), pos2(holo::snap(x + width / 2.0, holo::ppp(p)), plot.bottom()));
        let hot = hover.is_some_and(|at| (at.x - x).abs() < slot / 2.0 && at.y > plot.top() && at.y < plot.bottom() + label_h);
        let colour = if hot { mix(t.accent(), t.hot(), 0.4) } else { t.accent() };
        let n = mesh.vertices.len() as u32;
        // (Thick and faint beside thin and bright: the pane holds a tenth of the cap's light.
        // A stronger fill reads as olive on black.)
        for (at, strength) in [(rect.left_top(), 0.17), (rect.right_top(), 0.17), (rect.left_bottom(), 0.0), (rect.right_bottom(), 0.0)] {
            mesh.colored_vertex(at, lit(colour, if hot { strength * 1.8 } else { strength }));
        }
        mesh.add_triangle(n, n + 1, n + 2);
        mesh.add_triangle(n + 1, n + 3, n + 2);
        light.neon(&[rect.left_top(), rect.right_top()], false, 1.5, mix(colour, t.hot(), 0.5), if hot { 1.3 } else { 0.9 });
        for from in [rect.left_top(), rect.right_top()] {
            holo::rule(p, from, pos2(from.x, rect.bottom()), a(colour, 0.35));
        }
        holo::text(p, pos2(x, rect.top() - 4.0), Align2::CENTER_BOTTOM, Face::Figure, &format!("{}{unit}", trim_number(bar.value)), 13.0, t.ink());
        let label = holo::galley_within(p, Face::Label, &bar.label, 8.0, if hot { t.ink() } else { t.muted() }, slot - 4.0);
        p.galley(pos2(holo::snap(x - label.size().x / 2.0, holo::ppp(p)), plot.bottom() + 6.0), label, t.muted());
        if hot {
            tip = Some((format!("{} · {}{unit}", bar.label.to_uppercase(), bar.value), x, rect.top()));
        }
    }
    p.add(Shape::mesh(mesh));
    light.paint(p);
    // The bar under the pointer: its whole name and exact value.
    if let Some((text, x, y)) = tip {
        let g = holo::galley(p, Face::Mono, &text, 11.0, t.ink());
        let size = g.size() + vec2(12.0, 8.0);
        let plate = Rect::from_min_size(pos2((x - size.x / 2.0).clamp(r.left(), (r.right() - size.x).max(r.left())), (y - size.y - 22.0).max(r.top())), size);
        holo_frame::card(p, plate, 1.0, 1.0);
        p.galley(plate.min + vec2(6.0, 4.0), g, t.ink());
    }
    // What it shows, in a plain sentence under a rule.
    let cap = Rect::from_min_max(pos2(r.left(), r.bottom() - caption_h), r.max);
    holo::rule(p, cap.left_top(), cap.right_top(), a(t.ink(), 0.14));
    holo::rule(p, cap.left_top(), cap.left_top() + vec2(24.0, 0.0), a(t.accent(), 0.85));
    holo::wrapped(p, cap.min + vec2(0.0, 6.0), cap.width(), Face::Body, caption, 11.5 * scale, t.muted(), 2);
}

/// Web-Mercator-ish local projection of [lat, lon] into `r` around the centre.
pub fn map_point(r: Rect, lat0: f64, lon0: f64, span: f64, lat: f64, lon: f64) -> Pos2 {
    let k = lat0.to_radians().cos();
    let scale = r.width().min(r.height()) as f64 / span.max(1e-6);
    let dx = (lon - lon0) * k * scale;
    let dy = (lat - lat0) * scale;
    pos2(r.center().x + dx as f32, r.center().y - dy as f32)
}

/// A map drawn in light: the streets in ink (the main ones brighter and broader), water as a
/// dark shore in the deep tone, the place under a reticle in the accent, its name and its
/// coordinates in the corner.
#[allow(clippy::too_many_arguments)]
fn paint_map(p: &Painter, r: Rect, place: &str, lat: f64, lon: f64, span: f64, roads: &[Road], water: &[Vec<[f64; 2]>], pending: bool) {
    let t = holo::theme();
    p.rect_filled(r, Rounding::ZERO, t.void());
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let clock = p.ctx().input(|i| i.time) as f32;
    if pending || roads.is_empty() {
        holo::text(&p, r.center_top() + vec2(0.0, 18.0), Align2::CENTER_TOP, Face::Label, if pending { "Street data loading" } else { "No street data" }, 8.0, t.muted());
        if pending {
            // A line of light sweeps the map while the streets load.
            let y = r.top() + r.height() * (clock * 0.6).fract();
            let mut light = Light::new(&p);
            light.neon(&[pos2(r.left(), y), pos2(r.right(), y)], false, 1.0, a(t.accent(), 0.5), 0.6);
            light.paint(&p);
        }
    }
    let to = |pt: &[f64; 2]| map_point(r, lat, lon, span, pt[0], pt[1]);
    let shore = mix(t.void(), t.deep(), 0.30);
    for w in water {
        let pts: Vec<Pos2> = w.iter().map(to).collect();
        if pts.len() >= 3 && w.first() == w.last() {
            // Lakes and basins: a broad dark shore line (egui fills only convex shapes).
            p.add(Shape::closed_line(pts, stroke(5.0, shore)));
        } else if pts.len() >= 2 {
            p.add(Shape::line(pts, stroke(6.0, shore)));
        }
    }
    for road in roads {
        let (width, alpha) = match road.class {
            0 => (2.2, 1.0),
            1 => (1.6, 0.85),
            2 => (1.1, 0.60),
            _ => (0.7, 0.38),
        };
        let pts: Vec<Pos2> = road.points.iter().map(to).collect();
        if pts.len() >= 2 {
            p.add(Shape::line(pts, stroke(width, a(t.ink(), alpha))));
        }
    }
    // The place: a reticle in the accent, a ring running out from it.
    let c = r.center();
    let beat = (clock * 1.2).fract();
    let mut light = Light::new(&p);
    light.ring(c, 9.0 + 16.0 * beat, 1.2, a(t.accent(), 1.0 - beat));
    light.neon_arc(c, 9.0, 0.0, std::f32::consts::TAU, 1.4, t.accent(), 0.9);
    light.spark(c, 7.0, lit(t.hot(), 0.9));
    light.paint(&p);
    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        holo::rule(&p, c + vec2(dx, dy) * 13.0, c + vec2(dx, dy) * 22.0, a(t.ink(), 0.75));
    }
    p.circle_filled(c, 2.2, t.hot());
    // The place's name on two rows at most, never wider than the block (a long name in a
    // narrow cell was cut at the block's edge), its coordinates under it.
    let title = place.split(',').take(2).collect::<Vec<_>>().join(",").to_uppercase();
    let mut job = egui::text::LayoutJob::simple(title, holo::font(Face::Head, 21.0), t.ink(), (r.width() - 28.0).max(40.0));
    job.wrap.max_rows = 2;
    job.wrap.overflow_character = Some('…');
    let galley = p.layout_job(job);
    let (left, bottom) = (r.left() + 14.0, r.bottom() - 30.0);
    p.galley(pos2(left, bottom - galley.size().y), galley, t.ink());
    holo::text(&p, pos2(left, bottom + 4.0), Align2::LEFT_TOP, Face::Mono, &format!("{lat:.4}° N  {lon:.4}° E"), 10.5, t.muted());
}

/// Tests that drive the one global board run one at a time.
#[cfg(test)]
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The home board paints in the central panel (the background layer, no Area): the pointer
    /// on a block there hovers it; a window in front of it takes the hover away.
    #[test]
    fn a_block_on_the_home_board_is_hovered() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let block = Rect::from_min_size(pos2(100.0, 100.0), vec2(200.0, 100.0));
        let mut seen = Vec::new();
        for at in [pos2(150.0, 150.0), pos2(150.0, 150.0), pos2(500.0, 150.0), pos2(250.0, 180.0), pos2(250.0, 180.0)] {
            let input = egui::RawInput { screen_rect: Some(screen), events: vec![egui::Event::PointerMoved(at)], ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| seen.push(hovered(ui.painter(), block)));
                // A window over the block's right half.
                egui::Area::new(egui::Id::new("in_front")).fixed_pos(pos2(240.0, 160.0)).show(ctx, |ui| {
                    ui.allocate_exact_size(vec2(100.0, 60.0), egui::Sense::hover());
                });
            });
        }
        assert_eq!(seen[1], true, "on the block, nothing in front");
        assert_eq!(seen[2], false, "beside the block");
        assert_eq!(seen[4], false, "a window in front of the block");
    }

    /// A block in a window of its own grows into the spotlight on the monitor it stands on:
    /// in the middle of its work area, never larger than that monitor has room for.
    #[test]
    fn a_blocks_window_grows_in_the_middle_of_its_own_monitor() {
        // A monitor to the right of the first, the taskbar at its bottom.
        let [x, y, w, h] = spotlight_window([2560, 0, 5120, 1392], 1.0, [620, 150]);
        assert_eq!((w, h), (800, 538), "the spotlight, the room round it and its keys under it");
        assert_eq!((x + w / 2, y + h / 2), (3840, 696), "in the middle of that monitor");
        // A small monitor: it keeps inside.
        let [x, y, w, h] = spotlight_window([0, 0, 800, 600], 1.0, [300, 200]);
        assert_eq!((w, h), (736, 538));
        assert!(x >= 0 && x + w <= 800 && y >= 0 && y + h <= 600, "{x},{y} {w}x{h}");
        // A scaled screen: as many pixels more.
        assert_eq!(spotlight_window([0, 0, 3840, 2160], 1.5, [600, 300])[2], 1200);
        // A block that stands larger than the spotlight is not made smaller by it (seen live
        // 2026-11-14: "maximise the chart" shrank a 1691 by 848 window to 802 by 539).
        let [_, _, w, h] = spotlight_window([2560, 0, 5120, 1392], 1.0, [1691, 848]);
        assert_eq!((w, h), (1691, 848 + 38), "as large as it was, and its keys under it");
        let [_, y, _, h] = spotlight_window([2560, 0, 5120, 1392], 1.0, [1691, 1392]);
        assert!(y >= 0 && y + h <= 1392, "never taller than the monitor: {y} + {h}");
    }

    #[test]
    fn windows_animate_open_and_closed_over_several_frames() {
        let dt = 1.0 / 60.0;
        let mut a = UiAnimState { t: 0.0, target_open: true };
        let mut opening = Vec::new();
        while a.t < 1.0 && opening.len() < 120 {
            opening.push(a.step(dt));
        }
        assert!(opening.len() >= 10 && opening.len() <= 30, "opens in ~0.2-0.5 s: {} frames", opening.len());
        assert!(opening.windows(2).all(|w| w[1] >= w[0]) && opening[0] > 0.0 && opening[0] < 0.5);
        // Closing keeps it on screen (t in 0..1) for several frames before it may be removed.
        a.target_open = false;
        let mut frames = 0;
        while a.visible() {
            let t = a.step(dt);
            assert!(t < 1.0);
            frames += 1;
            assert!(frames < 120);
        }
        assert!(frames >= 10, "closing takes {frames} frames, not one");
        assert!(!a.moving() && a.t == 0.0);
        // Frame-rate independent: 144 Hz lands where 60 Hz does after the same time.
        let (mut s60, mut s144) = (UiAnimState { t: 0.0, target_open: true }, UiAnimState { t: 0.0, target_open: true });
        (0..6).for_each(|_| { s60.step(1.0 / 60.0); });
        (0..14).for_each(|_| { s144.step(14.4f32.recip() / 10.0); });
        assert!((s60.t - s144.t).abs() < 0.03, "{} vs {}", s60.t, s144.t);
        // The spring overshoots a touch, then settles on 1.
        assert!((1..10).map(|k| ease_out_back(k as f32 / 10.0)).any(|v| v > 1.0));
        assert!((ease_out_back(1.0) - 1.0).abs() < 1e-5 && ease_out_back(0.0).abs() < 1e-5);
    }

    #[test]
    fn a_minimised_spotlight_shrinks_back_instead_of_vanishing() {
        let _guard = test_lock();
        present(report());
        let ctx = egui::Context::default();
        crate::holo::install_fonts(&ctx);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let mut view = BoardView::default();
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
        let frame = |view: &mut BoardView| {
            let input = egui::RawInput { screen_rect: Some(area), predicted_dt: 1.0 / 60.0, ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.paint(ui, area, rt.handle());
                });
            });
        };
        frame(&mut view);
        assert!(maximize(Some("the chart")).is_ok());
        for _ in 0..40 {
            frame(&mut view);
        }
        assert_eq!(view.spot.t, 1.0, "the spotlight grew to full size");
        assert!(minimize());
        frame(&mut view);
        assert!(view.spot.visible() && view.spot.t < 1.0, "one frame after minimise it is still shrinking: {}", view.spot.t);
        for _ in 0..60 {
            frame(&mut view);
        }
        assert!(!view.spot.visible());
        sign_off();
    }

    /// A long note turned twenty rows still shows its top row whole: it turns by the pitch epaint
    /// really lays its rows out at (whole pixels), not by the font's unrounded row height (Segoe
    /// UI at 14.5: 19.29 against 19 px, its top row 5.7 px cut after twenty rows).
    #[test]
    fn a_long_note_turned_twenty_rows_shows_its_top_row_whole() {
        let ctx = egui::Context::default();
        crate::holo::install_fonts(&ctx);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let body = "word ".repeat(600);
        let block = Block::new("NOTE // 01", "FACT", "", BlockKind::NoteStack { notes: vec![Note { headline: "Battery".into(), body: body.clone() }] });
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let mut view = BoardView::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
            // Twenty rows as far apart as the body's rows really are.
            let pitch = ctx.fonts(|f| f.layout_no_wrap("a\nb".into(), FontId::proportional(14.5), Color32::WHITE)).rows[1].rect.min.y;
            view.scroll.insert((usize::MAX, 0), 20.0 * pitch);
            let p = Painter::new(ctx.clone(), egui::LayerId::background(), screen);
            view.paint_block(&p, Rect::from_min_size(pos2(40.0, 40.0), vec2(320.0, 300.0)), &block, (usize::MAX, 0), false, false, 1.0, rt.handle());
        });
        let (shown, text) = out
            .shapes
            .iter()
            .find_map(|c| match &c.shape {
                Shape::Text(t) if t.galley.text() == body => Some((c.clip_rect, t.clone())),
                _ => None,
            })
            .expect("the note's text is painted");
        let pitch = text.galley.rows[1].rect.min.y - text.galley.rows[0].rect.min.y;
        let turned = shown.top() - text.pos.y;
        assert_eq!((turned / pitch).round(), 20.0, "turned twenty rows: {turned} px, rows {pitch} px apart");
        assert!((turned - 20.0 * pitch).abs() < 0.01, "its top row is cut by {} px ({turned} px turned, rows {pitch} px apart)", turned - 20.0 * pitch);
    }

    fn note(h: &str) -> Note {
        Note { headline: h.into(), body: format!("{h} body") }
    }

    /// The reference video's board: no two blocks the same size. A price is a small tile, a few
    /// words a low note, a long text a whole cell, a photo its own shape.
    #[test]
    fn blocks_are_as_big_as_what_they_show() {
        let cell = vec2(300.0, 250.0);
        let price = Block::new("FIGURE // PRICE", "FACT", "", BlockKind::HeroFigure { header: "List price".into(), figure: "$199".into(), note: "USD at launch.".into() });
        let few = Block::new("NOTE // 01", "FACT", "", BlockKind::NoteStack { notes: vec![Note { headline: "Battery".into(), body: "Lasts two days.".into() }] });
        let much = Block::new("NOTE // 02", "FACT", "", BlockKind::NoteStack { notes: vec![Note { headline: "Buttons and sensors".into(), body: "word ".repeat(220) }] });
        let photo = Block::new("PHOTO // 01", "", "", BlockKind::MediaCard { images: vec!["https://example.org/a.jpg".into()], caption: "The device".into() });
        let video = Block::new("VIDEO // 01", "", "", BlockKind::MediaCard { images: vec!["https://www.youtube.com/watch?v=dQw4w9WgXcQ".into()], caption: "Hands-on".into() });
        let chart = Block::new("CHART", "", "", BlockKind::BarChart { unit: "%".into(), bars: vec![Bar { label: "a".into(), value: 1.0 }], caption: String::new() });
        let tile = natural_size(&price, cell, None);
        assert!(tile.x <= cell.x * 0.85 && tile.y <= cell.y * 0.8 && tile.x >= MIN_FIT[0] && tile.y >= MIN_FIT[1], "a small tile: {tile:?}");
        let low = natural_size(&few, cell, None);
        assert!(low.x < cell.x && low.y < cell.y * 0.8 && low.y > 110.0, "a low note: {low:?}");
        assert_eq!(natural_size(&much, cell, None), cell, "a long text takes its whole cell (and scrolls)");
        // A longer text is never given less room than a shorter one.
        let mut last = 0.0;
        for words in [3usize, 12, 30, 60, 120] {
            let note = Block::new("NOTE", "", "", BlockKind::NoteStack { notes: vec![Note { headline: "Displays".into(), body: "word ".repeat(words) }] });
            let h = natural_size(&note, vec2(300.0, 900.0), None).y;
            assert!(h >= last, "{words} words: {h} after {last}");
            last = h;
        }
        // A photo: the cell until it is here, then its own shape inside the cell.
        assert_eq!(natural_size(&photo, cell, None), cell);
        let upright = natural_size(&photo, cell, Some([600, 1200]));
        assert!(upright.y == cell.y && upright.x <= cell.x * 0.7, "{upright:?}");
        let wide = natural_size(&photo, cell, Some([1600, 900]));
        assert!(wide.x == cell.x && wide.y < cell.y, "{wide:?}");
        let playing = natural_size(&video, vec2(612.0, 250.0), None);
        assert!(playing.y == 250.0 && playing.x < 612.0, "16:9 inside two cells: {playing:?}");
        assert_eq!(natural_size(&chart, cell, None), cell, "a chart fills its room");
        // On the board: every block inside its own cell (so none overlaps another or leaves the
        // board), drawn towards the middle.
        let area = Rect::from_min_size(pos2(40.0, 60.0), vec2(1600.0, 860.0));
        let report = Report { topic: "busy bar".into(), created_at: String::new(), blocks: vec![chart, price, few, much, photo, video], sources: Vec::new(), id: None };
        let (rooms, rects) = (cells(&report, area, false), layout(&report, area, false));
        assert_eq!(rects.len(), 6);
        let mut smaller = 0;
        for (room, rect) in rooms.iter().zip(&rects) {
            let (room, rect) = (room.expect("a cell for every block"), rect.expect("a place for every block"));
            assert!(room.expand(0.5).contains_rect(rect) && area.expand(0.5).contains_rect(rect), "{rect:?} in {room:?}");
            if rect.width() < room.width() - 1.0 || rect.height() < room.height() - 1.0 {
                smaller += 1;
                // The side nearer the middle of the board is the one it keeps.
                let mid = area.center();
                if room.center().x < mid.x - room.width() * 0.25 && rect.width() < room.width() - 1.0 {
                    assert!((rect.right() - room.right()).abs() < 0.5, "left of the middle: drawn to its right edge ({rect:?} in {room:?})");
                }
                if room.center().x > mid.x + room.width() * 0.25 && rect.width() < room.width() - 1.0 {
                    assert!((rect.left() - room.left()).abs() < 0.5, "right of the middle: drawn to its left edge ({rect:?} in {room:?})");
                }
            }
        }
        assert!(smaller >= 2, "the tile and the short note are smaller than their cells");
        for (i, a) in rects.iter().flatten().enumerate() {
            for b in rects.iter().flatten().skip(i + 1) {
                assert!(!a.shrink(0.5).intersects(b.shrink(0.5)), "{a:?} over {b:?}");
            }
        }
    }

    fn report() -> Report {
        Report {
            topic: "state of VR".into(),
            created_at: "2026-11-02 10:00:00".into(),
            blocks: vec![
                Block::new("CHART // HEADSET SHARE ON PC", "SOURCE: STEAM", "SOURCE: STEAM HARDWARE SURVEY", BlockKind::BarChart {
                    unit: "%".into(),
                    bars: vec![Bar { label: "Quest 2".into(), value: 38.2 }, Bar { label: "Quest 3".into(), value: 24.1 }, Bar { label: "Index".into(), value: 11.0 }],
                    caption: "Share of VR headsets used on Steam.".into(),
                }),
                Block::new("NOTE // 01 — HARDWARE & PRICING", "FACT", "SOURCE: META", BlockKind::NoteStack { notes: vec![note("Quest 3S costs 299"), note("Index is discontinued")] }),
                Block::new("FIGURE // 2026", "FACT", "SOURCE: IDC", BlockKind::HeroFigure { header: "Headsets shipped".into(), figure: "≈20m".into(), note: "IDC estimate".into() }),
                Block::new("VIDEO // STEAM DECK HANDS-ON", "SOURCE: YOUTUBE", "", BlockKind::MediaCard { images: vec!["a".into(), "b".into()], caption: "Hands-on".into() }),
            ],
            sources: vec!["https://store.steampowered.com".into(); 42],
            id: None,
        }
    }

    #[test]
    fn bento_packs_first_fit_and_keeps_the_corner_free() {
        let cells = bento(&[(2, 2), (1, 1), (1, 1), (1, 1), (1, 1)], 4, 3, &[(3, 2)]);
        assert_eq!(cells[0], Some(Cell { col: 0, row: 0, w: 2, h: 2 }));
        assert_eq!(cells[1], Some(Cell { col: 2, row: 0, w: 1, h: 1 }));
        assert!(cells.iter().flatten().all(|c| !(c.col <= 3 && 3 < c.col + c.w && c.row <= 2 && 2 < c.row + c.h)), "corner reserved");
        // Too many blocks: the rest gets no slot instead of overlapping.
        let crowded = bento(&[(2, 2); 4], 3, 3, &[]);
        assert_eq!(crowded.iter().filter(|c| c.is_some()).count(), 1);
    }

    #[test]
    fn video_blocks_know_their_youtube_id_and_frames() {
        for url in ["https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10", "https://youtu.be/dQw4w9WgXcQ?si=x", "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"] {
            assert_eq!(youtube_id(url).as_deref(), Some("dQw4w9WgXcQ"), "{url}");
        }
        assert_eq!(youtube_id("https://example.com/photo.jpg"), None);
        assert_eq!(youtube_id("https://youtu.be/short"), None, "ids are 11 characters");
        let frames = video_frames("dQw4w9WgXcQ");
        assert_eq!(frames.len(), 4);
        assert!(frames[0].ends_with("/hqdefault.jpg") && frames[3].ends_with("/3.jpg"));
        let video = Block::new("VIDEO // HANDS-ON", "SOURCE: YOUTUBE", "", BlockKind::MediaCard { images: frames, caption: "Hands-on".into() });
        assert_eq!(video.cells(), (2, 1), "videos play wide");
        let photo = Block::new("PHOTO // HEADSET", "", "", BlockKind::MediaCard { images: vec!["https://example.com/a.jpg".into()], caption: String::new() });
        assert_eq!(photo.cells(), (1, 1));
    }

    #[test]
    fn research_block_starts_centred_then_glides_into_its_slot() {
        let area = Rect::from_min_size(pos2(24.0, 40.0), vec2(1800.0, 1000.0));
        let slot = research_rect(area);
        // Flying in from the left, then holding in the middle at 1.6×.
        assert!(research_rect_at(slot, area, 0.0).right() < area.left());
        let intro = research_rect_at(slot, area, 1.0);
        assert!((intro.center() - area.center()).length() < 1.0);
        assert!((intro.width() - slot.width() * 1.6).abs() < 0.5);
        assert!(crate::hud_dock::centre_covered(area, &[intro]), "while it sits there the emblem makes room");
        // In its slot once the agent is under way.
        assert_eq!(research_rect_at(slot, area, RESEARCH_INTRO_SECS + RESEARCH_GLIDE_SECS + 0.1), slot);
        // The bar: the share done and a sweeping run of bright cells while it works.
        let cells = research_cells(0.5, 0.3, true);
        assert_eq!(cells.len(), RESEARCH_CELLS);
        assert!(cells[..15].iter().all(|c| *c >= 0.55));
        assert!(cells[15..].iter().any(|c| *c > 0.8), "{cells:?}");
        assert!(research_cells(1.0, 0.3, false).iter().all(|c| *c >= 0.55));
        assert_eq!(ResearchStatus { topic: "VR".into(), phase: "x".into(), started: Instant::now(), blocks: 0, sources: 0, progress: 0.0 }.block(Instant::now()).label, "RESEARCH //");
    }

    /// Sir's evening of 2026-11-12: the briefing on the board, "close the briefing" (a program
    /// was looked for), "close the windows" ("nothing is open").
    #[test]
    fn what_the_board_calls_by_a_name_closes_by_that_name() {
        let _one = test_lock();
        sign_off();
        let stat = |label: &str, tag: &str| Block::new(label, tag, "SOURCE: WEBUNTIS", BlockKind::HeroFigure { header: "Lessons tomorrow".into(), figure: "3".into(), note: String::new() });
        let briefing = || Report { topic: "Tomorrow".into(), created_at: String::new(), blocks: vec![stat("BRIEFING // SCHOOL TOMORROW", "WEBUNTIS"), stat("WEBUNTIS // TIMETABLE", "FRI 13 NOV"), stat("CALENDAR // TOMORROW", "2 EVENTS")], sources: vec![], id: None };
        present(briefing());
        // "The briefing" names the block tagged so; "the timetable" that one; a program's name none.
        assert!(names_block("briefing") && names_block("timetable") && !names_block("spotify"));
        assert_eq!(close_matching("briefing"), ["BRIEFING // SCHOOL TOMORROW"]);
        assert_eq!(board().shown.as_ref().map(|s| s.report.blocks.len()), Some(2));
        assert_eq!(close_matching("the timetable"), ["WEBUNTIS // TIMETABLE"]);
        assert!(close_matching("spotify").is_empty());
        // "Close the windows": what is left goes, a pinned block stays.
        assert_eq!(close_unpinned(), 1);
        assert!(!is_active() && close_unpinned() == 0);
        present(briefing());
        assert_eq!(toggle_pin(1), Some(true));
        assert_eq!(close_unpinned(), 2);
        assert_eq!(board().shown.as_ref().map(|s| s.report.blocks.iter().map(|b| b.label.clone()).collect::<Vec<_>>()), Some(vec!["WEBUNTIS // TIMETABLE".to_string()]));
        // A report whose topic is the name goes whole.
        toggle_pin(0);
        present(Report { topic: "Evening briefing".into(), ..briefing() });
        assert_eq!(close_matching("briefing").len(), 3);
        assert!(!is_active());
        sign_off();
    }

    #[test]
    fn walkthrough_place_and_highlight_for_the_shown_board() {
        let _one = test_lock();
        sign_off();
        assert!(walkthrough().is_none() && current_place().is_none() && !highlight(0));
        let mut r = report();
        r.blocks.push(Block::new("MAP // TULLEYS FARM", "", "SOURCE: OSM", BlockKind::DarkVectorMap { place: "Tulleys Farm".into(), lat: 51.1, lon: -0.17, span: 0.02, roads: vec![], water: vec![], pending: false }));
        present(r);
        let steps = walkthrough().unwrap();
        assert_eq!(steps.len(), 5);
        assert!(steps.iter().all(|(_, line)| !line.is_empty()));
        assert_eq!(current_place(), Some(("Tulleys Farm".to_string(), 51.1, -0.17)));
        assert!(highlight(steps[0].0) && !highlight(99));
        assert!(board().shown.as_ref().unwrap().highlights[steps[0].0].is_some());
        sign_off();
    }

    #[test]
    fn the_spotlight_block_tells_the_suggestion_bar_what_it_is() {
        let report = Report {
            topic: "State of VR".into(),
            created_at: String::new(),
            blocks: vec![
                Block::new("NOTE // 01", "FACT", "META", BlockKind::NoteStack { notes: vec![Note { headline: "Hardware".into(), body: "Lighter.".into() }, Note { headline: "Pricing".into(), body: "Cheaper.".into() }] }),
                Block::new("MAP", "PLACE", "OSM", BlockKind::DarkVectorMap { place: "Europa-Park".into(), lat: 48.27, lon: 7.72, span: 0.01, roads: vec![], water: vec![], pending: false }),
            ],
            sources: vec!["Meta Reality Labs".into(), "https://example.org/vr".into()],
            id: None,
        };
        let note = subject_of(&report, 0, 1);
        assert_eq!((note.kind, note.headline.as_str(), note.page, note.pages, note.blocks), ("note", "Pricing", 1, 2, 2));
        assert_eq!(note.source.as_deref(), Some("https://example.org/vr"), "the first source that is a web page");
        let map = subject_of(&report, 1, 0);
        assert_eq!((map.kind, map.headline.as_str(), map.pages), ("map", "Europa-Park", 1));
        // The bar for them: a note on its last page, a map with its route.
        let fresh = crate::hud_suggestions::Habits::default();
        let keys = |s: &crate::hud_suggestions::Subject| crate::hud_suggestions::top(s, &fresh).into_iter().map(|k| k.label).collect::<Vec<_>>();
        assert_eq!(keys(&note), ["WALK ME THROUGH", "DIG DEEPER", "OPEN SOURCE"]);
        assert_eq!(keys(&map)[0], "ROUTE THERE");
        // What Sir says is what he would have clicked.
        assert_eq!(habit_label("flip_note", &json!({"direction": "next"})), Some("NEXT PAGE"));
        assert_eq!(habit_label("flip_note", &json!({"direction": "previous"})), Some("PREV PAGE"));
        assert_eq!(habit_label("flip_note", &json!({"page": 3})), None, "a jump to a page is no key of the bar");
        assert_eq!(habit_label("close", &json!({})), Some("CLOSE BOARD"));
        assert_eq!(habit_label("close", &json!({"target": "feature lab"})), None, "closing another block is not closing the board");
        assert_eq!(habit_label("walkthrough", &json!({})), Some("WALK ME THROUGH"));
        assert_eq!(habit_label("status", &json!({})), None);
    }

    #[test]
    fn block_footers_say_when_and_where() {
        assert_eq!(added_label(0), "ADDED JUST NOW");
        assert_eq!(added_label(300), "ADDED 5 MIN AGO");
        assert_eq!(added_label(7300), "ADDED 2 H AGO");
        let code = block_code("NOTE // 01 — HARDWARE");
        assert!(code.starts_with("0x") && code.len() == 6);
        assert_eq!(code, block_code("NOTE // 01 — HARDWARE"), "stable");
        assert_ne!(code, block_code("NOTE // 02 — SOFTWARE"));
    }

    #[test]
    fn layout_never_overlaps_and_reserves_the_emblem_corner() {
        let area = Rect::from_min_size(pos2(24.0, 30.0), vec2(1872.0, 1000.0));
        let rects: Vec<Rect> = layout(&report(), area, true).into_iter().flatten().collect();
        assert_eq!(rects.len(), 4);
        for (i, a) in rects.iter().enumerate() {
            assert!(area.expand(0.5).contains_rect(*a));
            for b in &rects[i + 1..] {
                assert!(!a.shrink(0.5).intersects(b.shrink(0.5)), "{a:?} overlaps {b:?}");
            }
            assert!(!a.contains(area.right_bottom() - vec2(75.0, 75.0)), "the docked emblem's corner stays free");
            assert!(!a.intersects(research_rect(area)), "the research block's corner stays free");
            assert!(!a.intersects(research_rect_for(Some(&report()), area)), "and its cell-sized block too");
        }
        let spot = spotlight_rect(area);
        assert_eq!(spot.size(), vec2(780.0, 480.0));
        assert_eq!(spot.center(), area.center());
    }

    #[test]
    fn spoken_lines_match_the_film_cadence() {
        assert_eq!(number_words(13), "thirteen");
        assert_eq!(number_words(42), "forty-two");
        assert_eq!(number_words(40), "forty");
        assert_eq!(
            presented_line(13, 42, true),
            "Screen's awake. Report's up — thirteen blocks on the HUD, notes stacked at the back, and forty-two sources behind it."
        );
        assert_eq!(
            finished_line("state of VR", 12, 42),
            "Sir, state of VR report's done. Twelve blocks, forty-two sources. Want me to put it on screen and walk you through it?"
        );
        assert_eq!(wake_line(&ScreenState::SignedOff), "I'm here, Sir. Screen's still down from the sign-off, so nothing's in front of you.");
    }

    #[test]
    fn blocks_are_found_by_kind_ordinal_and_header_words() {
        let r = report();
        assert_eq!(find_block(&r, "the chart"), Some(0));
        assert_eq!(find_block(&r, "photo"), Some(3));
        assert_eq!(find_block(&r, "the note"), Some(1));
        assert_eq!(find_block(&r, "headset share"), Some(0));
        assert_eq!(find_block(&r, "hardware pricing"), Some(1));
        assert_eq!(find_block(&r, "zebra"), None);
    }

    #[test]
    fn axis_ticks_round_to_the_scale() {
        assert_eq!(axis_ticks(38.2), vec![0.0, 10.0, 20.0, 30.0, 40.0]);
        assert_eq!(axis_ticks(3.0).last().copied(), Some(3.0));
    }

    #[test]
    fn topic_words_drop_filler_and_keep_short_acronyms() {
        assert_eq!(topic_words("that VR report you gave me earlier"), vec!["vr"]);
        assert_eq!(topic_words("state of VR"), vec!["state", "vr"]);
    }


    #[test]
    fn the_research_block_fits_its_cell_on_a_narrow_board() {
        let mut crowded = report();
        crowded.blocks.extend(report().blocks);
        let area = Rect::from_min_size(pos2(24.0, 42.0), vec2(1232.0, 568.0));
        let research = research_rect_for(Some(&crowded), area);
        for r in layout(&crowded, area, true).into_iter().flatten() {
            assert!(!r.intersects(research), "{r:?} overlaps the research block {research:?}");
        }
        assert!(research.width() <= RESEARCH_BLOCK_SIZE[0]);
    }

    #[test]
    fn every_block_kind_has_a_type_text_and_spoken_answer() {
        let mut blocks = report().blocks;
        blocks.push(Block::new("MAP // TULLEYS FARM, CRAWLEY", "", "SOURCE: OSM", BlockKind::DarkVectorMap { place: "Tulleys Farm, Crawley".into(), lat: 51.1, lon: -0.17, span: 0.02, roads: vec![], water: vec![], pending: false }));
        let status = ResearchStatus { topic: "state of VR".into(), phase: "Starting research agent".into(), started: Instant::now(), blocks: 12, sources: 42, progress: 0.4 };
        blocks.push(status.block(Instant::now()));
        blocks.push(dialogue_block("are you there?", "At your service, Sir.", false, "14:02:11").unwrap());
        let words: Vec<&str> = blocks.iter().map(|b| b.type_word()).collect();
        for kind in ["chart", "note", "figure", "photo", "map", "research", "comms"] {
            assert!(words.contains(&kind), "{kind} missing from {words:?}");
        }
        for b in &blocks {
            assert!(!b.text().is_empty() && !b.spoken_answer().trim().is_empty(), "{}", b.label);
        }
        let research = blocks.iter().find(|b| b.type_word() == "research").unwrap();
        assert!(research.text().contains("12 blocks, 42 sources"), "{}", research.text());
        assert!(blocks[0].spoken_answer().contains("Quest 2 at 38.2%"), "{}", blocks[0].spoken_answer());
    }

    #[test]
    fn dialogue_feed_shows_every_exchange_and_marks_streaming() {
        assert!(dialogue_block("", "  ", false, "12:00:00").is_none());
        let live = dialogue_block("what's on the board", "The chart shows", true, "12:00:00").unwrap();
        assert_eq!(live.label, "COMMS // LIVE RESPONSE");
        assert_eq!(live.tag, "LIVE");
        let done = dialogue_block("", "Done, Sir.", false, "12:00:05").unwrap();
        assert_eq!(done.tag, "12:00:05");
        assert!(matches!(done.kind, BlockKind::DialogueFeed { streaming: false, .. }));
    }

    #[test]
    fn spotlight_dims_the_rest_to_12_percent_and_sign_off_clears_the_board() {
        let _one = test_lock();
        assert!((DIM_OPACITY - 0.12).abs() < 1e-6);
        assert_eq!(SPOTLIGHT_SIZE, [780.0, 480.0]);
        present(report());
        assert!(matches!(screen_state(), ScreenState::BoardActive { block_count: 4, source_count: 42, .. }));
        assert_eq!(maximize(Some("the chart")).unwrap(), "CHART // HEADSET SHARE ON PC");
        assert!(spotlight_active());
        assert!(minimize());
        let epoch = sign_off();
        assert_eq!(screen_state(), ScreenState::SignedOff);
        assert!(still_signed_off(epoch), "the delayed VRAM flush still applies");
        // Waking ("are you there?") ends the sign-off: the flush is called off.
        assert_eq!(wake(), ScreenState::SignedOff);
        assert!(!still_signed_off(epoch));
        assert_eq!(wake_line(&ScreenState::SignedOff), "I'm here, Sir. Screen's still down from the sign-off, so nothing's in front of you.");
    }

    #[test]
    fn board_questions_are_answered_from_the_blocks_without_the_model() {
        let _one = test_lock();
        present(report());
        let answer = answer_from_board("how many headsets were shipped in 2026").expect("the figure block answers");
        assert!(answer.starts_with("From the board, Sir:") && answer.contains("≈20m"), "{answer}");
        // The block it used is highlighted.
        assert!(board().shown.as_ref().unwrap().highlights.iter().any(|h| h.is_some()));
        assert!(answer_from_board("what is the weather in Tokyo").is_none());
        assert_eq!(crate::brain::deterministic_fallback("what is the weather in Tokyo"), crate::brain::COLD_START_FALLBACK);
        sign_off();
        assert!(answer_from_board("how many headsets were shipped").is_none(), "no board, no board answer");
    }

    #[test]
    fn reports_round_trip_through_sqlite_and_recall_by_topic() {
        let dir = std::env::temp_dir().join(format!("jai_board_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.db").display().to_string();
        let mut vr = report();
        let id = save_report(&path, &vr).unwrap();
        let mut other = report();
        other.topic = "solid-state batteries".into();
        save_report(&path, &other).unwrap();
        let back = load_report(&path, Some("that VR report")).unwrap();
        vr.id = Some(id);
        assert_eq!(back, vr);
        assert_eq!(load_report(&path, None).unwrap().topic, "solid-state batteries");
        assert_eq!(list_reports(&path, 10).len(), 2);
        assert!(load_report(&path, Some("zebras")).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn block_text_carries_exact_figures_for_board_questions() {
        let r = report();
        let chart = r.blocks[0].text();
        assert!(chart.contains("Quest 2 38.2%") && chart.contains("Quest 3 24.1%"), "{chart}");
        assert!(r.blocks[2].text().contains("≈20m"));
        assert_eq!(r.note_count(), 2);
        assert!(r.stacked());
    }

    #[test]
    fn map_projection_centres_the_pin() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
        assert_eq!(map_point(r, 51.1, -0.2, 0.02, 51.1, -0.2), r.center());
        let north = map_point(r, 51.1, -0.2, 0.02, 51.11, -0.2);
        assert!(north.y < r.center().y);
        let east = map_point(r, 51.1, -0.2, 0.02, 51.1, -0.19);
        assert!(east.x > r.center().x);
    }

    #[test]
    fn bring_back_the_screen_restores_the_last_board() {
        let _one = test_lock();
        present(report());
        sign_off();
        assert_eq!(screen_state(), ScreenState::SignedOff);
        assert_eq!(visible_rects(Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0))).len(), 0, "nothing covers the screen once it is down");
        let (topic, blocks) = restore().expect("the signed-off board comes back");
        assert_eq!(blocks, 4);
        assert!(!topic.is_empty());
        assert!(matches!(screen_state(), ScreenState::BoardActive { block_count: 4, .. }));
        // Its blocks are what the emblem must not cover.
        let area = Rect::from_min_size(pos2(24.0, 40.0), vec2(1600.0, 900.0));
        let rects = visible_rects(area);
        assert!(!rects.is_empty() && rects.iter().all(|r| area.expand(1.0).contains_rect(*r)));
        sign_off();
    }

    #[test]
    fn the_walk_through_pulses_each_block_as_its_sentence_starts() {
        let script = vec![(0, "Quest 3 costs 499 dollars".to_string()), (2, "".to_string()), (3, "About 20 million headsets were sold.".to_string()), (5, "Steam shows the Quest 2 ahead".to_string())];
        let (line, cues) = walkthrough_schedule("Here's the walk-through, Sir.", &script, 7);
        assert!(line.starts_with("Here's the walk-through, Sir. Quest 3 costs 499 dollars. About 20 million"), "{line}");
        assert!(line.ends_with("Quest 2 ahead."));
        assert_eq!(cues.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 3, 5], "empty sentences are skipped");
        assert!(cues.windows(2).all(|w| w[1].1 > w[0].1), "in speaking order");
        assert!(cues[0].1 > SPEECH_LEAD_SECS, "after the intro");
        let (_, capped) = walkthrough_schedule("Intro.", &script, 1);
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn photo_cards_are_turned_a_little_in_3d() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        let q = tilted_quad(r, 1.0, PHOTO_TILT);
        // Right edge away: shorter and a little in; the left edge stays the block's.
        assert_eq!((q[0], q[3]), (r.left_top(), r.left_bottom()));
        assert!((q[2].y - q[1].y) < r.height() && (q[2].y - q[1].y) > r.height() * 0.85);
        assert!(q[1].x < r.right() && q[1].x > r.right() - 20.0);
        let mirrored = tilted_quad(r, -1.0, PHOTO_TILT);
        assert_eq!((mirrored[1], mirrored[2]), (r.right_top(), r.right_bottom()));
        // Every corner stays inside the block.
        assert!(q.iter().chain(&mirrored).all(|c| r.expand(0.01).contains(*c)));
        // Cropped to fill: a wide image keeps its full height and loses the sides.
        let uv = cover_uv([400, 100], r);
        assert!((uv.height() - 1.0).abs() < 1e-6 && (uv.width() - 0.5).abs() < 1e-6);
    }
}
