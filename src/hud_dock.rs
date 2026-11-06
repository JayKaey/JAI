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
    });
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
        let ids: HashSet<String> = s.last_blocks.iter().chain(s.blocks.iter()).map(|(id, _, _)| id.clone()).filter(|id| id != "console").collect();
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
    Some(if has(&["telemetry", "stats", "system", "vram", "cpu", "gpu"]) {
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

/// An in-app block was drawn at `rect` this frame.
pub fn publish_block(id: &str, title: &str, rect: Rect) {
    with(|s| {
        s.blocks.push((id.to_string(), title.to_string(), rect));
        s.last_rect.insert(id.to_string(), rect);
    });
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
}
