//! Jarvis' own windows moved and sized on command: "move it to the middle", "centre the Earth",
//! "put the earth on the left half", "make it bigger", "full screen the Earth", "exit full
//! screen", "move the radar to my second monitor", "put them side by side", "resize it to half
//! the screen", "shrink it", "minimise it", "restore it", "mach es größer", "verschieb es in die
//! Mitte", "Vollbild".
//!
//! One tool, `arrange_jarvis_window`, for his views on the board (in-app blocks: they glide
//! there, `hud_dock`) and in windows of their own (OS windows: `ViewportCommand`s). The HUD
//! carries an arrangement out in its next frame, where it knows the screens (`gui`).
//!
//! What "it" means: the view of his that was opened, moved or arranged last, while that is newer
//! than the app the conversation is about (else "it" stays that app, as before). His own HUD is
//! never moved unless Sir says "yourself" or "Jarvis" (`configure_interface` does that).
//!
//! The fast path here catches only what has no way yet: a view named by name ("full screen the
//! Earth"), and for "it" the places and sizes nothing else knows (the middle, a corner, a third,
//! half the screen, "shrink it", "side by side", "swap them"). "Make it full screen", "minimise
//! it", "move it to the left", "make it bigger" keep their rules; the tools hand them over here
//! when "it" is one of his views (`redirect`).

use crate::tools::{str_arg, ToolContext};
use eframe::egui::{pos2, vec2, Rect, Vec2};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// "It" means a view of his for this long after it was opened, moved or arranged, in a sentence
/// that can only mean a view ("move it to the middle", "resize it to half the screen").
const IT_TTL: Duration = Duration::from_secs(15 * 60);
/// A window command that could just as well mean an app ("make it full screen", "minimise it")
/// goes to his view for this long only: after that, "it" is the app in front again.
const TAKE_OVER_TTL: Duration = Duration::from_secs(5 * 60);
/// An arrangement for a view that is still opening waits this long for it.
const OPENING_WAIT: Duration = Duration::from_secs(5);
/// An arrangement for a view that is up is carried out in the next frame (or dropped by then).
const NEXT_FRAME_WAIT: Duration = Duration::from_millis(1500);
/// The gap between two views put side by side.
pub const PAIR_GAP: f32 = 12.0;

// ------------------------------------------------------------------------------ the words ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Move,
    Resize,
    Fullscreen,
    ExitFullscreen,
    Maximize,
    Restore,
    Minimize,
    Detach,
    Dock,
    ToMonitor,
    SideBySide,
    Swap,
}

impl Action {
    pub fn parse(raw: &str) -> Option<Self> {
        let a = raw.trim().to_lowercase().replace([' ', '-'], "_");
        Some(match a.as_str() {
            "move" | "place" | "put" | "center" | "centre" | "snap" | "position" => Self::Move,
            "resize" | "size" | "scale" | "bigger" | "smaller" | "grow" | "shrink" => Self::Resize,
            "fullscreen" | "full_screen" | "enter_fullscreen" | "vollbild" => Self::Fullscreen,
            "exit_fullscreen" | "leave_fullscreen" | "exit_full_screen" | "unfullscreen" | "end_fullscreen" => Self::ExitFullscreen,
            "maximize" | "maximise" | "maximieren" => Self::Maximize,
            "restore" | "unminimize" | "unminimise" | "wiederherstellen" => Self::Restore,
            "minimize" | "minimise" | "minimieren" => Self::Minimize,
            "detach" | "pop_out" | "undock" => Self::Detach,
            "dock" | "dock_back" | "bring_back" => Self::Dock,
            "to_monitor" | "move_to_monitor" | "monitor" => Self::ToMonitor,
            "side_by_side" | "next_to_each_other" | "nebeneinander" => Self::SideBySide,
            "swap" | "exchange" | "tauschen" => Self::Swap,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Resize => "resize",
            Self::Fullscreen => "fullscreen",
            Self::ExitFullscreen => "exit_fullscreen",
            Self::Maximize => "maximize",
            Self::Restore => "restore",
            Self::Minimize => "minimize",
            Self::Detach => "detach",
            Self::Dock => "dock",
            Self::ToMonitor => "to_monitor",
            Self::SideBySide => "side_by_side",
            Self::Swap => "swap",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Centre,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    LeftThird,
    CentreThird,
    RightThird,
    LeftTwoThirds,
    RightTwoThirds,
}

impl Position {
    #[cfg(test)]
    pub const ALL: [Position; 14] = [
        Self::Centre,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
        Self::LeftThird,
        Self::CentreThird,
        Self::RightThird,
        Self::LeftTwoThirds,
        Self::RightTwoThirds,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Centre => "centre",
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::TopLeft => "top_left",
            Self::TopRight => "top_right",
            Self::BottomLeft => "bottom_left",
            Self::BottomRight => "bottom_right",
            Self::LeftThird => "left_third",
            Self::CentreThird => "centre_third",
            Self::RightThird => "right_third",
            Self::LeftTwoThirds => "left_two_thirds",
            Self::RightTwoThirds => "right_two_thirds",
        }
    }

    /// A place as the tool or Sir names it: "centre", "the middle of my screen", "top_left",
    /// "the upper left corner", "left half", "die linke Seite", "oben rechts", "Mitte".
    pub fn parse(raw: &str) -> Option<Self> {
        let mut p = raw.trim().to_lowercase().replace(['_', '-'], " ");
        for lead in ["the ", "my ", "your ", "die ", "den ", "der ", "dem "] {
            if let Some(rest) = p.strip_prefix(lead) {
                p = rest.to_string();
            }
        }
        for tail in [
            " of the screen", " of my screen", " of the monitor", " of my monitor", " of the display", " of the hud", " on the screen", " on my screen", " of it",
            " des bildschirms", " vom bildschirm", " am bildschirm", " auf dem bildschirm",
        ] {
            if let Some(rest) = p.strip_suffix(tail) {
                p = rest.to_string();
            }
        }
        let p = p.split_whitespace().filter(|w| !matches!(*w, "side" | "half" | "corner" | "part" | "seite" | "hälfte" | "haelfte" | "ecke" | "teil" | "hand")).collect::<Vec<_>>().join(" ");
        Some(match p.as_str() {
            "centre" | "center" | "middle" | "mitte" | "zentrum" | "bildschirmmitte" | "centre of" | "center of" => Self::Centre,
            "left" | "links" | "linke" | "linken" | "lefthand" => Self::Left,
            "right" | "rechts" | "rechte" | "rechten" | "righthand" => Self::Right,
            "top" | "upper" | "oben" | "obere" | "oberen" => Self::Top,
            "bottom" | "lower" | "unten" | "untere" | "unteren" => Self::Bottom,
            "top left" | "upper left" | "left top" | "oben links" | "links oben" | "linke obere" | "obere linke" | "linken oberen" | "oberen linken" => Self::TopLeft,
            "top right" | "upper right" | "right top" | "oben rechts" | "rechts oben" | "rechte obere" | "obere rechte" | "rechten oberen" | "oberen rechten" => Self::TopRight,
            "bottom left" | "lower left" | "left bottom" | "unten links" | "links unten" | "linke untere" | "untere linke" | "linken unteren" | "unteren linken" => Self::BottomLeft,
            "bottom right" | "lower right" | "right bottom" | "unten rechts" | "rechts unten" | "rechte untere" | "untere rechte" | "rechten unteren" | "unteren rechten" => Self::BottomRight,
            "left third" | "first third" | "linkes drittel" | "linke drittel" | "linken drittel" => Self::LeftThird,
            "centre third" | "center third" | "middle third" | "mittleres drittel" | "mittlere drittel" | "mittleren drittel" => Self::CentreThird,
            "right third" | "last third" | "rechtes drittel" | "rechte drittel" | "rechten drittel" => Self::RightThird,
            "left two thirds" | "linke zwei drittel" | "linken zwei drittel" => Self::LeftTwoThirds,
            "right two thirds" | "rechte zwei drittel" | "rechten zwei drittel" => Self::RightTwoThirds,
            _ => return None,
        })
    }

    /// One of the four sides (Windows' own snap rules already know these for an app).
    fn is_side(self) -> bool {
        matches!(self, Self::Left | Self::Right | Self::Top | Self::Bottom)
    }

    fn spoken(self) -> &'static str {
        match self {
            Self::Centre => "in the middle",
            Self::Left => "on the left half",
            Self::Right => "on the right half",
            Self::Top => "on the top half",
            Self::Bottom => "on the bottom half",
            Self::TopLeft => "in the top left corner",
            Self::TopRight => "in the top right corner",
            Self::BottomLeft => "in the bottom left corner",
            Self::BottomRight => "in the bottom right corner",
            Self::LeftThird => "on the left third",
            Self::CentreThird => "on the middle third",
            Self::RightThird => "on the right third",
            Self::LeftTwoThirds => "on the left two thirds",
            Self::RightTwoThirds => "on the right two thirds",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Bigger,
    Smaller,
    MuchBigger,
    MuchSmaller,
    /// Half the screen: the half its middle is on.
    Half,
    /// This share of the screen's width and height (percent).
    Percent(f32),
    /// Exactly this large (points).
    Exact(f32, f32),
}

impl Size {
    /// "bigger", "a bit smaller", "much bigger", "half", "half the screen", "60%", "60 percent",
    /// "900x600", "900 by 600", "größer", "kleiner".
    pub fn parse(raw: &str) -> Option<Self> {
        static EXACT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{2,4})\s*(?:x|by|×|mal|\*)\s*(\d{2,4})(?:\s*(?:pixels?|px))?$").expect("size regex"));
        static PERCENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,3})\s*(?:%|percent|prozent)(?:\s+of\s+(?:the|my)\s+screen)?$").expect("percent regex"));
        let s = raw.trim().to_lowercase().replace('_', " ");
        let s = s.trim_start_matches("to ").trim_start_matches("a ").trim();
        if let Some(c) = EXACT.captures(s) {
            return Some(Self::Exact(c[1].parse().ok()?, c[2].parse().ok()?));
        }
        if let Some(c) = PERCENT.captures(s) {
            return Some(Self::Percent(c[1].parse::<f32>().ok()?.clamp(10.0, 100.0)));
        }
        let much = s.starts_with("much ") || s.starts_with("way ") || s.starts_with("viel ") || s.starts_with("a lot ");
        let s = s.trim_start_matches("much ").trim_start_matches("way ").trim_start_matches("viel ").trim_start_matches("a lot ").trim_start_matches("bit ").trim_start_matches("little ").trim_start_matches("slightly ").trim_start_matches("etwas ");
        Some(match s {
            "bigger" | "larger" | "grow" | "größer" | "groesser" | "double" | "twice as big" | "huge" if much || matches!(s, "huge") => Self::MuchBigger,
            "bigger" | "larger" | "grow" | "größer" | "groesser" | "enlarge" => Self::Bigger,
            "double" | "twice as big" => Self::MuchBigger,
            "smaller" | "shrink" | "kleiner" | "tiny" if much || s == "tiny" => Self::MuchSmaller,
            "smaller" | "shrink" | "kleiner" => Self::Smaller,
            "half" | "half the screen" | "half of the screen" | "half screen" | "half the size" | "die hälfte" | "halber bildschirm" | "halben bildschirm" | "hälfte" => Self::Half,
            "quarter" | "a quarter" | "quarter of the screen" | "a quarter of the screen" => Self::Percent(50.0),
            "third" | "a third" | "a third of the screen" | "third of the screen" => Self::Percent(33.0),
            "full" | "whole screen" | "the whole screen" => Self::Percent(100.0),
            _ => return None,
        })
    }

    fn spoken(self) -> String {
        match self {
            Self::Bigger => "larger".into(),
            Self::Smaller => "smaller".into(),
            Self::MuchBigger => "a good deal larger".into(),
            Self::MuchSmaller => "a good deal smaller".into(),
            Self::Half => "half the screen".into(),
            Self::Percent(p) => format!("{p:.0} percent of the screen"),
            Self::Exact(w, h) => format!("{w:.0} by {h:.0}"),
        }
    }

    fn name(self) -> String {
        match self {
            Self::Bigger => "bigger".into(),
            Self::Smaller => "smaller".into(),
            Self::MuchBigger => "much bigger".into(),
            Self::MuchSmaller => "much smaller".into(),
            Self::Half => "half".into(),
            Self::Percent(p) => format!("{p:.0}%"),
            Self::Exact(w, h) => format!("{w:.0}x{h:.0}"),
        }
    }
}

// ------------------------------------------------------------------------------ geometry ----

/// The smallest a view is made.
fn min_size() -> Vec2 {
    Vec2::from(crate::hud_dock::MIN_BLOCK)
}

/// `rect` moved (and shrunk only if it must be) to lie inside `area`.
pub fn fit(rect: Rect, area: Rect) -> Rect {
    let size = rect.size().min(area.size());
    let min = pos2(rect.min.x.clamp(area.min.x, area.max.x - size.x), rect.min.y.clamp(area.min.y, area.max.y - size.y));
    Rect::from_min_size(min, size)
}

/// Where `position` puts a view in `area`: the middle at its own size, a half, a quarter, a
/// third or two.
pub fn place(area: Rect, current: Rect, position: Position) -> Rect {
    let (l, t, r, b) = (area.left(), area.top(), area.right(), area.bottom());
    let (cx, cy) = (area.center().x, area.center().y);
    let third = area.width() / 3.0;
    let span = |x0: f32, x1: f32| Rect::from_min_max(pos2(x0, t), pos2(x1, b));
    match position {
        Position::Centre => Rect::from_center_size(area.center(), current.size().min(area.size())),
        Position::Left => span(l, cx),
        Position::Right => span(cx, r),
        Position::Top => Rect::from_min_max(pos2(l, t), pos2(r, cy)),
        Position::Bottom => Rect::from_min_max(pos2(l, cy), pos2(r, b)),
        Position::TopLeft => Rect::from_min_max(pos2(l, t), pos2(cx, cy)),
        Position::TopRight => Rect::from_min_max(pos2(cx, t), pos2(r, cy)),
        Position::BottomLeft => Rect::from_min_max(pos2(l, cy), pos2(cx, b)),
        Position::BottomRight => Rect::from_min_max(pos2(cx, cy), pos2(r, b)),
        Position::LeftThird => span(l, l + third),
        Position::CentreThird => span(l + third, r - third),
        Position::RightThird => span(r - third, r),
        Position::LeftTwoThirds => span(l, r - third),
        Position::RightTwoThirds => span(l + third, r),
    }
}

/// `current` made `size`, about its middle, inside `area`, never smaller than a view may be.
pub fn resize(area: Rect, current: Rect, size: Size) -> Rect {
    let want = match size {
        Size::Bigger => current.size() * 1.25,
        Size::Smaller => current.size() * 0.8,
        Size::MuchBigger => current.size() * 1.6,
        Size::MuchSmaller => current.size() * 0.6,
        Size::Half => return place(area, current, if current.center().x < area.center().x { Position::Left } else { Position::Right }),
        Size::Percent(p) => area.size() * (p / 100.0).clamp(0.1, 1.0),
        Size::Exact(w, h) => vec2(w, h),
    };
    let size = want.max(min_size()).min(area.size());
    fit(Rect::from_center_size(current.center(), size), area)
}

/// Where a view standing at `current` goes in `area`: resized first, then placed.
pub fn target_rect(area: Rect, current: Rect, position: Option<Position>, size: Option<Size>) -> Rect {
    let mut r = current;
    if let Some(s) = size {
        r = resize(area, r, s);
    }
    if let Some(p) = position {
        r = place(area, r, p);
    }
    fit(r, area)
}

/// Two views side by side in `area`: its left and right halves, `PAIR_GAP` apart.
pub fn side_by_side(area: Rect) -> (Rect, Rect) {
    let mid = area.center().x;
    (Rect::from_min_max(area.min, pos2(mid - PAIR_GAP / 2.0, area.max.y)), Rect::from_min_max(pos2(mid + PAIR_GAP / 2.0, area.min.y), area.max))
}

// ----------------------------------------------------------------------- the HUD's queue ----

/// One arrangement for the HUD's next frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement {
    /// The block or window id (`earth`, `network_radar`, `sim:…`, a window's id).
    pub id: String,
    pub action: Action,
    pub position: Option<Position>,
    pub size: Option<Size>,
    /// The monitor phrase for `ToMonitor` ("2", "second", "left").
    pub monitor: Option<Value>,
    /// The second view (`SideBySide`, `Swap`).
    pub other: Option<String>,
    /// Dropped when it could not be carried out by then.
    pub deadline: Instant,
}

static PENDING: Mutex<Vec<Arrangement>> = Mutex::new(Vec::new());
/// Where a block sent to another monitor opens as a window of its own (screen points).
static OS_PLACEMENT: Mutex<Option<HashMap<String, Rect>>> = Mutex::new(None);
static ANY_PLACEMENT: AtomicBool = AtomicBool::new(false);
/// The views in windows of their own that Sir put in full screen ("exit full screen" means them).
static FULLSCREEN_WINDOWS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// For the HUD's next frame.
pub fn queue(arrangement: Arrangement) {
    crate::crash_guard::lock(&PENDING).push(arrangement);
    crate::gui::refresh();
}

/// Everything waiting (the HUD puts back what is not ready yet with `requeue`).
pub fn take_pending() -> Vec<Arrangement> {
    let mut pending = crate::crash_guard::lock(&PENDING);
    if pending.is_empty() {
        return Vec::new();
    }
    std::mem::take(&mut *pending)
}

pub fn requeue(arrangement: Arrangement) {
    crate::crash_guard::lock(&PENDING).push(arrangement);
}

/// Whether arrangements are waiting.
pub fn pending() -> bool {
    !crate::crash_guard::lock(&PENDING).is_empty()
}

/// A block sent to another monitor opens there as a window of its own (`rect` in screen points).
pub fn set_os_placement(id: &str, rect: Rect) {
    crate::crash_guard::lock(&OS_PLACEMENT).get_or_insert_with(HashMap::new).insert(id.to_string(), rect);
    ANY_PLACEMENT.store(true, Ordering::Release);
}

/// Where window `id` opens instead of where its block stood (None: where its block stood).
pub fn os_placement(id: &str) -> Option<Rect> {
    if !ANY_PLACEMENT.load(Ordering::Acquire) {
        return None;
    }
    crate::crash_guard::lock(&OS_PLACEMENT).as_ref()?.get(id).copied()
}

/// Block `id` is on the board again: a later window of its own opens where the block stands.
pub fn forget_os_placement(id: &str) {
    if !ANY_PLACEMENT.load(Ordering::Acquire) {
        return;
    }
    let mut map = crate::crash_guard::lock(&OS_PLACEMENT);
    if let Some(m) = map.as_mut() {
        m.remove(id);
        if m.is_empty() {
            ANY_PLACEMENT.store(false, Ordering::Release);
        }
    }
}

/// Window `id` went to (true) or left (false) full screen.
pub fn note_window_fullscreen(id: &str, on: bool) {
    let mut ids = crate::crash_guard::lock(&FULLSCREEN_WINDOWS);
    ids.retain(|known| known != id);
    if on {
        ids.push(id.to_string());
    }
}

/// The views in windows of their own Sir put in full screen (None: none, the usual case).
pub fn fullscreen_windows() -> Option<Vec<String>> {
    let ids = crate::crash_guard::lock(&FULLSCREEN_WINDOWS);
    (!ids.is_empty()).then(|| ids.clone())
}

/// The view of his filling a screen (a block on the HUD, or a window of its own), if one does.
pub fn fullscreen_view() -> Option<String> {
    crate::hud_dock::fullscreen_block().or_else(|| {
        let mut ids = crate::crash_guard::lock(&FULLSCREEN_WINDOWS);
        ids.retain(|id| crate::hud_dock::whereabouts(id) == crate::hud_dock::Whereabouts::Window);
        ids.last().cloned()
    })
}

// ------------------------------------------------------------------------- who "it" is ----

/// The view "it" means: the one of his opened, moved or arranged last, while that is newer than
/// the app the conversation is about (`app_age`: how long ago that app came up; None: none) and
/// not older than `ttl`.
pub fn it_is_view(latest: Option<(String, Duration)>, app_age: Option<Duration>, ttl: Duration) -> Option<String> {
    let (id, age) = latest?;
    if age > ttl || app_age.is_some_and(|app| app < age) {
        return None;
    }
    Some(id)
}

/// "It" for this conversation (None: the app or website just discussed).
fn it(ctx: &ToolContext, ttl: Duration) -> Option<String> {
    let app_age = ctx.focus.focused_app().and(ctx.focus.focused_app_age());
    it_is_view(crate::hud_dock::latest_block(), app_age, ttl)
}

/// The call an unnamed window command becomes when "it" is one of his views (or one of them
/// fills the screen, for "exit full screen"): `arrange_jarvis_window` arguments, or None (it
/// stays with the app). `view_is_it`: "it" is one of his views; `fullscreen`: a view of his
/// fills a screen; `latest_is_widget_window`: "it" is a window of the window engine, which
/// `update_native_window` sizes itself.
pub fn redirect(name: &str, args: &Value, view_is_it: bool, fullscreen: bool, latest_is_widget_window: bool) -> Option<Value> {
    let unnamed = |keys: &[&str]| str_arg(args, keys).is_none_or(|t| crate::focus::is_pronoun(t) || t == "focus");
    let call = |action: Action, extra: Value| {
        let mut call = json!({ "target": "it", "action": action.name() });
        if let (Some(o), Some(extra)) = (call.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                o.insert(k.clone(), v.clone());
            }
        }
        call
    };
    match name {
        "window_management" => {
            if !unnamed(&["target_window", "target", "app_name"]) {
                return None;
            }
            let action = str_arg(args, &["action"])?.to_lowercase().replace([' ', '-'], "_");
            if matches!(action.as_str(), "exit_fullscreen" | "leave_fullscreen" | "unfullscreen" | "exit_full_screen" | "end_fullscreen") && fullscreen {
                return Some(json!({ "target": "fullscreen", "action": "exit_fullscreen" }));
            }
            if !view_is_it {
                return None;
            }
            Some(match action.as_str() {
                "fullscreen" | "full_screen" | "go_fullscreen" | "enter_fullscreen" | "vollbild" => call(Action::Fullscreen, json!({})),
                "maximize" | "maximise" | "maximieren" => call(Action::Maximize, json!({})),
                "minimize" | "minimise" | "minimieren" => call(Action::Minimize, json!({})),
                "restore" | "unminimize" | "wiederherstellen" => call(Action::Restore, json!({})),
                "snap_left" | "left" | "links" => call(Action::Move, json!({ "position": "left" })),
                "snap_right" | "right" | "rechts" => call(Action::Move, json!({ "position": "right" })),
                "snap_top" | "top" | "up" | "oben" => call(Action::Move, json!({ "position": "top" })),
                "snap_bottom" | "bottom" | "down" | "unten" => call(Action::Move, json!({ "position": "bottom" })),
                "bigger" | "larger" | "grow" | "enlarge" | "make_bigger" => call(Action::Resize, json!({ "size": "bigger" })),
                "smaller" | "shrink" | "make_smaller" => call(Action::Resize, json!({ "size": "smaller" })),
                _ => return None,
            })
        }
        "update_native_window" => {
            if !view_is_it || latest_is_widget_window || !unnamed(&["id", "window_id"]) {
                return None;
            }
            // Only a new size (content, a title or a style stay with the window engine).
            if let Some(o) = args.as_object() {
                if o.keys().any(|k| !matches!(k.as_str(), "id" | "window_id" | "size" | "width" | "height")) {
                    return None;
                }
            }
            let size = match (args["width"].as_f64(), args["height"].as_f64()) {
                (Some(w), Some(h)) => format!("{w:.0}x{h:.0}"),
                _ => {
                    let s = str_arg(args, &["size"])?.to_lowercase();
                    match s.as_str() {
                        "much smaller" | "tiny" => "much smaller".to_string(),
                        "much bigger" | "huge" | "double" => "much bigger".to_string(),
                        "half" => "half".to_string(),
                        s if s.contains("small") || s.contains("klein") => "smaller".to_string(),
                        _ => "bigger".to_string(),
                    }
                }
            };
            Some(call(Action::Resize, json!({ "size": size })))
        }
        "move_window_to_monitor" => {
            if !view_is_it || !unnamed(&["app_or_window_name", "app_name", "window"]) {
                return None;
            }
            let monitor = args.get("target_monitor").or_else(|| args.get("monitor")).cloned()?;
            Some(call(Action::ToMonitor, json!({ "monitor": monitor })))
        }
        _ => None,
    }
}

/// `redirect` with what the conversation knows.
pub fn redirect_for(ctx: &ToolContext, name: &str, args: &Value) -> Option<Value> {
    if !matches!(name, "window_management" | "update_native_window" | "move_window_to_monitor") {
        return None;
    }
    let view = it(ctx, TAKE_OVER_TTL);
    let widget_window = view.as_deref().is_some_and(|id| ctx.widgets.find_window_named(id).is_some());
    redirect(name, args, view.is_some(), fullscreen_view().is_some(), widget_window)
}

// ---------------------------------------------------------------------- the fast path ----

/// What a phrase names: "it", "them", or one of his views by name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    It,
    Them,
    Named(String),
    /// Two views named ("the earth and the radar").
    Pair(String, String),
}

/// Words for "it" (the view or app the conversation is about).
fn is_it(o: &str) -> bool {
    crate::focus::is_pronoun(o) || matches!(o, "it there" | "that thing" | "this thing" | "the thing")
}

fn is_them(o: &str) -> bool {
    matches!(o, "them" | "both" | "both of them" | "those" | "these" | "the two" | "those two" | "these two" | "the windows" | "both windows" | "beide" | "die beiden" | "sie beide" | "beide fenster")
}

/// Never this tool: Jarvis himself (his HUD, board, orb, core), which only moves when Sir says
/// "yourself" or "Jarvis" (`configure_interface`).
fn names_himself(o: &str) -> bool {
    crate::tools::names_jarvis(o)
        || matches!(
            o.trim_start_matches("the ").trim_start_matches("your ").trim_start_matches("my "),
            "hud" | "board" | "hud board" | "orb" | "core" | "emblem" | "sphere" | "reactor" | "ring" | "rings" | "kugel" | "kern" | "interface" | "gui" | "screen" | "display" | "monitor" | "console" | "text console" | "dich" | "dein fenster" | "das hud" | "die kugel" | "den orb"
        )
}

/// A view of his named in a phrase ("the earth", "the network radar window", "die erde", "the
/// feature lab"): its name, or None (an app, a website or nothing of his).
fn view_name(o: &str) -> Option<String> {
    let words: Vec<&str> = o
        .split_whitespace()
        .filter(|w| !matches!(*w, "the" | "my" | "your" | "that" | "this" | "die" | "den" | "das" | "der" | "dem" | "mein" | "meine" | "meinen" | "deine" | "dein"))
        .collect();
    let says_window = words.last().is_some_and(|w| matches!(*w, "window" | "fenster" | "block" | "panel" | "view" | "ansicht"));
    let name = words.iter().filter(|w| !matches!(**w, "window" | "fenster" | "block" | "panel" | "view" | "ansicht")).copied().collect::<Vec<_>>().join(" ");
    if name.len() < 3 || names_himself(&name) {
        return None;
    }
    use crate::jarvis_views::JarvisSubWindow as View;
    // The panels by their own names: never an app of his.
    if matches!(View::parse(&name), Some(View::Earth | View::NetworkRadar | View::DiskMap)) && name.split_whitespace().count() <= 3 {
        return Some(name);
    }
    if crate::apps::find_spec(&name).is_some() || crate::sites::find(&name).is_some() {
        return None;
    }
    // "The PC stats window", "the drive space window": the window engine's live windows
    // (`spawn_native_window` live=…), never the HUD's telemetry block that "stats" also names.
    if says_window && matches!(name.as_str(), "pc stats" | "stats" | "pc stats live" | "drive space" | "drives" | "processes" | "network traffic") {
        return None;
    }
    // "The drive space window": a window of the window engine, which sizes its own.
    if says_window && crate::hud_dock::find_open(&name).is_none() {
        return None;
    }
    let known = matches!(name.as_str(), "feature lab" | "features" | "theme studio" | "mixer" | "audio mixer" | "volume mixer" | "telemetry" | "system telemetry" | "simulation lab" | "sim lab" | "plotter" | "qr code" | "schedule" | "calendar" | "stundenplan" | "kalender")
        || crate::hud_dock::find_open(&name).is_some();
    known.then_some(name)
}

fn target_of(o: &str) -> Option<Target> {
    let o = o.trim();
    if is_it(o) {
        return Some(Target::It);
    }
    if is_them(o) {
        return Some(Target::Them);
    }
    for joint in [" and ", " und ", " with ", " mit "] {
        if let Some((a, b)) = o.split_once(joint) {
            return Some(Target::Pair(view_name(a)?, view_name(b)?));
        }
    }
    view_name(o).map(Target::Named)
}

/// A monitor phrase ("my second monitor", "monitor 2", "the left screen", "den zweiten
/// Bildschirm"): the phrase for `monitors::resolve`.
fn monitor_of(w: &str) -> Option<Value> {
    static MONITOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:(?:my|the|your|meinen|meinem|den|dem|mein)\s+)?(?:(\d)(?:st|nd|rd|th)?|first|second|third|fourth|other|left|right|primary|main|next|erste[nm]?|zweite[nm]?|dritte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)$|^(?:monitor|screen|display|bildschirm)\s+(\d)$").expect("monitor regex")
    });
    MONITOR.is_match(w.trim()).then(|| {
        let digits: String = w.chars().filter(char::is_ascii_digit).collect();
        digits.parse::<u64>().map_or_else(|_| json!(w.trim()), |n| json!(n))
    })
}

static MOVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:move|put|place|shift|drag|push|snap|bring|send|throw|set|verschieb|verschiebe|schieb|schiebe|pack|packe|tu|tue|stell|stelle|setz|setze|leg|lege|beweg|bewege)\s+(.+?)\s+(?:over\s+|rüber\s+)?(?:to|onto|on|into|in|at|auf|nach|zu|zum|zur|an)\s+(.+?)(?:\s+(?:hin|rüber))?$").expect("move regex")
});
static CENTRE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:centre|center|zentrier|zentriere|zentrieren)\s+(.+?)(?:\s+on\s+(?:the|my)\s+screen)?$").expect("centre regex"));
static FULLSCREEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:full ?screen|go full ?screen (?:with|on))\s+(.+)$|^(?:make|put|set|turn|switch|bring|show|open|display|mach|mache|stell|stelle|zeig|zeige|schalt|schalte)\s+(.+?)\s+(?:to\s+|in\s+|into\s+|on\s+|auf\s+|im\s+|in den\s+)?(?:full ?screen|vollbild)(?:\s*modus|\s+mode)?(?:\s+an)?$").expect("fullscreen regex")
});
static EXIT_FULLSCREEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:take|bring|get|hol|hole)\s+(.+?)\s+(?:out of|aus dem)\s+(?:full ?screen|vollbild)(?:\s*modus|\s+mode)?(?:\s+raus)?$|^(?:exit|leave|end|stop|close)\s+(?:the\s+)?full ?screen(?:\s+mode)?\s+(?:on|for|of)\s+(.+)$").expect("exit fullscreen regex")
});
static MAX_MIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(maximi[sz]e|minimi[sz]e|restore|unminimi[sz]e|maximiere|minimiere)\s+(.+)$|^(?:make|mach|mache)\s+(.+?)\s+(maximi[sz]ed|minimi[sz]ed)$").expect("max/min regex")
});
static RESIZE_RELATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:make|set|turn|mach|mache)\s+(.+?)\s+((?:a bit\s+|a little\s+|slightly\s+|much\s+|way\s+|a lot\s+|etwas\s+|viel\s+)?(?:smaller|bigger|larger|kleiner|größer|groesser))$").expect("relative resize regex")
});
static RESIZE_TO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:resize|scale|size|make|set|change|bring)\s+(.+?)\s+(?:to\s+|down to\s+|up to\s+)(.+)$|^(?:mach|mache|stell|stelle)\s+(.+?)\s+(?:auf\s+)(.+?)(?:\s+groß)?$").expect("resize regex")
});
static SHRINK_GROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(shrink|grow|enlarge|expand|verkleiner|verkleinere|vergrößer|vergrößere|vergroesser|vergroessere)\s+(.+?)(?:\s+(?:a bit|a little|slightly|etwas))?$").expect("shrink regex")
});
static SIDE_BY_SIDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:put|place|show|arrange|move|set|lay out|stell|stelle|leg|lege|zeig|zeige|ordne)\s+(.+?)\s+(?:side by side|next to each other|beside each other|nebeneinander)(?:\s+an)?$|^(?:put|place|move|stell|stelle)\s+(.+?)\s+(?:next to|beside|neben)\s+(.+)$").expect("side by side regex")
});
/// "Dock it back", "dock the 3D model", "put it back in the HUD", "attach it back".
static DOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:re-?dock|dock)\s+(.+?)(?:\s+(?:back|again|wieder))?(?:\s+(?:in|into|inside|onto|on|to)(?:\s+(?:the\s+)?(?:hud|board|main window|app|yourself|you)))?(?:\s+an)?$|^attach\s+(.+?)\s+back(?:\s+(?:in|into|inside|onto|on|to)(?:\s+(?:the\s+)?(?:hud|board|main window|app|yourself|you))?)?$|^(?:put|bring|move|pull)\s+(.+?)\s+back\s+(?:in|into|inside|onto|on|to)\s+(?:the\s+)?(?:hud|board|main window|app|yourself|you)$").expect("dock regex")
});
static SWAP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:swap|exchange|switch around|tausch|tausche|vertausch|vertausche)\s+(.+?)(?:\s+(?:around|places|positions|die plätze|die positionen|um))?$").expect("swap regex")
});

/// The arguments of `arrange_jarvis_window` for a sentence that says to move or size one of his
/// views, or None (nothing for this tool: an app, himself, or a phrase another rule has).
pub fn parse(text: &str) -> Option<Value> {
    let s = crate::intents::normalize_phrase(text);
    if s.is_empty() || s.split_whitespace().count() > 14 {
        return None;
    }
    let s = s.replace("full-screen", "full screen").replace("fullscreen", "full screen");
    let s = s.as_str();
    let call = |target: &Target, action: Action, extra: Value| -> Option<Value> {
        let mut call = json!({ "action": action.name() });
        match target {
            Target::It => call["target"] = json!("it"),
            Target::Them => call["target"] = json!("them"),
            Target::Named(name) => call["target"] = json!(name),
            Target::Pair(a, b) => {
                call["target"] = json!(a);
                call["other"] = json!(b);
            }
        }
        for (k, v) in extra.as_object()? {
            call[k] = v.clone();
        }
        Some(call)
    };
    // A pair or "them": side by side, swapped.
    if let Some(c) = SIDE_BY_SIDE.captures(s) {
        let target = match (c.get(1), c.get(2), c.get(3)) {
            (Some(both), _, _) => target_of(both.as_str())?,
            (None, Some(a), Some(b)) => Target::Pair(view_name(a.as_str())?, view_name(b.as_str())?),
            _ => return None,
        };
        return matches!(target, Target::Them | Target::Pair(..)).then(|| call(&target, Action::SideBySide, json!({}))).flatten();
    }
    if let Some(c) = SWAP.captures(s) {
        let target = target_of(&c[1])?;
        return matches!(target, Target::Them | Target::Pair(..)).then(|| call(&target, Action::Swap, json!({}))).flatten();
    }
    if let Some(c) = DOCK.captures(s) {
        let target = target_of(c.get(1).or(c.get(2)).or(c.get(3))?.as_str())?;
        return matches!(target, Target::It | Target::Named(_)).then(|| call(&target, Action::Dock, json!({}))).flatten();
    }
    // Full screen, by name ("make it full screen" keeps its rule; the tools hand it over).
    if let Some(c) = FULLSCREEN.captures(s) {
        let o = c.get(1).or(c.get(2))?.as_str();
        return match target_of(o)? {
            target @ Target::Named(_) => call(&target, Action::Fullscreen, json!({})),
            _ => None,
        };
    }
    if let Some(c) = EXIT_FULLSCREEN.captures(s) {
        let o = c.get(1).or(c.get(2))?.as_str();
        return match target_of(o)? {
            target @ Target::Named(_) => call(&target, Action::ExitFullscreen, json!({})),
            _ => None,
        };
    }
    if let Some(c) = MAX_MIN.captures(s) {
        let (verb, o) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(v), Some(o), _, _) => (v.as_str(), o.as_str()),
            (_, _, Some(o), Some(v)) => (v.as_str(), o.as_str()),
            _ => return None,
        };
        let action = if verb.starts_with("maxim") {
            Action::Maximize
        } else if verb.starts_with("minim") {
            Action::Minimize
        } else {
            Action::Restore
        };
        return match target_of(o)? {
            target @ Target::Named(_) => call(&target, action, json!({})),
            _ => None,
        };
    }
    if let Some(c) = CENTRE.captures(s) {
        let target = target_of(&c[1])?;
        return matches!(target, Target::It | Target::Named(_)).then(|| call(&target, Action::Move, json!({ "position": "centre" }))).flatten();
    }
    if let Some(c) = SHRINK_GROW.captures(s) {
        let target = target_of(&c[2])?;
        // (With a board block in the spotlight, "shrink it" is the board's.)
        if target == Target::It && crate::hud_board::spotlight_active() {
            return None;
        }
        let grow = !c[1].starts_with("shrink") && !c[1].starts_with("verklein");
        return matches!(target, Target::It | Target::Named(_)).then(|| call(&target, Action::Resize, json!({ "size": if grow { "bigger" } else { "smaller" } }))).flatten();
    }
    if let Some(c) = RESIZE_RELATIVE.captures(s) {
        // "Make it bigger" keeps its rule (the tools hand it over); a view by name is this tool's.
        let target = target_of(&c[1])?;
        let size = Size::parse(&c[2])?;
        return match target {
            Target::Named(_) => call(&target, Action::Resize, json!({ "size": size.name() })),
            _ => None,
        };
    }
    if let Some(c) = RESIZE_TO.captures(s) {
        let (o, how) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(o), Some(h), _, _) | (_, _, Some(o), Some(h)) => (o.as_str(), h.as_str()),
            _ => return None,
        };
        if let Some(size) = Size::parse(how) {
            let target = target_of(o)?;
            // "Resize it to 800 by 500" keeps its rule; half the screen or a share of it is new,
            // said as a size ("resize it to 60%", "make it half the screen"): "set it to 30
            // percent" is the volume.
            let sized = ["resize ", "scale ", "size "].iter().any(|v| s.starts_with(v)) || how.contains("screen") || how.contains("bildschirm");
            let new_for_it = matches!(size, Size::Half | Size::Percent(_)) && sized;
            return match target {
                Target::Named(_) => call(&target, Action::Resize, json!({ "size": size.name() })),
                Target::It if new_for_it => call(&target, Action::Resize, json!({ "size": size.name() })),
                _ => None,
            };
        }
    }
    if let Some(c) = MOVE.captures(s) {
        let (o, place) = (c[1].trim(), c[2].trim());
        let target = target_of(o)?;
        if let Some(monitor) = monitor_of(place) {
            // ("Move it to my second monitor" keeps its rule: the tools hand it over.)
            return match target {
                Target::Named(_) => call(&target, Action::ToMonitor, json!({ "monitor": monitor })),
                _ => None,
            };
        }
        let position = Position::parse(place)?;
        return match target {
            Target::Named(_) => call(&target, Action::Move, json!({ "position": position.name() })),
            // ("Move it to the left" is Windows' snap for an app; the middle, a corner or a third
            // has no rule of its own.)
            Target::It if !position.is_side() => call(&target, Action::Move, json!({ "position": position.name() })),
            _ => None,
        };
    }
    None
}

// ---------------------------------------------------------------------------- the tool ----

pub fn is_tool(name: &str) -> bool {
    name == "arrange_jarvis_window"
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": "arrange_jarvis_window",
        "description": "Move, size, full-screen, minimise, restore, detach or dock one of YOUR OWN views (Earth, radar, disk map, Feature Lab, a simulation, a window you opened): \"full screen the Earth\", \"move it to the middle\". target: its name, \"it\" (the latest) or \"them\". Not Sir's apps (window_management), not yourself.",
        "parameters": {"type": "object", "properties": {
            "target": {"type": "string"},
            "action": {"type": "string", "enum": ["move", "resize", "fullscreen", "exit_fullscreen", "maximize", "restore", "minimize", "detach", "dock", "to_monitor", "side_by_side", "swap"]},
            "position": {"type": "string", "description": "centre, left, right, top, bottom, top_left …, left_third, right_two_thirds"},
            "size": {"type": "string", "description": "bigger, smaller, half, 60%, 900x600"},
            "monitor": {"type": "string", "description": "1, 2, left, right"},
            "other": {"type": "string", "description": "the second view (side_by_side, swap)"}
        }, "required": ["action"]}
    }})]
}

/// How Sir hears a view's name ("the Earth", "the network radar", "the Feature Lab").
pub fn spoken_name(id: &str) -> String {
    match id {
        "earth" => "the Earth".into(),
        "network_radar" => "the network radar".into(),
        "disk_map" => "the disk map".into(),
        "features" => "the Feature Lab".into(),
        "audio_mixer" => "the mixer".into(),
        "theme_studio" => "the Theme Studio".into(),
        "discord_audio" => "the Discord routing".into(),
        "pairing" => "the pairing code".into(),
        "system_telemetry" => "your telemetry".into(),
        "model3d" => "the 3D model".into(),
        "image_viewer" => "the picture".into(),
        "webcams" => "the live camera".into(),
        "street_view" => "street view".into(),
        id if id.starts_with("sim:") => "the simulation".into(),
        other => format!("the {}", other.replace('_', " ")),
    }
}

/// The view a name means, open or not: its block id (a simulation's is only known once it is up:
/// `sim:`), and None when it is open now, else the `open_jarvis_view` name that opens it.
fn resolve_named(name: &str) -> Option<(String, Option<&'static str>)> {
    if let Some(id) = crate::hud_dock::find_open(name) {
        return Some((id, None));
    }
    use crate::jarvis_views::JarvisSubWindow as View;
    let view = View::parse(name)?;
    let id = match view {
        View::SystemStatsMonitor => "system_telemetry",
        View::MobileSatelliteQr => "pairing",
        View::SimulationLab => "sim:",
        other => other.id(),
    };
    Some((id.to_string(), Some(view.id())))
}

/// The views "them" means: the two of his opened, moved or arranged last.
fn latest_two() -> Option<(String, String)> {
    let recent = crate::hud_dock::recent_blocks();
    let mut ids = recent.into_iter().filter(|(_, age)| *age <= IT_TTL).map(|(id, _)| id);
    Some((ids.next()?, ids.next()?))
}

/// `arrange_jarvis_window`. The HUD carries it out in its next frame; the result ends with a
/// "Spoken:" line.
pub async fn run_tool(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let action = str_arg(args, &["action"]).and_then(Action::parse).ok_or("arrange_jarvis_window needs an action (move, resize, fullscreen, exit_fullscreen, maximize, restore, minimize, detach, dock, to_monitor, side_by_side, swap).")?;
    let position = str_arg(args, &["position", "where", "place"]).map(|p| Position::parse(p).ok_or(format!("'{p}' is not a place I know (centre, left, right, top, bottom, a corner, a third)."))).transpose()?;
    let size = str_arg(args, &["size"]).map(|s| Size::parse(s).ok_or(format!("'{s}' is not a size I know (bigger, smaller, half, 60%, 900x600)."))).transpose()?;
    let monitor = args.get("monitor").or_else(|| args.get("target_monitor")).filter(|m| !m.is_null()).cloned();
    if action == Action::ToMonitor && monitor.is_none() {
        return Err("to_monitor needs a monitor (1, 2, left, right, other).".into());
    }
    if !crate::gui::windows_supported() {
        return Err("Jarvis is running without a display (headless), so he has no windows to arrange.".into());
    }
    let raw = str_arg(args, &["target", "view", "name", "id"]).unwrap_or("it").to_string();
    let lower = raw.to_lowercase();
    if names_himself(&lower) {
        return Err("arrange_jarvis_window arranges my views; my own HUD moves with configure_interface (\"full screen yourself\").".into());
    }
    // "Exit full screen": whichever of his views fills a screen.
    let raw = if lower == "fullscreen" || (action == Action::ExitFullscreen && is_it(&lower)) { fullscreen_view().unwrap_or(raw) } else { raw };
    let lower = raw.to_lowercase();
    // Two views: side by side, swapped.
    let pair = matches!(action, Action::SideBySide | Action::Swap);
    let (id, other, opening) = if is_them(&lower) || (pair && is_it(&lower) && str_arg(args, &["other"]).is_none()) {
        match latest_two() {
            Some((a, b)) => (a, Some(b), None),
            // ("Put them side by side" with fewer than two of his views up: Sir's own windows.)
            None if action == Action::SideBySide => return Box::pin(forward(ctx, "tile_windows", json!({ "layout": "side_by_side" }))).await,
            None => return Err("Fewer than two of my views are open, Sir.".into()),
        }
    } else if is_it(&lower) {
        match it(ctx, IT_TTL) {
            Some(id) => (id, None, None),
            // "It" is the app or website the conversation is about: the window tools' business.
            None => return Box::pin(app_instead(ctx, action, position, size, monitor)).await,
        }
    } else {
        let (id, opens) = resolve_named(&raw).ok_or_else(|| format!("I have no view called '{raw}', Sir."))?;
        let other = match str_arg(args, &["other"]) {
            Some(o) => Some(resolve_named(o).filter(|(_, opens)| opens.is_none()).map(|(id, _)| id).ok_or_else(|| format!("'{o}' is not open, Sir."))?),
            None if pair => Some(crate::hud_dock::recent_blocks().into_iter().map(|(other, _)| other).find(|other| *other != id).ok_or("Only one of my views is open, Sir.")?),
            None => None,
        };
        (id, other, opens)
    };
    // A panel that is not up yet opens first (its arrangement waits for it).
    if let Some(view) = opening {
        if matches!(action, Action::Minimize | Action::ExitFullscreen | Action::Restore | Action::Dock) {
            return Err(format!("{} is not open, Sir.", capitalised(&spoken_name(&id))));
        }
        let opened = Box::pin(ctx.execute("open_jarvis_view", &json!({ "view": view }))).await;
        if !opened.success {
            return Err(format!("I could not open {}: {}", spoken_name(&id), opened.output.trim_start_matches("Error: ")));
        }
    }
    // (A window of the window engine: "close it" means it now, as for one it just opened.)
    if ctx.widgets.find_window_named(&id).is_some() {
        ctx.focus.note_jarvis_window(&id);
    }
    crate::hud_dock::touch(&id);
    let wait = if opening.is_some() { OPENING_WAIT } else { NEXT_FRAME_WAIT };
    queue(Arrangement { id: id.clone(), action, position, size, monitor: monitor.clone(), other: other.clone(), deadline: Instant::now() + wait });
    let name = spoken_name(&id);
    let spoken = match action {
        Action::Move => format!("{} is {}, Sir.", capitalised(&name), position.unwrap_or(Position::Centre).spoken()),
        Action::Resize => format!("{} is {} now, Sir.", capitalised(&name), size.unwrap_or(Size::Bigger).spoken()),
        Action::Fullscreen => format!("{} fills the screen, Sir. Escape brings it back.", capitalised(&name)),
        Action::ExitFullscreen => format!("{} is back where it was, Sir.", capitalised(&name)),
        Action::Maximize => format!("{} is maximised, Sir.", capitalised(&name)),
        Action::Restore => format!("{} is restored, Sir.", capitalised(&name)),
        Action::Minimize => format!("{} is put away, Sir. Say \"restore it\" to bring it back.", capitalised(&name)),
        Action::Detach => format!("{} has a window of its own now, Sir.", capitalised(&name)),
        Action::Dock => format!("{} is back on your HUD, Sir.", capitalised(&name)),
        Action::ToMonitor => format!("{} is on monitor {}, Sir.", capitalised(&name), monitor.as_ref().map_or_else(String::new, |m| m.as_str().map_or_else(|| m.to_string(), str::to_string))),
        Action::SideBySide => format!("{} and {} are side by side, Sir.", capitalised(&name), other.as_deref().map_or_else(String::new, spoken_name)),
        Action::Swap => format!("{} and {} have swapped places, Sir.", capitalised(&name), other.as_deref().map_or_else(String::new, spoken_name)),
    };
    let detail = [position.map(|p| format!("position {}", p.name())), size.map(|s| format!("size {}", s.name())), other.as_ref().map(|o| format!("with '{o}'"))].into_iter().flatten().collect::<Vec<_>>().join(", ");
    Ok(format!("Arranging '{id}': {}{}{}.\nSpoken: {spoken}", action.name(), if detail.is_empty() { "" } else { " " }, detail))
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |first| first.to_uppercase().chain(c).collect())
}

/// "It" is the app or website the conversation is about: the same request for the window tools.
async fn app_instead(ctx: &ToolContext, action: Action, position: Option<Position>, size: Option<Size>, monitor: Option<Value>) -> Result<String, String> {
    let window = |a: &str| ("window_management", json!({ "action": a }));
    let (tool, args) = match action {
        Action::Fullscreen => window("fullscreen"),
        Action::ExitFullscreen => window("exit_fullscreen"),
        Action::Maximize => window("maximize"),
        Action::Minimize => window("minimize"),
        Action::Restore => window("restore"),
        Action::ToMonitor => ("move_window_to_monitor", json!({ "app_or_window_name": "it", "target_monitor": monitor })),
        Action::Resize => match size {
            Some(Size::Smaller | Size::MuchSmaller) => window("smaller"),
            Some(Size::Bigger | Size::MuchBigger) => window("bigger"),
            _ => return Err("None of my views is open to resize, Sir, and I can only make an app's window bigger or smaller.".into()),
        },
        Action::Move => match position {
            Some(Position::Left) => window("snap_left"),
            Some(Position::Right) => window("snap_right"),
            Some(Position::Top) => window("snap_top"),
            Some(Position::Bottom) => window("snap_bottom"),
            _ => return Err("None of my views is open to move, Sir, and I can only snap an app's window to a side of the screen.".into()),
        },
        Action::Detach | Action::Dock | Action::SideBySide | Action::Swap => return Err("None of my views is open, Sir.".into()),
    };
    Box::pin(forward(ctx, tool, args)).await
}

/// The same request for another tool, its result as this tool's.
async fn forward(ctx: &ToolContext, tool: &str, args: Value) -> Result<String, String> {
    let outcome = Box::pin(ctx.execute(tool, &args)).await;
    if outcome.success {
        Ok(outcome.output)
    } else {
        Err(outcome.output.trim_start_matches("Error: ").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_max(pos2(8.0, 44.0), pos2(1912.0, 1036.0))
    }

    #[test]
    fn places_and_sizes_land_inside_the_screen() {
        let a = area();
        let block = Rect::from_min_size(pos2(40.0, 80.0), vec2(800.0, 500.0));
        let centre = place(a, block, Position::Centre);
        assert_eq!(centre.size(), block.size());
        assert!((centre.center() - a.center()).length() < 0.01);
        assert_eq!(place(a, block, Position::Left), Rect::from_min_max(a.min, pos2(a.center().x, a.max.y)));
        assert_eq!(place(a, block, Position::BottomRight), Rect::from_min_max(a.center(), a.max));
        let thirds = [Position::LeftThird, Position::CentreThird, Position::RightThird].map(|p| place(a, block, p));
        assert!((thirds[0].width() - a.width() / 3.0).abs() < 0.01 && (thirds[1].left() - thirds[0].right()).abs() < 0.01 && (thirds[2].right() - a.right()).abs() < 0.01);
        for p in Position::ALL {
            let r = target_rect(a, block, Some(p), None);
            assert!(a.expand(0.01).contains_rect(r), "{p:?}: {r:?}");
            assert_eq!(Position::parse(p.name()), Some(p), "the tool's own words read back");
        }
        // Bigger and smaller about the middle, never past the screen, never below a block.
        let bigger = resize(a, block, Size::Bigger);
        assert!((bigger.center() - block.center()).length() < 0.01 || !a.contains_rect(Rect::from_center_size(block.center(), block.size() * 1.25)));
        assert!(bigger.width() > block.width() && a.contains_rect(bigger));
        let tiny = (0..20).fold(block, |r, _| resize(a, r, Size::Smaller));
        assert!(tiny.width() >= crate::hud_dock::MIN_BLOCK[0] - 0.01 && tiny.height() >= crate::hud_dock::MIN_BLOCK[1] - 0.01, "{tiny:?}");
        let huge = (0..20).fold(block, |r, _| resize(a, r, Size::MuchBigger));
        assert!(a.expand(0.01).contains_rect(huge) && huge.width() > a.width() - 1.0);
        // Half the screen: the half its middle is on.
        assert_eq!(resize(a, block, Size::Half), place(a, block, Position::Left));
        let right = block.translate(vec2(1200.0, 0.0));
        assert_eq!(resize(a, right, Size::Half), place(a, right, Position::Right));
        let sixty = resize(a, block, Size::Percent(60.0));
        assert!((sixty.width() - a.width() * 0.6).abs() < 0.01 && a.contains_rect(sixty));
        assert_eq!(resize(a, block, Size::Exact(900.0, 600.0)).size(), vec2(900.0, 600.0));
        let (l, r) = side_by_side(a);
        assert!(!l.intersects(r) && (r.left() - l.right() - PAIR_GAP).abs() < 0.01 && l.left() == a.left() && r.right() == a.right());
        // A window at the edge comes back inside whole.
        let off = Rect::from_min_size(pos2(1700.0, 900.0), vec2(800.0, 500.0));
        assert!(a.contains_rect(fit(off, a)));
    }

    #[test]
    fn the_words_for_places_and_sizes() {
        for (said, p) in [
            ("the middle of my screen", Position::Centre),
            ("middle", Position::Centre),
            ("the center of the screen", Position::Centre),
            ("die Mitte", Position::Centre),
            ("left half", Position::Left),
            ("the left side", Position::Left),
            ("die linke Seite", Position::Left),
            ("the top right corner", Position::TopRight),
            ("oben links", Position::TopLeft),
            ("the left third", Position::LeftThird),
            ("right two thirds", Position::RightTwoThirds),
        ] {
            assert_eq!(Position::parse(said), Some(p), "{said}");
        }
        assert_eq!(Position::parse("monitor 2"), None);
        for (said, size) in [("bigger", Size::Bigger), ("a bit smaller", Size::Smaller), ("much bigger", Size::MuchBigger), ("half the screen", Size::Half), ("half", Size::Half), ("60%", Size::Percent(60.0)), ("75 percent", Size::Percent(75.0)), ("900x600", Size::Exact(900.0, 600.0)), ("800 by 500", Size::Exact(800.0, 500.0)), ("größer", Size::Bigger), ("kleiner", Size::Smaller)] {
            assert_eq!(Size::parse(said), Some(size), "{said}");
        }
        assert_eq!(Size::parse("purple"), None);
        assert_eq!(monitor_of("my second monitor"), Some(json!("my second monitor")));
        assert_eq!(monitor_of("monitor 2"), Some(json!(2)));
        assert_eq!(monitor_of("den zweiten Bildschirm".to_lowercase().as_str()), Some(json!("den zweiten bildschirm")));
        assert_eq!(monitor_of("the middle"), None);
    }

    #[test]
    fn sentences_that_arrange_his_views() {
        let p = |text: &str| parse(text);
        let args = |target: &str, action: &str, extra: Value| {
            let mut v = json!({ "target": target, "action": action });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            Some(v)
        };
        // The two sentences from the log that had no tool to call.
        assert_eq!(p("Move it to the middle of my screen."), args("it", "move", json!({ "position": "centre" })));
        // A view of its own window comes back on the board ("dock it back" reached the brain).
        for docked in ["dock it back", "Dock it.", "dock it back into the HUD", "put it back in the HUD", "attach it back", "redock it"] {
            assert_eq!(p(docked), args("it", "dock", json!({})), "{docked}");
        }
        assert_eq!(p("dock the earth back"), args("earth", "dock", json!({})));
        // ("Put it back" alone may be the model's colours or a moved block's place.)
        assert_ne!(p("put it back"), args("it", "dock", json!({})));
        assert_eq!(p("No Jarvis, can you full screen the Earth?"), args("earth", "fullscreen", json!({})));
        assert_eq!(p("Centre the Earth"), args("earth", "move", json!({ "position": "centre" })));
        assert_eq!(p("Put the earth on the left half"), args("earth", "move", json!({ "position": "left" })));
        assert_eq!(p("Full screen the Earth"), args("earth", "fullscreen", json!({})));
        assert_eq!(p("Make the Earth full screen"), args("earth", "fullscreen", json!({})));
        assert_eq!(p("Move the radar to my second monitor"), args("radar", "to_monitor", json!({ "monitor": "my second monitor" })));
        assert_eq!(p("Move the network radar to monitor 2"), args("network radar", "to_monitor", json!({ "monitor": 2 })));
        assert_eq!(p("Put them side by side"), args("them", "side_by_side", json!({})));
        assert_eq!(p("Put the earth and the radar side by side"), Some(json!({ "target": "earth", "other": "radar", "action": "side_by_side" })));
        assert_eq!(p("Swap them"), args("them", "swap", json!({})));
        assert_eq!(p("Resize it to half the screen"), args("it", "resize", json!({ "size": "half" })));
        assert_eq!(p("Resize it to 60 percent"), args("it", "resize", json!({ "size": "60%" })));
        assert_eq!(p("Make the disk map bigger"), args("disk map", "resize", json!({ "size": "bigger" })));
        assert_eq!(p("Resize the earth to 900 by 600"), args("earth", "resize", json!({ "size": "900x600" })));
        assert_eq!(p("Move it to the top left corner"), args("it", "move", json!({ "position": "top_left" })));
        assert_eq!(p("Minimise the earth"), args("earth", "minimize", json!({})));
        assert_eq!(p("Restore the radar"), args("radar", "restore", json!({})));
        assert_eq!(p("Take the earth out of full screen"), args("earth", "exit_fullscreen", json!({})));
        assert_eq!(p("Verschieb es in die Mitte"), args("it", "move", json!({ "position": "centre" })));
        assert_eq!(p("Zentriere die Erde"), args("erde", "move", json!({ "position": "centre" })));
        assert_eq!(p("Mach die Erde größer"), args("erde", "resize", json!({ "size": "bigger" })));
        assert_eq!(p("Mach die Erde im Vollbild"), args("erde", "fullscreen", json!({})));
        if !crate::hud_board::spotlight_active() {
            assert_eq!(p("Shrink it"), args("it", "resize", json!({ "size": "smaller" })));
        }
        // What other rules keep: "it" to a side, full screen, bigger, a monitor; an app; himself.
        for keep in [
            "Move it to the left",
            "Make it full screen",
            "Make it bigger",
            "Mach es größer",
            "Vollbild",
            "Exit full screen",
            "Minimise it",
            "Restore it",
            "Move it to my second monitor",
            "Resize it to 800 by 500",
            "Make the drive space window smaller",
            "Move Discord to the middle",
            "Full screen Spotify",
            "Make Firefox bigger",
            "Full screen yourself",
            "Move yourself to the left",
            "Maximize Jarvis",
            "Make the HUD full screen",
            "Put the board on the left",
            "Move the orb to the middle",
            "What is in the middle of the earth",
            // The volume.
            "Set it to 30 percent",
            "Set it to half",
            "Bring it down to 20%",
            "Stell es auf 40 Prozent",
        ] {
            assert_eq!(p(keep), None, "{keep}");
        }
    }

    #[test]
    fn it_means_his_view_only_while_that_is_newer_than_the_app() {
        let view = |s: u64| Some(("earth".to_string(), Duration::from_secs(s)));
        assert_eq!(it_is_view(view(5), None, IT_TTL).as_deref(), Some("earth"));
        assert_eq!(it_is_view(view(5), Some(Duration::from_secs(60)), IT_TTL).as_deref(), Some("earth"), "opened after the app");
        assert_eq!(it_is_view(view(60), Some(Duration::from_secs(5)), IT_TTL), None, "the app came up since");
        assert_eq!(it_is_view(view(20 * 60), None, IT_TTL), None, "too long ago");
        // A command that could mean an app takes the view over for a shorter while.
        assert_eq!(it_is_view(view(10 * 60), None, IT_TTL).as_deref(), Some("earth"));
        assert_eq!(it_is_view(view(10 * 60), None, TAKE_OVER_TTL), None);
        assert_eq!(it_is_view(None, None, IT_TTL), None);
    }

    #[test]
    fn window_commands_go_to_his_view_when_it_is_it() {
        let wm = |action: &str| json!({ "action": action });
        assert_eq!(redirect("window_management", &wm("fullscreen"), true, false, false), Some(json!({ "target": "it", "action": "fullscreen" })));
        assert_eq!(redirect("window_management", &wm("snap_left"), true, false, false), Some(json!({ "target": "it", "action": "move", "position": "left" })));
        assert_eq!(redirect("window_management", &wm("minimize"), true, false, false), Some(json!({ "target": "it", "action": "minimize" })));
        assert_eq!(redirect("window_management", &wm("fullscreen"), false, false, false), None, "an app is it");
        assert_eq!(redirect("window_management", &json!({ "action": "fullscreen", "target_window": "Discord" }), true, false, false), None, "named");
        assert_eq!(redirect("window_management", &wm("close"), true, false, false), None);
        // "Exit full screen" goes to the view filling a screen, whatever "it" is (never the HUD).
        assert_eq!(redirect("window_management", &wm("exit_fullscreen"), false, true, false), Some(json!({ "target": "fullscreen", "action": "exit_fullscreen" })));
        assert_eq!(redirect("window_management", &wm("exit_fullscreen"), false, false, false), None);
        assert_eq!(redirect("update_native_window", &json!({ "id": "focus", "size": "bigger" }), true, false, false), Some(json!({ "target": "it", "action": "resize", "size": "bigger" })));
        assert_eq!(redirect("update_native_window", &json!({ "id": "focus", "width": 800, "height": 500 }), true, false, false), Some(json!({ "target": "it", "action": "resize", "size": "800x500" })));
        assert_eq!(redirect("update_native_window", &json!({ "id": "focus", "size": "bigger" }), true, false, true), None, "a window of the window engine sizes itself");
        assert_eq!(redirect("update_native_window", &json!({ "id": "focus", "add_content": ["cpu"] }), true, false, false), None);
        assert_eq!(redirect("move_window_to_monitor", &json!({ "app_or_window_name": "it", "target_monitor": 2 }), true, false, false), Some(json!({ "target": "it", "action": "to_monitor", "monitor": 2 })));
        assert_eq!(redirect("move_window_to_monitor", &json!({ "app_or_window_name": "jarvis", "target_monitor": 2 }), true, false, false), None);
    }
}
