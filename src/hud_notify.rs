//! Notifications as Sir chose them (2026-11-12): one stack at the top right of the HUD (cards
//! slide in, stack, and leave after a few seconds), a history of the last hours that "what did I
//! miss?" brings back, and the edge light: the screen's edges glow for a moment when something
//! happens, so it is noticed without a sound.

use chrono::{DateTime, Local};
use eframe::egui::{self, pos2, vec2, Color32, FontId, Rect, Rounding};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

use crate::hud_style::stroke;

// ------------------------------------------------------------------------------ history ----

/// How many notifications the history keeps, and for how long one counts as recent.
const KEPT: usize = 60;
const RECENT_HOURS: i64 = 12;

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub text: String,
    pub alert: bool,
    pub at: DateTime<Local>,
}

static HISTORY: Mutex<VecDeque<Note>> = Mutex::new(VecDeque::new());

/// A notification was shown: it goes into the history, and the edges light up.
pub fn record(text: &str, alert: bool) {
    load_once();
    record_at(text, alert, Local::now());
    edge(if alert { Edge::Alert } else { Edge::Notice });
    keep();
}

/// The history survives a restart: it is read from the kit's store once (what is still news),
/// and written there after each change, off the caller's thread. A test keeps to its own lines.
fn load_once() {
    static LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if cfg!(test) || LOADED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let Some(serde_json::Value::Array(kept)) = crate::kit::get("notifications", "history") else { return };
    let now = Local::now();
    let mut history = lock(&HISTORY);
    let mut before: VecDeque<Note> = kept
        .iter()
        .filter_map(|n| {
            let at = DateTime::parse_from_rfc3339(n["at"].as_str()?).ok()?.with_timezone(&Local);
            Some(Note { text: n["text"].as_str()?.to_string(), alert: n["alert"].as_bool().unwrap_or(false), at })
        })
        .filter(|n| (now - n.at).num_hours() < RECENT_HOURS)
        .collect();
    before.extend(history.drain(..));
    while before.len() > KEPT {
        before.pop_front();
    }
    *history = before;
}

fn keep() {
    if cfg!(test) {
        return;
    }
    let notes: Vec<serde_json::Value> = lock(&HISTORY).iter().map(|n| serde_json::json!({ "text": n.text, "alert": n.alert, "at": n.at.to_rfc3339() })).collect();
    let _ = std::thread::Builder::new().name("jai-notes".into()).spawn(move || {
        let _ = crate::kit::put("notifications", "history", &serde_json::Value::Array(notes));
    });
}

/// "Clear my notifications": the history is empty from now on.
pub fn clear() -> usize {
    let n = {
        let mut history = lock(&HISTORY);
        let n = history.len();
        history.clear();
        n
    };
    keep();
    n
}

/// The kit's phrase for it.
pub fn command(s: &str) -> Option<serde_json::Value> {
    static CLEAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:clear|delete|empty|forget)\s+(?:my\s+|the\s+|all\s+(?:my\s+|the\s+)?)?(?:notifications|notification\s+history)$|^(?:lösche?|leere?)\s+(?:die\s+|meine\s+|alle\s+)?benachrichtigungen$|^benachrichtigungen\s+(?:löschen|leeren)$").expect("valid regex")
    });
    CLEAR.is_match(s.trim()).then(|| serde_json::json!({ "action": "clear" }))
}

pub fn run(_args: &serde_json::Value) -> Result<String, String> {
    let n = clear();
    Ok(crate::kit::say(format!("Cleared {n} notification{} from the history.", if n == 1 { "" } else { "s" }), if n == 0 { "There was nothing in it, Sir." } else { "Cleared, Sir." }))
}

fn record_at(text: &str, alert: bool, at: DateTime<Local>) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let mut history = lock(&HISTORY);
    // The same line again within a minute is one notification.
    if history.back().is_some_and(|last| last.text == text && (at - last.at).num_seconds().abs() < 60) {
        return;
    }
    history.push_back(Note { text: text.to_string(), alert, at });
    while history.len() > KEPT {
        history.pop_front();
    }
}

/// The notifications of the last hours, the latest first.
pub fn recent(now: DateTime<Local>) -> Vec<Note> {
    load_once();
    lock(&HISTORY).iter().rev().filter(|n| (now - n.at).num_hours() < RECENT_HOURS).cloned().collect()
}

/// "12 s ago", "5 min ago", "2 h ago".
fn ago(secs: i64) -> String {
    match secs {
        i64::MIN..=59 => format!("{} s ago", secs.max(0)),
        60..=3599 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    }
}

/// Within this long, notifications from one source stand together.
const GROUP_SECS: i64 = 300;
/// A grouped card shows this many of its newest lines.
const GROUP_LINES: usize = 3;

/// Who a notification is from, how many it stands for, and what it says: its words before the
/// first colon ("Discord", "WhatsApp from Max"; "Discord (3)" is three). None: a line without
/// such a head.
fn source(text: &str) -> Option<(&str, u32, &str)> {
    let (head, rest) = text.split_once(": ")?;
    if head.is_empty() || head.chars().count() > 36 || head.contains('\n') || rest.trim().is_empty() {
        return None;
    }
    Some(match head.rsplit_once(" (").and_then(|(name, n)| Some((name, n.strip_suffix(')')?.parse::<u32>().ok()?))) {
        Some((name, n)) => (name, n, rest),
        None => (head, 1, rest),
    })
}

/// A notice that follows another from the same source joins it: the source, how many there
/// were, and its newest lines (newest first). None: they are from different sources.
pub fn grouped(shown: &str, new: &str) -> Option<String> {
    let (was, count, lines) = source(shown)?;
    let (is, _, line) = source(new)?;
    if !was.eq_ignore_ascii_case(is) {
        return None;
    }
    let line = line.trim();
    let mut newest = vec![line];
    newest.extend(lines.lines().map(str::trim).filter(|l| !l.is_empty() && *l != line));
    newest.truncate(GROUP_LINES);
    Some(format!("{is} ({}): {}", count + 1, newest.join("\n")))
}

/// The history as the "what did I miss?" block shows it: newest first, and what one source
/// sent in a row (five messages from one chat) as one entry with its lines.
pub fn missed_text(now: DateTime<Local>) -> String {
    let notes = recent(now);
    if notes.is_empty() {
        return "Nothing came up in the last twelve hours, Sir.".to_string();
    }
    // (the newest note of the entry, the oldest moment in it, further lines from the same source)
    let mut entries: Vec<(Note, DateTime<Local>, Vec<String>)> = Vec::new();
    for note in notes {
        let from = source(&note.text).map(|(name, _, rest)| (name.to_lowercase(), rest.trim().to_string()));
        if let (Some((name, rest)), Some((head, oldest, more))) = (&from, entries.last_mut()) {
            let same = source(&head.text).is_some_and(|(n, _, _)| n.to_lowercase() == *name);
            if same && head.alert == note.alert && (*oldest - note.at).num_seconds().abs() < GROUP_SECS {
                more.push(rest.clone());
                *oldest = note.at;
                continue;
            }
        }
        let at = note.at;
        entries.push((note, at, Vec::new()));
    }
    entries
        .iter()
        .map(|(n, _, more)| {
            let stamp = format!("{}  ·  {}{}", n.at.format("%H:%M"), ago((now - n.at).num_seconds()), if n.alert { "  ·  ALERT" } else { "" });
            match source(&n.text).filter(|_| !more.is_empty()) {
                Some((name, _, rest)) => format!("{stamp}\n{name} ({})\n  {}\n  {}", more.len() + 1, rest.trim(), more.join("\n  ")),
                None => format!("{stamp}\n{}", n.text),
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The "what did I miss?" block's id.
pub const MISSED_ID: &str = "missed";
static MISSED_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Draws the history block while it is open ("what did I miss?" opens it).
pub fn show(ctx: &egui::Context) {
    use std::sync::atomic::Ordering;
    if crate::hud_feel::take_missed() {
        MISSED_OPEN.store(true, Ordering::Release);
        crate::hud_dock::cancel_close(MISSED_ID);
    }
    if !MISSED_OPEN.load(Ordering::Acquire) {
        return;
    }
    // (The title says how many there are.)
    let count = recent(Local::now()).len();
    let title = match count {
        0 => "What you missed // nothing".to_string(),
        1 => "What you missed // 1 notification".to_string(),
        n => format!("What you missed // {n} notifications"),
    };
    if crate::gui::text_block(ctx, MISSED_ID, &title, [460.0, 420.0], &missed_text(Local::now())) {
        MISSED_OPEN.store(false, Ordering::Release);
        crate::hud_dock::forget(MISSED_ID);
    }
}

// -------------------------------------------------------------------------------- stack ----

/// A card slides in over this long and fades out over the same.
const SLIDE: f32 = 0.2;
const CARD_WIDTH: f32 = 360.0;

/// One card of the stack: its text, whether it is an alert, how long it has been up and how
/// long it still stays (seconds).
pub struct Card<'a> {
    pub text: &'a str,
    pub alert: bool,
    pub age: f32,
    pub left: f32,
}

/// How far in a card is (0 off screen … 1 in place) and how strong (0-1), from its age and the
/// time it has left.
pub fn card_motion(age: f32, left: f32) -> (f32, f32) {
    let enter = crate::hud_style::motion::settle((age / SLIDE).clamp(0.0, 1.0));
    let leave = (left / SLIDE).clamp(0.0, 1.0);
    (enter, leave.min((age / (SLIDE * 0.6)).clamp(0.0, 1.0)))
}

/// Paints the stack: the newest card at the top right of `area` (under the header strip), the
/// older ones below it. Returns whether anything moves.
pub fn paint_stack(ctx: &egui::Context, area: Rect, cards: &[Card<'_>]) -> bool {
    if cards.is_empty() {
        return false;
    }
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("hud_notifications")));
    let mut y = area.top();
    let mut moving = false;
    for card in cards.iter().rev() {
        let (enter, strength) = card_motion(card.age, card.left);
        moving |= card.age < SLIDE * 1.2 || card.left < SLIDE * 1.2;
        let edge = if card.alert { crate::hud_style::FAULT } else { crate::hud_board::border_colour() };
        let tag = if card.alert { "ALERT //" } else { "NOTICE //" };
        let galley = p.layout(card.text.to_string(), FontId::proportional(12.0), Color32::from_rgb(0xE8, 0xF2, 0xFC), CARD_WIDTH - 26.0);
        let height = galley.size().y + 34.0;
        // It unfolds downwards from its top edge as it comes, and folds up as it goes: the cards
        // under it make room and close the gap at the same pace, so no two ever overlap.
        let open = enter.min(1.0) * (card.left / SLIDE).clamp(0.0, 1.0);
        let rect = Rect::from_min_size(pos2(area.right() - CARD_WIDTH, y), vec2(CARD_WIDTH, height));
        let mut q = p.with_clip_rect(Rect::from_min_max(pos2(rect.left() - 40.0, y - 6.0), pos2(rect.right() + 40.0, y + (height + 8.0) * open + if open >= 1.0 { 30.0 } else { 0.0 })));
        q.set_opacity(strength);
        crate::hud_style::shadow(&q, rect, 0.8);
        q.rect_filled(rect, Rounding::ZERO, crate::hud_board::BLOCK_FILL);
        q.rect_stroke(rect.shrink(0.5), Rounding::ZERO, stroke(1.0, edge));
        // An alert carries a bar down its left edge.
        if card.alert {
            q.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), Rounding::ZERO, crate::hud_style::FAULT);
        }
        crate::hud_board::brackets(&q, rect.shrink(0.75), crate::hud_board::bracket_colour());
        q.text(rect.min + vec2(13.0, 9.0), egui::Align2::LEFT_TOP, tag, FontId::monospace(11.0), if card.alert { crate::hud_style::FAULT } else { Color32::from_rgb(0x7F, 0xA6, 0xB6) });
        q.galley(rect.min + vec2(13.0, 24.0), galley, Color32::PLACEHOLDER);
        // Its time runs out along the bottom edge.
        let total = (card.age + card.left).max(0.1);
        q.rect_filled(Rect::from_min_size(pos2(rect.left(), rect.bottom() - 1.5), vec2(rect.width() * (card.left / total).clamp(0.0, 1.0), 1.5)), Rounding::ZERO, edge.gamma_multiply(0.8));
        y += (height + 8.0) * open;
    }
    moving
}

// --------------------------------------------------------------------------- edge light ----

/// What lit the edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Something for Sir to see: the theme's colour.
    Notice,
    /// Something wrong: red.
    Alert,
}

static EDGE: Mutex<Option<(Edge, Instant)>> = Mutex::new(None);

/// The whole pulse: up quickly, held a moment, down slowly.
const EDGE_RISE: f32 = 0.12;
const EDGE_HOLD: f32 = 0.3;
const EDGE_FALL: f32 = 0.9;
/// How far the glow reaches into the screen (points).
const EDGE_REACH: f32 = 30.0;

/// The screen's edges light up once.
pub fn edge(kind: Edge) {
    let mut lit = lock(&EDGE);
    // An alert is never replaced by a notice that came in the same moment.
    if lit.is_none_or(|(now, at)| now != Edge::Alert || kind == Edge::Alert || at.elapsed().as_secs_f32() > EDGE_RISE + EDGE_HOLD) {
        *lit = Some((kind, Instant::now()));
    }
}

/// The pulse's strength `age` seconds after it was lit (0 … 1 … 0).
pub fn edge_strength(age: f32) -> f32 {
    if age < 0.0 {
        0.0
    } else if age < EDGE_RISE {
        crate::hud_style::motion::ease_out(age / EDGE_RISE)
    } else if age < EDGE_RISE + EDGE_HOLD {
        1.0
    } else {
        let t = ((age - EDGE_RISE - EDGE_HOLD) / EDGE_FALL).clamp(0.0, 1.0);
        (1.0 - t) * (1.0 - t)
    }
}

/// Paints the edge light on `screen` while a pulse runs (`dim`: night mode's share). Returns
/// whether it is lit.
pub fn paint_edge(ctx: &egui::Context, screen: Rect, dim: f32) -> bool {
    let Some((kind, at)) = *lock(&EDGE) else { return false };
    let age = at.elapsed().as_secs_f32();
    if age > EDGE_RISE + EDGE_HOLD + EDGE_FALL {
        *lock(&EDGE) = None;
        return false;
    }
    let strength = edge_strength(age) * dim;
    let colour = match kind {
        Edge::Notice => crate::hud_board::active_colour(),
        Edge::Alert => crate::hud_style::FAULT,
    };
    // (A glow, not a frame: soft at the very edge, gone a finger's width in.)
    let lit = colour.gamma_multiply(0.26 * strength);
    let line = colour.gamma_multiply(0.5 * strength);
    let clear = Color32::TRANSPARENT;
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("hud_edge_light")));
    let mut mesh = egui::Mesh::default();
    let inner = screen.shrink(EDGE_REACH);
    let outer = [screen.left_top(), screen.right_top(), screen.right_bottom(), screen.left_bottom()];
    let inside = [inner.left_top(), inner.right_top(), inner.right_bottom(), inner.left_bottom()];
    for at in outer {
        mesh.colored_vertex(at, lit);
    }
    for at in inside {
        mesh.colored_vertex(at, clear);
    }
    for k in 0..4u32 {
        let n = (k + 1) % 4;
        mesh.add_triangle(k, n, 4 + n);
        mesh.add_triangle(k, 4 + n, 4 + k);
    }
    p.add(egui::Shape::mesh(mesh));
    p.rect_stroke(screen.shrink(0.75), Rounding::ZERO, stroke(1.5, line));
    ctx.request_repaint();
    true
}

/// How long a notification's card stays up.
pub fn stays(alert: bool, night: bool) -> Duration {
    Duration::from_secs(match (alert, night) {
        (true, _) => 8,
        (false, true) => 4,
        (false, false) => 5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    static ONE: Mutex<()> = Mutex::new(());

    #[test]
    fn the_history_keeps_the_last_hours_and_counts_a_repeat_once() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        lock(&HISTORY).clear();
        let at = |h: u32, m: u32| Local.with_ymd_and_hms(2026, 11, 12, h, m, 0).unwrap();
        record_at("Timer done.", false, at(8, 0));
        record_at("Timer done.", false, at(8, 0));
        record_at("GPU at 91 °C.", true, at(9, 30));
        record_at("  ", false, at(9, 31));
        assert_eq!(recent(at(10, 0)).iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["GPU at 91 °C.", "Timer done."]);
        let text = missed_text(at(10, 0));
        assert!(text.starts_with("09:30  ·  30 min ago  ·  ALERT\nGPU at 91 °C."), "{text}");
        assert!(text.contains("08:00  ·  2 h ago\nTimer done."), "{text}");
        assert_eq!(recent(at(21, 0)).len(), 1, "after twelve hours a notification is no longer news");
        for k in 0..(KEPT + 20) {
            record_at(&format!("note {k}"), false, at(10, 0) + chrono::Duration::minutes(2 * k as i64));
        }
        assert_eq!(lock(&HISTORY).len(), KEPT);
        lock(&HISTORY).clear();
        assert_eq!(missed_text(at(10, 0)), "Nothing came up in the last twelve hours, Sir.");
        assert_eq!((ago(12), ago(300), ago(7300)), ("12 s ago".to_string(), "5 min ago".to_string(), "2 h ago".to_string()));
    }

    #[test]
    fn what_one_source_sends_in_a_row_is_one_entry() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        lock(&HISTORY).clear();
        let at = |h: u32, m: u32| Local.with_ymd_and_hms(2026, 11, 12, h, m, 0).unwrap();
        record_at("Discord: Alex is online", false, at(9, 0));
        record_at("Discord: Alex: are you there?", false, at(9, 1));
        record_at("Discord: Alex: hello?", false, at(9, 2));
        record_at("Timer done.", false, at(9, 3));
        record_at("Discord: Sam joined the call", false, at(9, 40));
        let text = missed_text(at(10, 0));
        assert!(text.contains("09:02  ·  58 min ago\nDiscord (3)\n  Alex: hello?\n  Alex: are you there?\n  Alex is online"), "{text}");
        assert!(text.contains("09:40  ·  20 min ago\nDiscord: Sam joined the call"), "a later one stands alone: {text}");
        assert!(text.contains("Timer done."), "{text}");
        lock(&HISTORY).clear();
        // On screen: the card of a source takes the next line from it and counts.
        let two = grouped("Discord: Alex is online", "Discord: Alex: are you there?").expect("same source");
        assert_eq!(two, "Discord (2): Alex: are you there?\nAlex is online");
        let four = grouped(&grouped(&two, "discord: third").expect("three"), "Discord: fourth").expect("four");
        assert_eq!(four, "Discord (4): fourth\nthird\nAlex: are you there?", "the three newest lines stay");
        assert_eq!(grouped("Discord: a", "WhatsApp from Alex: b"), None);
        assert_eq!(grouped("Timer done.", "Timer done."), None, "a line without a source joins nothing");
        assert_eq!(grouped("Discord: same", "Discord: same").as_deref(), Some("Discord (2): same"));
    }

    #[test]
    fn a_card_slides_in_stays_and_fades() {
        let (enter, strength) = card_motion(0.0, 5.0);
        assert!(enter < 0.05 && strength < 0.05);
        let (enter, strength) = card_motion(1.0, 4.0);
        assert_eq!((enter, strength), (1.0, 1.0));
        let (enter, strength) = card_motion(4.95, 0.05);
        assert!(enter == 1.0 && strength < 0.3, "leaving: {strength}");
        // The room a card takes grows with it and never exceeds its own height.
        let open = |age: f32, left: f32| card_motion(age, left).0.min(1.0) * (left / SLIDE).clamp(0.0, 1.0);
        assert!(open(0.0, 5.0) < 0.05 && open(0.1, 4.9) > 0.5 && open(1.0, 4.0) == 1.0 && open(4.95, 0.05) < 0.3);
        assert!(stays(true, false) > stays(false, false) && stays(false, true) <= stays(false, false));
    }

    #[test]
    fn the_edge_light_is_one_short_pulse_and_an_alert_holds_its_colour() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(edge_strength(0.0), 0.0);
        assert_eq!(edge_strength(EDGE_RISE + 0.1), 1.0);
        assert!(edge_strength(EDGE_RISE + EDGE_HOLD + EDGE_FALL * 0.5) < 0.3);
        assert_eq!(edge_strength(5.0), 0.0);
        assert!(EDGE_RISE + EDGE_HOLD + EDGE_FALL < 1.5, "a moment, not a light show");
        *lock(&EDGE) = None;
        edge(Edge::Alert);
        edge(Edge::Notice);
        assert_eq!(lock(&EDGE).map(|(kind, _)| kind), Some(Edge::Alert));
        *lock(&EDGE) = None;
    }
}
