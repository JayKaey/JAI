//! The HUD's views and where its windows live.
//!
//! Every tool view (Simulation Lab and function plotter, system telemetry, schedule, the phone
//! QR link, Jarvis' markdown windows) opens **inside** the main fullscreen board as a ROSE block
//! by default. A view becomes its own OS window only when Sir asks — the block's
//! `[ DETACH ]` button, "open the telemetry in a separate window", "detach the plotter" — and
//! "dock all windows back inside Jarvis" brings every one of them home.
//!
//! This module is the shared state behind that (which ids are detached, which views should open
//! detached), the top navigation dock's `HudViewMode`, and the emblem's spatial rule: it only
//! leaves the centre of the screen when something actually covers the 360 × 360 px centre safe
//! zone, and when it docks while the text console is out, it docks left of the console.
//! The state is plain data behind a mutex; the geometry is pure and unit-tested.

use crate::crash_guard::lock;
use eframe::egui::{pos2, vec2, Pos2, Rect};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Side of the square in the middle of the screen the emblem keeps for itself (px).
pub const CENTRE_SAFE_SIZE: f32 = 360.0;
/// The docked emblem's centre sits this far from the right edge (or the console's edge) and the
/// bottom.
pub const DOCK_INSET: f32 = 75.0;

/// The views of the top navigation dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HudViewMode {
    /// The emblem alone: every view closed, the board down.
    Home,
    /// The HUD board with its report blocks (the last one restored).
    Board,
    /// The interactive 3D core sphere.
    CoreArchitectureSphere,
    /// The Simulation Lab / function plotter.
    SimLab,
    /// System telemetry and VRAM.
    Telemetry,
    /// WebUntis timetable and the calendar.
    Schedule,
    /// The slide-out text console.
    TextConsole,
}

impl HudViewMode {
    /// The dock tab's text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "HOME",
            Self::Board => "HUD BOARD",
            Self::CoreArchitectureSphere => "3D CORE",
            Self::SimLab => "SIM LAB",
            Self::Telemetry => "TELEMETRY",
            Self::Schedule => "SCHEDULE",
            Self::TextConsole => "TEXT CONSOLE",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Board => "board",
            Self::CoreArchitectureSphere => "core",
            Self::SimLab => "sim_lab",
            Self::Telemetry => "telemetry",
            Self::Schedule => "schedule",
            Self::TextConsole => "console",
        }
    }

    /// A tab as Sir says it ("home", "the HUD board", "3D core", "sim lab", "telemetry").
    pub fn parse(name: &str) -> Option<Self> {
        let n = name.trim().to_lowercase().replace(['_', '-'], " ");
        let n = n.trim_start_matches("the ").trim_start_matches("my ").trim_start_matches("your ").trim();
        let n = n.trim_end_matches(" view").trim_end_matches(" tab").trim();
        Some(match n {
            "home" | "emblem" | "home emblem" | "home screen" => Self::Home,
            "board" | "hud board" | "hud" | "blocks" => Self::Board,
            "core" | "3d core" | "core sphere" | "core architecture" | "kern" => Self::CoreArchitectureSphere,
            "sim lab" | "simlab" | "simulation lab" | "lab" | "plotter" | "function plotter" => Self::SimLab,
            "telemetry" | "stats" | "system stats" | "system telemetry" => Self::Telemetry,
            "schedule" | "calendar" | "timetable" | "stundenplan" => Self::Schedule,
            "console" | "text console" | "text" => Self::TextConsole,
            _ => return None,
        })
    }
}

#[derive(Default)]
struct DockState {
    /// Window ids shown as their own OS viewport.
    detached: HashSet<String>,
    /// Id prefixes of views that should open detached when they next appear.
    pending: Vec<String>,
    /// Ids already seen (a pending prefix only applies to a window that is new).
    seen: HashSet<String>,
    /// The in-app blocks drawn this frame: (id, title, rect).
    blocks: Vec<(String, String, Rect)>,
    /// The blocks of the previous frame (what the emblem's centre check reads).
    last_blocks: Vec<(String, String, Rect)>,
    /// Where each block was last drawn (a detached window opens right there).
    last_rect: HashMap<String, Rect>,
    /// Navigation requests from voice or tools (the HUD applies them next frame).
    views: Vec<HudViewMode>,
    /// Blocks Sir asked to close by voice, and when: each fades out over `CLOSE_FADE`, then
    /// closes through its own `[ CLOSE ]` path.
    closing: HashMap<String, Instant>,
    /// `Pin Block`: these stay through "close the windows" and keep their place when the
    /// others are tiled.
    pinned: HashSet<String>,
    /// Where the dock is moving a block (a new block's slot, a tiled grid cell, the maximised
    /// rect), and for how many more frames the block is held there.
    forced: HashMap<String, (Rect, u8)>,
    /// The maximised block and the rect it goes back to.
    maximized: Option<(String, Rect)>,
    /// What each block says (its Markdown, its figures): `Copy Content`, the exports and the
    /// search read it.
    content: HashMap<String, String>,
    /// The search across the open blocks (lower case; empty: every block is lit).
    search: String,
    /// Blocks that have been drawn at least once (the materialise cue sounds on the first).
    drawn: HashSet<String>,
    /// The block Sir pressed the mouse on: where it stood then, and where the pointer was.
    grab: Option<(String, Rect, Pos2)>,
}

/// A block closed by voice fades out over this long before it leaves the board.
pub const CLOSE_FADE: Duration = Duration::from_millis(180);

static STATE: Mutex<Option<DockState>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut DockState) -> R) -> R {
    let mut g = lock(&STATE);
    f(g.get_or_insert_with(DockState::default))
}

/// Whether window `id` is its own OS window.
pub fn is_detached(id: &str) -> bool {
    with(|s| s.detached.contains(id))
}

/// Window `id` opens as its own OS window from now on.
pub fn detach(id: &str) {
    with(|s| s.detached.insert(id.to_string()));
    crate::gui::refresh();
}

/// Window `id` comes back inside the HUD.
pub fn dock(id: &str) {
    with(|s| s.detached.remove(id));
    crate::gui::refresh();
}

/// Every detached window comes back inside the HUD; returns how many.
pub fn dock_all() -> usize {
    let n = with(|s| {
        s.pending.clear();
        std::mem::take(&mut s.detached).len()
    });
    crate::gui::refresh();
    n
}

/// The next window whose id starts with `prefix` opens as its own OS window.
pub fn detach_next(prefix: &str) {
    with(|s| {
        if !s.pending.iter().any(|p| p == prefix) {
            s.pending.push(prefix.to_string());
        }
    });
}

/// Called for every window each frame: a window seen for the first time takes a pending detach
/// request for its kind. Returns whether it is detached.
pub fn adopt(id: &str) -> bool {
    with(|s| {
        if s.seen.insert(id.to_string()) {
            if let Some(k) = s.pending.iter().position(|p| id.starts_with(p.as_str())) {
                s.pending.remove(k);
                s.detached.insert(id.to_string());
            }
        }
        s.detached.contains(id)
    })
}

/// A window that closed: forgotten, so opening it again starts inside the HUD.
pub fn forget(id: &str) {
    with(|s| {
        s.seen.remove(id);
        s.detached.remove(id);
        s.last_rect.remove(id);
        s.closing.remove(id);
        s.pinned.remove(id);
        s.forced.remove(id);
        s.content.remove(id);
        s.drawn.remove(id);
        if s.maximized.as_ref().is_some_and(|(open, _)| open == id) {
            s.maximized = None;
        }
    });
}

// ----------------------------------------------------------- pin, content, search, maximise ----

/// `Pin Block`: flips the pin of block `id`; true when it is pinned now.
pub fn toggle_pin(id: &str) -> bool {
    with(|s| {
        if s.pinned.remove(id) {
            false
        } else {
            s.pinned.insert(id.to_string())
        }
    })
}

pub fn is_pinned(id: &str) -> bool {
    with(|s| s.pinned.contains(id))
}

/// What block `id` says, for `Copy Content`, the exports and the search.
pub fn describe_block(id: &str, text: &str) {
    with(|s| {
        if s.content.get(id).map(String::as_str) != Some(text) {
            s.content.insert(id.to_string(), text.to_string());
        }
    });
}

pub fn block_content(id: &str) -> Option<String> {
    with(|s| s.content.get(id).cloned())
}

/// The search across the open blocks ("find voltage on the board"); empty clears it.
pub fn set_search(text: &str) {
    with(|s| s.search = text.trim().to_lowercase());
    crate::gui::refresh();
}

pub fn search() -> String {
    with(|s| s.search.clone())
}

/// Whether a block with this title and content answers the search (every word of it, in any
/// order; no search: every block does).
pub fn search_matches(needle: &str, title: &str, content: &str) -> bool {
    let hay = format!("{} {}", title.to_lowercase(), content.to_lowercase());
    needle.split_whitespace().all(|word| hay.contains(word))
}

/// The open blocks the search lights up, as `(id, title)`.
pub fn search_hits() -> Vec<(String, String)> {
    with(|s| {
        if s.search.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(String, String)> = Vec::new();
        for (id, title, _) in s.last_blocks.iter().chain(s.blocks.iter()) {
            if search_matches(&s.search, title, s.content.get(id).map_or("", String::as_str)) && !hits.iter().any(|(known, _)| known == id) {
                hits.push((id.clone(), title.clone()));
            }
        }
        hits
    })
}

/// `Maximize`: block `id` fills `bounds`; a second call (or maximising another block) puts it
/// back where it was. True when it is maximised now.
pub fn toggle_maximize(id: &str, bounds: Rect) -> bool {
    with(|s| {
        let previous = s.maximized.take();
        if let Some((open, home)) = &previous {
            s.forced.insert(open.clone(), (*home, FORCED_FRAMES));
            if open == id {
                return false;
            }
        }
        let home = s.last_rect.get(id).copied().unwrap_or_else(|| Rect::from_center_size(bounds.center(), bounds.size() * 0.5));
        s.forced.insert(id.to_string(), (bounds.shrink(8.0), FORCED_FRAMES));
        s.maximized = Some((id.to_string(), home));
        true
    })
}

pub fn is_maximized(id: &str) -> bool {
    with(|s| s.maximized.as_ref().is_some_and(|(open, _)| open == id))
}

/// Where the dock wants block `id` this frame (a fresh slot, a grid cell, the maximised rect);
/// asked once per frame by the block, which is held there for a few frames and then free again.
pub fn take_forced(id: &str) -> Option<Rect> {
    with(|s| {
        let (rect, frames) = s.forced.get_mut(id)?;
        let rect = *rect;
        *frames = frames.saturating_sub(1);
        if *frames == 0 {
            s.forced.remove(id);
        }
        Some(rect)
    })
}

// ------------------------------------------------------------------------ closing by voice ----

/// A spoken block name without its small words ("the feature lab window" → "feature lab").
fn spoken_block_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter(|w| !matches!(*w, "the" | "my" | "your" | "that" | "this" | "a" | "an" | "to" | "window" | "windows" | "block" | "blocks" | "panel" | "card" | "view" | "tab" | "das" | "die" | "den" | "dein" | "deine" | "fenster"))
        .collect();
    words.join(" ")
}

/// The open in-app blocks a spoken name means, as `(id, title)`: "feature lab", "the telemetry
/// window", "plotter", "QR code". A title that carries the name wins; a view's id prefix
/// (`id_prefix_for`) is only tried when no title does, so "feature lab" never closes a
/// simulation because of the word "lab".
pub fn blocks_named(name: &str) -> Vec<(String, String)> {
    let n = spoken_block_name(name);
    if n.len() < 2 {
        return Vec::new();
    }
    with(|s| {
        let open: Vec<&(String, String, Rect)> = s.last_blocks.iter().chain(s.blocks.iter()).collect();
        let pick = |test: &dyn Fn(&str, &str) -> bool| {
            let mut out: Vec<(String, String)> = Vec::new();
            for (id, title, _) in open.iter().copied() {
                if test(id, &title.to_lowercase()) && !out.iter().any(|(i, _)| i == id) {
                    out.push((id.clone(), title.clone()));
                }
            }
            out
        };
        let by_title = pick(&|id, title| title.contains(n.as_str()) || id == n || id.replace('_', " ") == n);
        if !by_title.is_empty() {
            return by_title;
        }
        match id_prefix_for(&n) {
            Some(prefix) => pick(&|id, _| id.starts_with(prefix)),
            None => Vec::new(),
        }
    })
}

/// What the open in-app blocks are called ("Feature Lab // every system" is the Feature Lab).
pub fn open_titles() -> Vec<String> {
    with(|s| {
        let mut titles: Vec<String> = Vec::new();
        for (_, title, _) in s.last_blocks.iter().chain(s.blocks.iter()) {
            let title = title.split(" // ").next().unwrap_or(title).trim().to_string();
            if !title.is_empty() && !titles.contains(&title) {
                titles.push(title);
            }
        }
        titles
    })
}

/// "Close the feature lab": every open block the name means starts fading out; returns their
/// titles (empty: no such block is open).
pub fn close_named(name: &str) -> Vec<String> {
    // The text console is a drawer with its own close path (`gui::set_text_console`).
    let found: Vec<(String, String)> = blocks_named(name).into_iter().filter(|(id, _)| id != "console").collect();
    if found.is_empty() {
        return Vec::new();
    }
    let now = Instant::now();
    with(|s| {
        for (id, _) in &found {
            s.closing.entry(id.clone()).or_insert(now);
        }
    });
    crate::gui::refresh();
    found.into_iter().map(|(_, title)| title).collect()
}

/// "Close the windows": every open in-app block fades out (the text console keeps its own
/// drawer); returns how many.
pub fn close_all_blocks() -> usize {
    let now = Instant::now();
    let n = with(|s| {
        let ids: HashSet<String> = s.last_blocks.iter().chain(s.blocks.iter()).map(|(id, _, _)| id.clone()).filter(|id| id != "console" && !s.pinned.contains(id)).collect();
        for id in &ids {
            s.closing.entry(id.clone()).or_insert(now);
        }
        ids.len()
    });
    if n > 0 {
        crate::gui::refresh();
    }
    n
}

/// A close request nobody collected for this long is dropped (its block went another way), so a
/// block opened later under the same id never closes on its first frame.
const CLOSE_EXPIRY: Duration = Duration::from_secs(3);

/// How far block `id`'s voice-close fade is (0 … 1); None when nobody asked it to close. From 1
/// on the block must close: the request is consumed by that call.
pub fn closing_progress(id: &str) -> Option<f32> {
    with(|s| {
        let age = s.closing.get(id)?.elapsed();
        if age >= CLOSE_FADE {
            s.closing.remove(id);
        }
        if age > CLOSE_EXPIRY {
            return None;
        }
        Some((age.as_secs_f32() / CLOSE_FADE.as_secs_f32()).clamp(0.0, 1.0))
    })
}

/// The window-id prefix a spoken view name means ("telemetry" → `system_telemetry`).
pub fn id_prefix_for(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    // (The Earth's "satellite view" before the phone's satellite link.)
    Some(if has(&["earth", "globe", "world map", "erde", "globus", "satellite view", "satellite map"]) {
        "earth"
    } else if has(&["radar", "network", "netzwerk"]) {
        "network_radar"
    } else if has(&["disk map", "drive map", "storage map", "disk", "drive"]) {
        "disk_map"
    } else if has(&["telemetry", "stats", "system", "vram", "cpu", "gpu"]) {
        "system_telemetry"
    } else if has(&["schedule", "calendar", "kalender", "webuntis", "timetable", "stundenplan"]) {
        "schedule"
    } else if has(&["sim", "plotter", "plot", "lab", "graph of", "function", "pendulum", "projectile"]) {
        "sim:"
    } else if has(&["qr", "pairing", "satellite", "phone"]) {
        "pairing"
    } else if has(&["contact", "kontakt"]) {
        "contacts"
    } else if has(&["finance", "gym", "workout"]) {
        "workout_finance"
    } else {
        return None;
    })
}

/// "Detach the telemetry window": the open window(s) the name means become OS windows; a view
/// that is not open yet opens detached next time. Returns what was detached.
pub fn detach_named(name: &str) -> Result<String, String> {
    let n = name.to_lowercase();
    let prefix = id_prefix_for(&n);
    let open: Vec<(String, String)> = with(|s| {
        s.last_blocks
            .iter()
            .filter(|(id, title, _)| prefix.is_some_and(|p| id.starts_with(p)) || (!n.trim().is_empty() && title.to_lowercase().contains(n.trim())))
            .map(|(id, title, _)| (id.clone(), title.clone()))
            .collect()
    });
    if let Some((id, title)) = open.first() {
        detach(id);
        return Ok(title.clone());
    }
    match prefix {
        Some(p) => {
            detach_next(p);
            Ok(format!("the next {} window", p.trim_end_matches(':').replace('_', " ")))
        }
        None => Err(format!("I have no window called '{name}' open, Sir.")),
    }
}

/// Clears this frame's block list (the previous one stays readable for the emblem).
pub fn begin_frame() {
    with(|s| s.last_blocks = std::mem::take(&mut s.blocks));
}

/// An in-app block was drawn at `rect` this frame. True the first time this block is drawn
/// (it has just materialised).
pub fn publish_block(id: &str, title: &str, rect: Rect) -> bool {
    with(|s| {
        s.blocks.push((id.to_string(), title.to_string(), rect));
        s.last_rect.insert(id.to_string(), rect);
        s.drawn.insert(id.to_string())
    })
}

/// Where block `id` was last drawn inside the HUD.
pub fn block_rect(id: &str) -> Option<Rect> {
    with(|s| s.last_rect.get(id).copied())
}

/// The in-app blocks of the last complete frame.
pub fn block_rects() -> Vec<Rect> {
    with(|s| s.last_blocks.iter().chain(s.blocks.iter()).map(|(_, _, r)| *r).collect())
}

/// Asks the HUD to switch to a view (from voice or a tool).
pub fn request_view(mode: HudViewMode) {
    with(|s| s.views.push(mode));
    crate::gui::refresh();
}

pub fn take_view_requests() -> Vec<HudViewMode> {
    with(|s| std::mem::take(&mut s.views))
}

// ------------------------------------------------------------------------------ geometry ----

/// The square the emblem keeps in the middle of the screen.
pub fn centre_safe_zone(screen: Rect) -> Rect {
    Rect::from_center_size(screen.center(), vec2(CENTRE_SAFE_SIZE, CENTRE_SAFE_SIZE))
}

/// Whether any of `rects` reaches into the centre safe zone (only then the emblem docks).
pub fn centre_covered(screen: Rect, rects: &[Rect]) -> bool {
    let zone = centre_safe_zone(screen);
    rects.iter().any(|r| r.is_positive() && r.intersects(zone))
}

/// The docked emblem's centre: bottom-right, left of the text console while it is out
/// (`console_width` = how far it has slid in), so the console never covers it.
pub fn docked_centre(screen: Rect, console_width: f32) -> Pos2 {
    pos2(screen.right() - console_width.max(0.0) - DOCK_INSET, screen.bottom() - DOCK_INSET)
}

// ---------------------------------------------------------------- non-overlapping placement ----

/// The orb's rect and the board's own blocks (last frame): what a new window keeps off.
static OBSTACLES: Mutex<(Option<Rect>, Vec<Rect>)> = Mutex::new((None, Vec::new()));
/// "Overlap the windows" / "stack them": new windows cascade instead of packing.
static OVERLAP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The emblem's rect and the board's visible blocks this frame.
pub fn publish_obstacles(orb: Option<Rect>, board: Vec<Rect>) {
    *lock(&OBSTACLES) = (orb, board);
}

/// Sir told the emblem to get out of the way (it waits in its corner until he calls it back).
static ORB_ASIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_orb_aside(on: bool) {
    ORB_ASIDE.store(on, std::sync::atomic::Ordering::Release);
}

/// Whether the emblem is out of the way right now ("come back" means it then).
pub fn orb_aside() -> bool {
    ORB_ASIDE.load(std::sync::atomic::Ordering::Acquire)
}

pub fn set_overlap_allowed(on: bool) {
    OVERLAP.store(on, std::sync::atomic::Ordering::Release);
}

pub fn overlap_allowed() -> bool {
    OVERLAP.load(std::sync::atomic::Ordering::Acquire)
}

/// The top-most, then left-most spot in `bounds` where a `size` window touches none of `taken`
/// (kept `gap` apart): bottom-left packing over the candidate corners every placed rect opens
/// (its right edge and its bottom edge). None when nothing fits.
pub fn find_non_overlapping_slot(bounds: Rect, size: eframe::egui::Vec2, taken: &[Rect], gap: f32) -> Option<Pos2> {
    let mut xs = vec![bounds.left()];
    let mut ys = vec![bounds.top()];
    for r in taken {
        xs.push(r.right() + gap);
        ys.push(r.bottom() + gap);
    }
    let mut best: Option<Pos2> = None;
    for &y in &ys {
        for &x in &xs {
            let candidate = Rect::from_min_size(pos2(x, y), size);
            let inside = candidate.left() >= bounds.left() - 0.5 && candidate.top() >= bounds.top() - 0.5 && candidate.right() <= bounds.right() + 0.5 && candidate.bottom() <= bounds.bottom() + 0.5;
            if inside && !taken.iter().any(|r| r.expand(gap * 0.5 - 0.01).intersects(candidate.expand(gap * 0.5 - 0.01))) && best.is_none_or(|b| (y, x) < (b.y, b.x)) {
                best = Some(pos2(x, y));
            }
        }
    }
    best
}

/// Where a new in-app window of `size` opens in `bounds`: a free spot off every open window, the
/// board's blocks and the orb (else off the windows only, else the cascade `fallback`); with
/// "overlap the windows" on, always the cascade. `regions`: each monitor's part of `bounds`
/// when the HUD spans several screens, so no window straddles a bezel.
pub fn place_new_window(bounds: Rect, regions: &[Rect], size: eframe::egui::Vec2, own: Option<Rect>, fallback: Pos2) -> Pos2 {
    if overlap_allowed() {
        return fallback;
    }
    let (orb, board) = lock(&OBSTACLES).clone();
    let windows: Vec<Rect> = block_rects().into_iter().filter(|r| Some(*r) != own && r.is_positive()).chain(board).collect();
    let areas: Vec<Rect> = if regions.is_empty() { vec![bounds] } else { regions.iter().map(|r| r.intersect(bounds)).filter(|r| r.is_positive()).collect() };
    let with_orb: Vec<Rect> = windows.iter().copied().chain(orb).collect();
    for taken in [&with_orb, &windows] {
        if let Some(at) = areas.iter().find_map(|a| find_non_overlapping_slot(*a, size, taken, 12.0)) {
            return at;
        }
    }
    fallback
}

/// How many frames a block is held at the rect the dock gave it (egui needs two to settle a
/// window's size; after that Sir drags and resizes it freely).
const FORCED_FRAMES: u8 = 4;
/// The smallest a block is ever made, and the gap between tiled blocks.
pub const MIN_BLOCK: [f32; 2] = [300.0, 200.0];
pub const TILE_GAP: f32 = 12.0;
/// The largest a tiled cell is used (a lone block does not fill a 2560 px screen).
const MAX_TILE: [f32; 2] = [900.0, 620.0];

/// `count` cells of one size in `bounds`, none touching `keep_out` (the orb), none smaller than
/// `MIN_BLOCK`: the grid with the largest cells, row by row from the top left. None when even
/// the smallest cells do not fit.
pub fn tile_cells(bounds: Rect, count: usize, keep_out: Option<Rect>, gap: f32) -> Option<Vec<Rect>> {
    if count == 0 {
        return Some(Vec::new());
    }
    if !bounds.is_positive() {
        return None;
    }
    let mut best: Option<(f32, Vec<Rect>)> = None;
    for cols in 1..=count + 6 {
        for rows in 1..=count + 6 {
            if cols * rows < count {
                continue;
            }
            let w = (bounds.width() - gap * (cols - 1) as f32) / cols as f32;
            let h = (bounds.height() - gap * (rows - 1) as f32) / rows as f32;
            if w < MIN_BLOCK[0] || h < MIN_BLOCK[1] {
                break;
            }
            let size = vec2(w.min(MAX_TILE[0]), h.min(MAX_TILE[1]));
            let cells: Vec<Rect> = (0..rows)
                .flat_map(|row| (0..cols).map(move |col| (col, row)))
                .map(|(col, row)| Rect::from_min_size(pos2(bounds.left() + col as f32 * (w + gap), bounds.top() + row as f32 * (h + gap)), size))
                .filter(|cell| keep_out.is_none_or(|orb| !orb.expand(gap * 0.5).intersects(*cell)))
                .take(count)
                .collect();
            if cells.len() < count {
                continue;
            }
            // Large cells win; among equals, the shape closest to a 3:2 block.
            let score = size.x * size.y * (1.0 - ((size.x / size.y) - 1.5).abs().min(1.0) * 0.25);
            if best.as_ref().is_none_or(|(known, _)| score > *known) {
                best = Some((score, cells));
            }
        }
    }
    best.map(|(_, cells)| cells)
}

/// Where a new in-app block opens and how large: at the size it asks for in a free spot off
/// every open block, the board and the orb; else smaller; else every open block moves into one
/// grid around the orb (pinned blocks keep their place), so nothing ever lands on anything.
/// With "overlap the windows" on, the cascade `fallback`.
pub fn place_new_block(id: &str, bounds: Rect, regions: &[Rect], size: eframe::egui::Vec2, fallback: Pos2) -> (Pos2, eframe::egui::Vec2) {
    if overlap_allowed() {
        return (fallback, size);
    }
    let (orb, board) = lock(&OBSTACLES).clone();
    // Every open block where it is going to be (a block the dock is still moving counts at
    // its destination).
    let open: Vec<(String, Rect)> = with(|s| {
        let mut known: Vec<(String, Rect)> = Vec::new();
        for (other, _, rect) in s.last_blocks.iter().chain(s.blocks.iter()) {
            if other != id && !known.iter().any(|(k, _)| k == other) {
                known.push((other.clone(), s.forced.get(other).map_or(*rect, |(to, _)| *to)));
            }
        }
        for (other, (to, _)) in &s.forced {
            if other != id && !known.iter().any(|(k, _)| k == other) {
                known.push((other.clone(), *to));
            }
        }
        known
    });
    let windows: Vec<Rect> = open.iter().map(|(_, r)| *r).filter(|r| r.is_positive()).collect();
    let areas: Vec<Rect> = if regions.is_empty() { vec![bounds] } else { regions.iter().map(|r| r.intersect(bounds)).filter(|r| r.is_positive()).collect() };
    let with_board: Vec<Rect> = windows.iter().copied().chain(board).chain(orb).collect();
    let with_orb: Vec<Rect> = windows.iter().copied().chain(orb).collect();
    let hold = |at: Pos2, size: eframe::egui::Vec2| {
        with(|s| s.forced.insert(id.to_string(), (Rect::from_min_size(at, size), FORCED_FRAMES)));
        (at, size)
    };
    let mut tried: Option<eframe::egui::Vec2> = None;
    for shrink in [1.0f32, 0.8, 0.64, 0.5] {
        let fitted = vec2((size.x * shrink).max(MIN_BLOCK[0] + 20.0), (size.y * shrink).max(MIN_BLOCK[1] + 20.0));
        if tried.replace(fitted) == Some(fitted) {
            continue;
        }
        // The board's own blocks give way last: a window may open over a full report, never
        // over another window or the orb.
        for taken in [&with_board, &with_orb] {
            if let Some(at) = areas.iter().find_map(|a| find_non_overlapping_slot(*a, fitted, taken, TILE_GAP)) {
                return hold(at, fitted);
            }
        }
    }
    // Nothing fits beside what is open: one grid for all of them (pinned blocks stay put and
    // the grid goes around them only if it still can).
    let pinned: Vec<Rect> = with(|s| open.iter().filter(|(other, _)| s.pinned.contains(other)).map(|(_, r)| *r).collect());
    let movable: Vec<&(String, Rect)> = with(|s| open.iter().filter(|(other, _)| !s.pinned.contains(other)).collect());
    let keep_out = pinned.iter().copied().chain(orb).reduce(|a, b| a.union(b));
    let area = areas.iter().copied().max_by(|a, b| (a.width() * a.height()).total_cmp(&(b.width() * b.height()))).unwrap_or(bounds);
    let cells = tile_cells(area, movable.len() + 1, keep_out, TILE_GAP).or_else(|| tile_cells(area, movable.len() + 1, orb, TILE_GAP));
    match cells {
        Some(cells) => {
            with(|s| {
                for ((other, _), cell) in movable.iter().zip(&cells) {
                    s.forced.insert(other.clone(), (*cell, FORCED_FRAMES));
                }
            });
            let cell = cells[movable.len()];
            hold(cell.min, cell.size())
        }
        None => (fallback, size),
    }
}

/// Sir pressed the mouse on block `id` while it stood at `rect`, the pointer at `origin`.
pub fn grab(id: &str, rect: Rect, origin: Pos2) {
    with(|s| s.grab = Some((id.to_string(), rect, origin)));
}

/// Where block `id` stood and where the pointer was when Sir pressed the mouse on it.
pub fn grabbed(id: &str) -> Option<(Rect, Pos2)> {
    with(|s| s.grab.as_ref().filter(|(held, _, _)| held == id).map(|(_, rect, origin)| (*rect, *origin)))
}

/// The mouse button is up again: block `id` is no longer held.
pub fn let_go(id: &str) {
    with(|s| {
        if s.grab.as_ref().is_some_and(|(held, _, _)| held == id) {
            s.grab = None;
        }
    });
}

/// Holds block `id` at `rect` for the next frame only: a block that is being dragged is put
/// exactly under the pointer, frame by frame.
pub fn hold_once(id: &str, rect: Rect) {
    with(|s| {
        s.forced.insert(id.to_string(), (rect, 1));
    });
}

/// Whether block `id` was on screen in the last frame (a block that reopens is placed afresh
/// when its old spot has been taken).
pub fn is_open(id: &str) -> bool {
    with(|s| s.last_blocks.iter().chain(s.blocks.iter()).any(|(other, _, _)| other == id))
}

/// Whether `rect` would land on another open block or on the orb.
pub fn collides(id: &str, rect: Rect) -> bool {
    let orb = lock(&OBSTACLES).0;
    with(|s| s.last_blocks.iter().chain(s.blocks.iter()).filter(|(other, _, _)| other != id).map(|(other, _, r)| s.forced.get(other).map_or(*r, |(to, _)| *to)).chain(orb).any(|r| r.shrink(0.5).intersects(rect.shrink(0.5))))
}

/// "Tidy the windows": every open, unpinned block into one grid around the orb. How many moved.
pub fn retile_all(bounds: Rect) -> usize {
    let orb = lock(&OBSTACLES).0;
    with(|s| {
        let mut ids: Vec<String> = Vec::new();
        for (id, _, _) in s.last_blocks.iter().chain(s.blocks.iter()) {
            if !s.pinned.contains(id) && !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let keep_out = s.last_blocks.iter().chain(s.blocks.iter()).filter(|(id, _, _)| s.pinned.contains(id)).map(|(_, _, r)| *r).chain(orb).reduce(|a, b| a.union(b));
        let Some(cells) = tile_cells(bounds, ids.len(), keep_out, TILE_GAP).or_else(|| tile_cells(bounds, ids.len(), orb, TILE_GAP)) else { return 0 };
        s.maximized = None;
        for (id, cell) in ids.iter().zip(cells) {
            s.forced.insert(id.clone(), (cell, FORCED_FRAMES));
        }
        ids.len()
    })
}

/// The dock state is global: tests that change it run one at a time.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    lock(&L)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))
    }

    #[test]
    fn only_a_block_over_the_centre_moves_the_emblem() {
        let s = screen();
        // The text console alone (the right 440 px): the centre is free.
        let console = Rect::from_min_max(pos2(1920.0 - 440.0, 0.0), pos2(1920.0, 1080.0));
        assert!(!centre_covered(s, &[console]));
        // A bottom-left RESEARCH // ACTIVE box and a top-left note: still free.
        let research = Rect::from_min_size(pos2(24.0, 900.0), vec2(320.0, 130.0));
        let note = Rect::from_min_size(pos2(24.0, 60.0), vec2(400.0, 260.0));
        assert!(!centre_covered(s, &[console, research, note]));
        // A full report grid crosses the middle: dock.
        let grid = Rect::from_min_max(pos2(24.0, 60.0), pos2(1400.0, 900.0));
        assert!(centre_covered(s, &[console, grid]));
        // A block just touching the 360 px square counts; one 1 px short does not.
        let zone = centre_safe_zone(s);
        assert_eq!(zone.size(), vec2(360.0, 360.0));
        assert_eq!(zone.center(), s.center());
        assert!(centre_covered(s, &[Rect::from_min_size(pos2(zone.right() - 1.0, 0.0), vec2(50.0, 1080.0))]));
        assert!(!centre_covered(s, &[Rect::from_min_size(pos2(zone.right() + 1.0, 0.0), vec2(50.0, 1080.0))]));
        assert!(!centre_covered(s, &[]));
    }

    #[test]
    fn the_docked_emblem_sits_left_of_the_open_console() {
        let s = screen();
        assert_eq!(docked_centre(s, 0.0), pos2(1920.0 - 75.0, 1080.0 - 75.0));
        let c = docked_centre(s, 440.0);
        assert_eq!(c, pos2(1920.0 - 440.0 - 75.0, 1080.0 - 75.0));
        // The docked emblem (105 px) stays clear of the console.
        assert!(c.x + 52.5 < 1920.0 - 440.0);
    }

    #[test]
    fn views_open_inside_unless_detached_and_dock_back_on_request() {
        let _g = test_lock();
        dock_all();
        assert!(!adopt("system_telemetry"), "in-app by default");
        detach("system_telemetry");
        assert!(is_detached("system_telemetry"));
        // "Open the simulation lab in a separate window": the next sim opens detached.
        detach_next("sim:");
        assert!(!adopt("schedule"));
        assert!(adopt("sim:abc"), "the new simulation takes the request");
        assert!(!adopt("sim:def"), "only the next one");
        assert_eq!(dock_all(), 2);
        assert!(!is_detached("system_telemetry") && !is_detached("sim:abc"));
        forget("sim:abc");
        forget("sim:def");
        forget("system_telemetry");
        forget("schedule");
    }

    #[test]
    fn a_detached_window_starts_where_its_block_was() {
        let _g = test_lock();
        let r = Rect::from_min_size(pos2(48.0, 60.0), vec2(900.0, 560.0));
        publish_block("dock_test_where", "Plotter", r);
        assert_eq!(block_rect("dock_test_where"), Some(r));
        begin_frame();
        begin_frame();
        assert_eq!(block_rect("dock_test_where"), Some(r), "remembered after the block is gone");
        forget("dock_test_where");
        assert_eq!(block_rect("dock_test_where"), None);
    }

    #[test]
    fn a_spoken_name_closes_the_matching_in_app_block_with_a_fade() {
        let _g = test_lock();
        let r = Rect::from_min_size(pos2(40.0, 80.0), vec2(820.0, 680.0));
        publish_block("features", "Feature Lab // every system", r);
        publish_block("system_telemetry", "J.A.R.V.I.S. // SYSTEM TELEMETRY", r);
        publish_block("sim:pendulum", "Pendulum", r);
        begin_frame();
        // The title wins: "feature lab" is the lab, never the simulation ("lab" → `sim:`).
        assert_eq!(blocks_named("the Feature Lab"), vec![("features".to_string(), "Feature Lab // every system".to_string())]);
        assert_eq!(blocks_named("telemetry window").len(), 1);
        assert_eq!(blocks_named("my system stats")[0].0, "system_telemetry", "a view name finds its block by id prefix");
        assert_eq!(blocks_named("the plotter")[0].0, "sim:pendulum");
        assert!(blocks_named("Spotify").is_empty() && blocks_named("web search").is_empty(), "not a block: the caller looks elsewhere");
        assert_eq!(close_named("feature lab"), vec!["Feature Lab // every system".to_string()]);
        let t = closing_progress("features").expect("fading");
        assert!(t < 1.0, "the fade has only begun: {t}");
        assert_eq!(closing_progress("system_telemetry"), None, "only the named block");
        std::thread::sleep(CLOSE_FADE + Duration::from_millis(30));
        assert_eq!(closing_progress("features"), Some(1.0), "faded out: close now");
        assert_eq!(closing_progress("features"), None, "the request is consumed");
        // "Close the windows": every block at once.
        assert_eq!(close_all_blocks(), 3);
        for id in ["features", "system_telemetry", "sim:pendulum"] {
            assert!(closing_progress(id).is_some(), "{id}");
            forget(id);
        }
        begin_frame();
        begin_frame();
    }

    #[test]
    fn spoken_names_find_their_windows() {
        assert_eq!(id_prefix_for("the telemetry window"), Some("system_telemetry"));
        assert_eq!(id_prefix_for("function plotter"), Some("sim:"));
        assert_eq!(id_prefix_for("QR code"), Some("pairing"));
        assert_eq!(id_prefix_for("my calendar"), Some("schedule"));
        assert_eq!(id_prefix_for("banana"), None);
        assert_eq!(HudViewMode::parse("the 3D core"), Some(HudViewMode::CoreArchitectureSphere));
        assert_eq!(HudViewMode::parse("sim lab"), Some(HudViewMode::SimLab));
        assert_eq!(HudViewMode::parse("HUD board"), Some(HudViewMode::Board));
        assert_eq!(HudViewMode::parse("home"), Some(HudViewMode::Home));
        for mode in [HudViewMode::Home, HudViewMode::Board, HudViewMode::CoreArchitectureSphere, HudViewMode::SimLab, HudViewMode::Telemetry, HudViewMode::Schedule, HudViewMode::TextConsole] {
            // Every tab's id reads back as that tab (voice and clicks share one table).
            assert_eq!(HudViewMode::parse(&mode.id().replace('_', " ")), Some(mode));
            assert!(!mode.label().is_empty() && mode.label().is_ascii(), "a plain technical label, no icon: {}", mode.label());
        }
    }


    #[test]
    fn new_windows_never_land_on_open_windows_or_the_orb() {
        let _g = test_lock();
        let bounds = Rect::from_min_max(pos2(8.0, 60.0), pos2(2552.0, 1380.0));
        // The old cascade: the Feature Lab one slot (34 × 30 px) down from the telemetry block.
        let telemetry = Rect::from_min_size(pos2(48.0, 84.0), vec2(820.0, 600.0));
        let cascade = Rect::from_min_size(telemetry.min + vec2(34.0, 30.0), vec2(820.0, 680.0));
        assert!(cascade.intersects(telemetry), "reproduces the screenshot: the lab covers the telemetry");
        let at = find_non_overlapping_slot(bounds, vec2(820.0, 680.0), &[telemetry], 12.0).unwrap();
        let lab = Rect::from_min_size(at, vec2(820.0, 680.0));
        assert!(!lab.intersects(telemetry) && bounds.contains_rect(lab), "{lab:?}");
        assert_eq!(at, pos2(telemetry.right() + 12.0, bounds.top()), "beside it, top-most first");
        // The orb in the centre is protected too.
        let orb = Rect::from_center_size(bounds.center(), vec2(520.0, 520.0));
        publish_obstacles(Some(orb), Vec::new());
        let at = place_new_window(bounds, &[], vec2(820.0, 680.0), None, pos2(40.0, 80.0));
        let r = Rect::from_min_size(at, vec2(820.0, 680.0));
        assert!(!r.intersects(orb), "{r:?} on the orb");
        // Nothing fits: no panic, the cascade.
        assert_eq!(find_non_overlapping_slot(Rect::from_min_size(Pos2::ZERO, vec2(100.0, 100.0)), vec2(300.0, 300.0), &[], 12.0), None);
        // "Overlap the windows": the cascade again.
        set_overlap_allowed(true);
        assert_eq!(place_new_window(bounds, &[], vec2(820.0, 680.0), None, pos2(40.0, 80.0)), pos2(40.0, 80.0));
        set_overlap_allowed(false);
        // Spanning two monitors: a window fits inside one monitor, never across the bezel.
        let left = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 1000.0));
        let right = Rect::from_min_max(pos2(1000.0, 0.0), pos2(2000.0, 1000.0));
        let blocker = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 1000.0));
        let all = left.union(right);
        let free = find_non_overlapping_slot(all, vec2(700.0, 400.0), &[blocker], 12.0).unwrap();
        assert!(free.x < 1000.0 && free.x + 700.0 > 1000.0, "unconstrained it straddles the bezel");
        publish_obstacles(None, vec![blocker]);
        let at = place_new_window(all, &[left, right], vec2(700.0, 400.0), None, Pos2::ZERO);
        assert!(right.contains_rect(Rect::from_min_size(at, vec2(700.0, 400.0))), "{at:?}");
        publish_obstacles(None, Vec::new());
    }

    fn overlapping(rects: &[Rect]) -> Vec<(usize, usize)> {
        let mut pairs = Vec::new();
        for a in 0..rects.len() {
            for b in a + 1..rects.len() {
                if rects[a].shrink(0.5).intersects(rects[b].shrink(0.5)) {
                    pairs.push((a, b));
                }
            }
        }
        pairs
    }

    #[test]
    fn a_grid_of_cells_keeps_off_the_orb_and_never_goes_below_the_smallest_block() {
        let bounds = Rect::from_min_max(pos2(8.0, 60.0), pos2(2552.0, 1380.0));
        let orb = Rect::from_center_size(bounds.center(), vec2(420.0, 420.0));
        for count in [1usize, 2, 4, 8, 12] {
            let cells = tile_cells(bounds, count, Some(orb), TILE_GAP).unwrap_or_else(|| panic!("{count} blocks fit a 2560 px screen"));
            assert_eq!(cells.len(), count);
            assert!(overlapping(&cells).is_empty(), "{count}: {cells:?}");
            for cell in &cells {
                assert!(bounds.expand(0.5).contains_rect(*cell) && !cell.intersects(orb), "{count}: {cell:?}");
                assert!(cell.width() >= MIN_BLOCK[0] && cell.height() >= MIN_BLOCK[1], "{cell:?}");
            }
        }
        // The portrait monitor: twelve blocks still fit, three wide.
        let portrait = Rect::from_min_max(pos2(8.0, 60.0), pos2(1072.0, 1860.0));
        let cells = tile_cells(portrait, 12, Some(Rect::from_center_size(portrait.center(), vec2(300.0, 300.0))), TILE_GAP).expect("twelve on the portrait monitor");
        assert!(overlapping(&cells).is_empty() && cells.iter().all(|c| portrait.expand(0.5).contains_rect(*c)));
        // A phone-sized area cannot take twelve: no grid, the caller cascades.
        assert_eq!(tile_cells(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0)), 12, None, TILE_GAP), None);
        assert_eq!(tile_cells(bounds, 0, None, TILE_GAP), Some(Vec::new()));
    }

    #[test]
    fn twelve_new_blocks_never_overlap_each_other_or_the_orb() {
        let _g = test_lock();
        set_overlap_allowed(false);
        for (w, h) in [(2560.0, 1440.0), (1080.0, 1920.0), (6240.0, 1920.0)] {
            begin_frame();
            begin_frame();
            with(|s| s.forced.clear());
            let bounds = Rect::from_min_max(pos2(8.0, 60.0), pos2(w - 8.0, h - 60.0));
            let orb = Rect::from_center_size(bounds.center(), vec2(380.0, 380.0));
            publish_obstacles(Some(orb), Vec::new());
            let mut rects: HashMap<String, Rect> = HashMap::new();
            for n in 0..12 {
                let id = format!("tile_test_{n}");
                let (at, size) = place_new_block(&id, bounds, &[], vec2(820.0, 600.0), pos2(40.0, 80.0));
                rects.insert(id.clone(), Rect::from_min_size(at, size));
                // Blocks the dock moved into a grid are where it sent them.
                for (other, rect) in rects.iter_mut() {
                    if let Some((to, _)) = with(|s| s.forced.get(other).copied()) {
                        *rect = to;
                    }
                }
                begin_frame();
                for (other, rect) in &rects {
                    publish_block(other, "Tile test", *rect);
                }
                let placed: Vec<Rect> = rects.values().copied().collect();
                assert!(overlapping(&placed).is_empty(), "{w}x{h}: {} blocks overlap: {placed:?}", n + 1);
                assert!(placed.iter().all(|r| !r.intersects(orb) && bounds.expand(0.5).contains_rect(*r)), "{w}x{h}: {} blocks: {placed:?}", n + 1);
                assert!(placed.iter().all(|r| r.width() >= MIN_BLOCK[0] && r.height() >= MIN_BLOCK[1]));
            }
            for n in 0..12 {
                forget(&format!("tile_test_{n}"));
            }
        }
        begin_frame();
        begin_frame();
        publish_obstacles(None, Vec::new());
    }

    #[test]
    fn pinned_blocks_stay_and_a_maximised_block_goes_back_where_it_was() {
        let _g = test_lock();
        let bounds = Rect::from_min_max(pos2(8.0, 60.0), pos2(1912.0, 1020.0));
        let home = Rect::from_min_size(pos2(60.0, 90.0), vec2(500.0, 320.0));
        for id in ["pin_a", "pin_b"] {
            publish_block(id, "Pin test", home);
        }
        begin_frame();
        assert!(toggle_pin("pin_a") && is_pinned("pin_a"));
        assert_eq!(close_all_blocks(), 1, "the pinned block stays through 'close the windows'");
        assert!(closing_progress("pin_b").is_some() && closing_progress("pin_a").is_none());
        assert!(!toggle_pin("pin_a") && !is_pinned("pin_a"));
        // Maximise: held at the full bounds for a few frames; again: back home.
        assert!(toggle_maximize("pin_a", bounds) && is_maximized("pin_a"));
        assert_eq!(take_forced("pin_a"), Some(bounds.shrink(8.0)));
        assert!(!toggle_maximize("pin_a", bounds) && !is_maximized("pin_a"));
        assert_eq!(take_forced("pin_a"), Some(home));
        for _ in 0..FORCED_FRAMES {
            let _ = take_forced("pin_a");
        }
        assert_eq!(take_forced("pin_a"), None, "after a few frames the block is Sir's to move again");
        // The search lights up the blocks whose title or content carries every word.
        describe_block("pin_a", "Ohm's law: voltage = current x resistance");
        set_search("Voltage LAW");
        assert_eq!(search_hits(), vec![("pin_a".to_string(), "Pin test".to_string())]);
        assert!(search_matches("pin test", "Pin test", "") && !search_matches("reactor", "Pin test", "voltage"));
        set_search("");
        assert!(search_hits().is_empty() && search().is_empty());
        assert_eq!(block_content("pin_a").as_deref(), Some("Ohm's law: voltage = current x resistance"));
        for id in ["pin_a", "pin_b"] {
            forget(id);
        }
        assert_eq!(block_content("pin_a"), None);
        begin_frame();
        begin_frame();
    }
}
