//! Dynamic pop-up cards: the OLED HUD stays empty until something needs a picture, then a card
//! springs out beside the orb and folds away again.
//!
//! * `NowPlayingMediaCard` slides in when music starts or changes (previous / play-pause / next + volume) and
//!   collapses into a pill after 8 s; `VolumeMixerOsd` is a Dynamic-Island volume bar (3 s);
//!   schedule, tool-forge, confirmation and free-form data cards (`pop_up_ui_card`) follow.
//! * Every card animates from scale 0.88 / alpha 0 / +24 px to 1 / 1 / 0 on a spring
//!   (stiffness 220, damping 24), fades out when its TTL runs out (never while hovered or
//!   pinned), and goes with one click on the close button or "dismiss pop-ups".
//! * A card of the same kind updates in place (the volume OSD follows the slider, the media
//!   card follows the track) instead of stacking duplicates.

use eframe::egui::{self, emath::TSTransform, pos2, vec2, Color32, Id, Order, Rect, RichText, Rounding, Sense, Stroke};
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex, MutexGuard};

// -------------------------------------------------------------------------- physics ----

/// A spring towards `target`: `a = k·(target − x) − c·v`, integrated in ≤ 1/120 s steps so a
/// stalled frame never explodes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringDamper {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl SpringDamper {
    pub const STIFFNESS: f32 = 220.0;
    pub const DAMPING: f32 = 24.0;

    pub fn new(value: f32, target: f32) -> Self {
        Self { value, velocity: 0.0, target, stiffness: Self::STIFFNESS, damping: Self::DAMPING }
    }

    pub fn step(&mut self, dt: f32) {
        let mut left = dt.clamp(0.0, 0.25);
        while left > 0.0 {
            let h = left.min(1.0 / 120.0);
            let accel = self.stiffness * (self.target - self.value) - self.damping * self.velocity;
            self.velocity += accel * h;
            self.value += self.velocity * h;
            left -= h;
        }
    }

    pub fn settled(&self) -> bool {
        (self.target - self.value).abs() < 0.002 && self.velocity.abs() < 0.01
    }
}

// ------------------------------------------------------------------------- payloads ----

#[derive(Debug, Clone, PartialEq)]
pub enum PopupPayload {
    NowPlayingMediaCard { title: String, artist: String, playlist: Option<String>, player_name: String, progress: f32, is_playing: bool },
    VolumeMixerOsd { target_name: String, percent: u8, is_muted: bool },
    ScheduleAndCalendarCard { title: String, entries: Vec<(String, String)>, highlight_index: Option<usize> },
    ToolForgeLiveCard { tool_name: String, steps_summary: Vec<String>, status: String },
    QuickConfirmModal { title: String, description: String, confirm_action_id: String },
    VisualDataCard { title: String, markdown_or_code: String, chart_points: Vec<f32>, action_buttons: Vec<String> },
}

impl PopupPayload {
    /// Cards of one key replace each other.
    pub fn key(&self) -> String {
        match self {
            Self::NowPlayingMediaCard { .. } => "media".into(),
            Self::VolumeMixerOsd { target_name, .. } => format!("volume:{}", target_name.to_lowercase()),
            Self::ScheduleAndCalendarCard { .. } => "schedule".into(),
            Self::ToolForgeLiveCard { tool_name, .. } => format!("forge:{tool_name}"),
            Self::QuickConfirmModal { confirm_action_id, .. } => format!("confirm:{confirm_action_id}"),
            Self::VisualDataCard { title, .. } => format!("data:{}", title.to_lowercase()),
        }
    }

    /// Seconds before the card fades out on its own (`None`: until dismissed or answered).
    pub fn default_ttl(&self) -> Option<f32> {
        match self {
            Self::NowPlayingMediaCard { .. } => None,
            Self::VolumeMixerOsd { .. } => Some(3.0),
            Self::ScheduleAndCalendarCard { .. } => Some(20.0),
            Self::ToolForgeLiveCard { .. } => Some(12.0),
            Self::QuickConfirmModal { .. } => None,
            Self::VisualDataCard { .. } => Some(30.0),
        }
    }

    /// Seconds before the card shrinks into a pill (the media card).
    fn collapse_after(&self) -> Option<f32> {
        matches!(self, Self::NowPlayingMediaCard { .. }).then_some(8.0)
    }
}

/// What a click on a card asks the HUD to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PopupAction {
    MediaPrevious,
    MediaPlayPause,
    MediaNext,
    SetVolume { target: String, percent: u8 },
    Confirm(String),
    Cancel(String),
    /// A data card's button ("Play Clip"): its label goes to Jarvis as a request.
    Button(String),
}

#[derive(Debug, Clone)]
pub struct DynamicHudPopup {
    pub id: u64,
    pub payload: PopupPayload,
    pub ttl_secs: Option<f32>,
    pub age: f32,
    pub pinned: bool,
    pub hovered: bool,
    pub collapsed: bool,
    pub dismissing: bool,
    pub anim: SpringDamper,
    rect: Option<Rect>,
}

impl DynamicHudPopup {
    /// Scale, alpha and vertical offset of the entrance/exit animation.
    pub fn transform(&self) -> (f32, f32, f32) {
        let p = self.anim.value;
        (0.88 + 0.12 * p, p.clamp(0.0, 1.0), 24.0 * (1.0 - p))
    }
}

#[derive(Debug, Default)]
pub struct DynamicPopupManager {
    popups: Vec<DynamicHudPopup>,
    next_id: u64,
}

impl DynamicPopupManager {
    /// Shows a card (or updates the open one of its kind). `ttl`: `None` uses the kind's own.
    pub fn spawn(&mut self, payload: PopupPayload, ttl: Option<f32>) -> u64 {
        let ttl_secs = ttl.or_else(|| payload.default_ttl()).filter(|t| *t > 0.0);
        let key = payload.key();
        if let Some(p) = self.popups.iter_mut().find(|p| p.payload.key() == key) {
            p.payload = payload;
            p.ttl_secs = ttl_secs;
            p.age = 0.0;
            p.collapsed = false;
            p.dismissing = false;
            p.anim.target = 1.0;
            return p.id;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.popups.push(DynamicHudPopup { id, payload, ttl_secs, age: 0.0, pinned: false, hovered: false, collapsed: false, dismissing: false, anim: SpringDamper::new(0.0, 1.0), rect: None });
        id
    }

    /// Advances animations and lifetimes by `dt` seconds.
    pub fn step(&mut self, dt: f32) {
        for p in &mut self.popups {
            if !p.hovered && !p.pinned {
                p.age += dt;
            }
            if !p.dismissing && p.ttl_secs.is_some_and(|ttl| p.age >= ttl) {
                p.dismissing = true;
            }
            if !p.collapsed && p.payload.collapse_after().is_some_and(|after| p.age >= after) {
                p.collapsed = true;
            }
            p.anim.target = if p.dismissing { 0.0 } else { 1.0 };
            p.anim.step(dt);
        }
        self.popups.retain(|p| !(p.dismissing && p.anim.value < 0.02));
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        self.popups.iter_mut().filter(|p| p.id == id && !p.dismissing).map(|p| p.dismissing = true).count() > 0
    }

    /// Fades out every card (pinned ones only with `include_pinned`); returns how many.
    pub fn dismiss_all(&mut self, include_pinned: bool) -> usize {
        let mut n = 0;
        for p in self.popups.iter_mut().filter(|p| !p.dismissing && (include_pinned || !p.pinned)) {
            p.dismissing = true;
            n += 1;
        }
        n
    }

    /// Removes cards at once (no fade): the window-closing fast path.
    pub fn clear(&mut self) -> usize {
        let n = self.popups.iter().filter(|p| !p.dismissing).count();
        self.popups.clear();
        n
    }

    pub fn pin(&mut self, id: u64, on: bool) -> bool {
        self.popups.iter_mut().filter(|p| p.id == id).map(|p| p.pinned = on).count() > 0
    }

    /// "Pin that pop-up": the newest card still showing.
    pub fn pin_latest(&mut self, on: bool) -> Option<u64> {
        let p = self.popups.iter_mut().rev().find(|p| !p.dismissing)?;
        p.pinned = on;
        Some(p.id)
    }

    #[cfg(test)]
    pub fn get(&self, id: u64) -> Option<&DynamicHudPopup> {
        self.popups.iter().find(|p| p.id == id)
    }

    #[cfg(test)]
    pub fn visible(&self) -> impl Iterator<Item = &DynamicHudPopup> {
        self.popups.iter()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.popups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.popups.is_empty()
    }

    fn animating(&self) -> bool {
        self.popups.iter().any(|p| !p.anim.settled() || p.ttl_secs.is_some() || p.payload.collapse_after().is_some_and(|a| !p.collapsed && p.age < a))
    }
}

pub static POPUPS: LazyLock<Mutex<DynamicPopupManager>> = LazyLock::new(|| Mutex::new(DynamicPopupManager::default()));

pub fn manager() -> MutexGuard<'static, DynamicPopupManager> {
    POPUPS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows a card from any thread; the HUD repaints to animate it in.
pub fn spawn(payload: PopupPayload, ttl: Option<f32>) -> u64 {
    let id = manager().spawn(payload, ttl);
    crate::gui::refresh();
    id
}

/// Closes every card (pinned ones too with `include_pinned`) at once.
pub fn dismiss_all(include_pinned: bool) -> usize {
    let n = if include_pinned { manager().clear() } else { manager().dismiss_all(false) };
    if n > 0 {
        crate::gui::refresh();
    }
    n
}

/// The confirmation card of `action_id` goes once Sir answered (by voice or click).
pub fn dismiss_confirm(action_id: &str) {
    dismiss_key(&format!("confirm:{action_id}"));
}

/// Fades out the card of this key (`PopupPayload::key`): a card whose button did its work.
pub fn dismiss_key(key: &str) {
    let mut m = manager();
    let ids: Vec<u64> = m.popups.iter().filter(|p| p.payload.key() == key).map(|p| p.id).collect();
    for id in ids {
        m.dismiss(id);
    }
}

/// The media card for what just started playing (from the player's media session).
pub fn now_playing(title: &str, artist: &str, playlist: Option<&str>, player_name: &str) {
    if title.trim().is_empty() {
        return;
    }
    spawn(
        PopupPayload::NowPlayingMediaCard { title: title.into(), artist: artist.into(), playlist: playlist.map(String::from), player_name: player_name.into(), progress: 0.0, is_playing: true },
        None,
    );
}

/// The timeline card for a calendar or timetable result: every line that starts with a time
/// ("08:00 Maths", "14:30 – Dentist") becomes a row; the next one still ahead is highlighted.
pub fn schedule(title: &str, output: &str) {
    let entries = schedule_entries(output);
    if entries.is_empty() {
        return;
    }
    let now = chrono::Local::now().format("%H:%M").to_string();
    let highlight_index = entries.iter().position(|(when, _)| when.as_str() >= now.as_str());
    spawn(PopupPayload::ScheduleAndCalendarCard { title: title.into(), entries, highlight_index }, None);
}

fn schedule_entries(output: &str) -> Vec<(String, String)> {
    static TIMED: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\s*[-•*]?\s*(\d{1,2}[:.]\d{2})(?:\s*[-–]\s*\d{1,2}[:.]\d{2})?\s*[-–:,]?\s*(.+)$").expect("schedule line"));
    output
        .lines()
        .filter_map(|l| TIMED.captures(l))
        .map(|c| (format!("{:0>5}", c[1].replace('.', ":")), c[2].trim().to_string()))
        .take(10)
        .collect()
}

/// The Dynamic-Island volume bar.
pub fn volume(target_name: &str, percent: u8, is_muted: bool) {
    spawn(PopupPayload::VolumeMixerOsd { target_name: target_name.into(), percent: percent.min(100), is_muted }, None);
}

// ---------------------------------------------------------------------- voice + tool ----

/// "Pin that pop-up", "unpin it", "dismiss pop-ups", "schließ die Pop-ups".
pub fn voice_command(text: &str) -> Option<Value> {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' }).collect();
    let s = s.split_whitespace().filter(|w| !matches!(*w, "jarvis" | "please" | "bitte" | "sir")).collect::<Vec<_>>().join(" ");
    const POPUP: &str = r"(?:pop-?ups?|pop ups?|popups?|cards?|karten?)";
    static PIN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(&format!(r"^(?:pin|pinne|halte|behalte)\s+(?:that|this|the|it|das|die|den)?\s*(?:{POPUP})?(?:\s+(?:fest|offen))?$")).expect("pin expression")
    });
    static UNPIN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(&format!(r"^(?:unpin|löse|lös)\s+(?:that|this|the|it|das|die)?\s*(?:{POPUP})?$")).expect("unpin expression")
    });
    static DISMISS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(&format!(r"^(?:dismiss|close|clear|hide|remove|schließ|schließe|schliess|entferne)\s+(?:(?:all|the|alle|die|those|these)\s+)*{POPUP}$")).expect("dismiss expression")
    });
    if s.is_empty() {
        return None;
    }
    if UNPIN.is_match(&s) {
        return Some(json!({"action": "unpin"}));
    }
    if PIN.is_match(&s) {
        return Some(json!({"action": "pin"}));
    }
    if DISMISS.is_match(&s) {
        return Some(json!({"action": "dismiss_all"}));
    }
    None
}

pub fn is_tool(name: &str) -> bool {
    matches!(name, "pop_up_ui_card" | "control_popups")
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "pop_up_ui_card",
            "description": "Pop up a card beside the orb to SHOW something visual: a calculation, code snippet, research finding, QR text, a small chart (chart_points) or buttons Sir can click (action_buttons, e.g. 'Play Clip'). It fades after duration_secs unless pinned.",
            "parameters": {"type": "object", "properties": {
                "title": {"type": "string"},
                "content": {"type": "string", "description": "Markdown or code shown in the card"},
                "duration_secs": {"type": "number"},
                "pinned": {"type": "boolean"},
                "chart_points": {"type": "array", "items": {"type": "number"}},
                "action_buttons": {"type": "array", "items": {"type": "string"}}
            }, "required": ["title", "content"]}
        }}),
        json!({"type": "function", "function": {
            "name": "control_popups",
            "description": "Pin, unpin or dismiss the pop-up cards ('pin that pop-up', 'dismiss pop-ups').",
            "parameters": {"type": "object", "properties": {"action": {"type": "string", "enum": ["pin", "unpin", "dismiss_all"]}}, "required": ["action"]}
        }}),
    ]
}

pub fn run_tool(name: &str, args: &Value) -> Result<String, String> {
    if name == "control_popups" {
        return match args["action"].as_str().unwrap_or("") {
            "pin" => manager().pin_latest(true).map(|id| format!("Pinned pop-up #{id}.")).ok_or_else(|| "No pop-up is showing.".to_string()),
            "unpin" => manager().pin_latest(false).map(|id| format!("Unpinned pop-up #{id}.")).ok_or_else(|| "No pop-up is showing.".to_string()),
            "dismiss_all" | "dismiss" | "clear" => Ok(format!("Dismissed {} pop-up(s).", dismiss_all(false))),
            other => Err(format!("Unknown pop-up action '{other}' (pin, unpin, dismiss_all).")),
        };
    }
    let title = args["title"].as_str().filter(|t| !t.trim().is_empty()).ok_or("pop_up_ui_card needs a title.")?.trim().to_string();
    let content = args["content"].as_str().or(args["markdown_or_code"].as_str()).unwrap_or("").to_string();
    let points: Vec<f32> = args["chart_points"].as_array().into_iter().flatten().filter_map(Value::as_f64).map(|v| v as f32).take(500).collect();
    let buttons: Vec<String> = args["action_buttons"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).take(4).collect();
    let ttl = args["duration_secs"].as_f64().map(|t| t.clamp(2.0, 3600.0) as f32);
    let id = spawn(PopupPayload::VisualDataCard { title: title.clone(), markdown_or_code: content, chart_points: points, action_buttons: buttons }, ttl);
    if args["pinned"].as_bool() == Some(true) {
        manager().pin(id, true);
    }
    Ok(format!("Pop-up card '{title}' is showing beside the orb."))
}

// ------------------------------------------------------------------------ rendering ----

/// Card colours: the OLED surface, a hairline border and the accent.
#[derive(Debug, Clone, Copy)]
pub struct CardStyle {
    pub surface: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub rounding: f32,
}

/// Steps and draws every card in a column at the bottom right of the HUD; returns what Sir
/// clicked.
pub fn show(ctx: &egui::Context, style: CardStyle, dt: f32) -> Vec<PopupAction> {
    let mut actions = Vec::new();
    let mut m = manager();
    m.step(dt);
    if m.is_empty() {
        return actions;
    }
    // (The HUD's home screen: on a board across several monitors the window's own bottom
    // right corner is on none of them.)
    let screen = crate::gui::home_screen(ctx);
    let width = 340.0_f32.min(screen.width() - 32.0).max(180.0);
    // The column keeps off the text console: a card over its input line took the clicks and
    // the keys' focus meant for the line ("the text field sometimes refuses my input").
    let right = card_column_right(screen, width, crate::gui::console_rect());
    let mut bottom = screen.bottom() - 18.0;
    let mut dismiss = Vec::new();
    let mut pins = Vec::new();
    for p in m.popups.iter_mut().rev() {
        let (scale, alpha, dy) = p.transform();
        let height = p.rect.map(|r| r.height()).unwrap_or(if p.collapsed { 36.0 } else { 120.0 });
        let top = bottom - height;
        // (On whole pixels: a card between two would draw its text and border soft.)
        let ppp = ctx.pixels_per_point();
        let at = pos2(((right - width) * ppp).round() / ppp, (top * ppp).round() / ppp);
        let area_id = Id::new(("jarvis_popup", p.id));
        // A card on its way out, or still faint on its way in, takes no clicks: what is under it
        // (a block, the console's line) keeps them.
        let solid = !p.dismissing && alpha > 0.5;
        let response = egui::Area::new(area_id).order(Order::Foreground).fixed_pos(at).interactable(solid).show(ctx, |ui| {
            ui.set_width(width);
            ui.multiply_opacity(alpha);
            let hovered_border = if p.pinned { style.accent } else { style.border };
            egui::Frame::none()
                .fill(style.surface)
                .stroke(Stroke::new(1.0_f32, hovered_border))
                .rounding(Rounding::same(if p.collapsed { 18.0 } else { style.rounding }))
                .inner_margin(egui::Margin::symmetric(14.0, if p.collapsed { 8.0 } else { 12.0 }))
                .show(ui, |ui| {
                    ui.set_width(width - 28.0);
                    card_body(ui, p, &style, &mut actions, &mut dismiss, &mut pins);
                });
        });
        let rect = response.response.rect;
        p.hovered = solid && ctx.pointer_hover_pos().is_some_and(|pos| rect.contains(pos));
        if p.hovered && !p.pinned {
            // Hovering a card holds it (and lights its edge).
            ctx.layer_painter(egui::LayerId::new(Order::Foreground, area_id)).rect_stroke(rect.shrink(0.5), Rounding::same(style.rounding), Stroke::new(1.0_f32, style.accent.gamma_multiply(0.7)));
        }
        p.rect = Some(rect);
        // At rest exactly where it stands (a scale a hair off 1 blurs every letter); on its way
        // in or out it grows from, and shrinks to, a little below its place.
        let transform = if p.anim.settled() || ((scale - 1.0).abs() < 1e-3 && dy.abs() < 0.05) || !crate::gui::motion_enabled() {
            TSTransform::IDENTITY
        } else {
            let center = rect.center();
            TSTransform::new(vec2(center.x * (1.0 - scale), center.y * (1.0 - scale) + dy), scale)
        };
        ctx.set_transform_layer(egui::LayerId::new(Order::Foreground, area_id), transform);
        bottom = top - 10.0;
    }
    for id in dismiss {
        m.dismiss(id);
    }
    for (id, on) in pins {
        m.pin(id, on);
    }
    if m.animating() || !actions.is_empty() {
        ctx.request_repaint();
    }
    actions
}

/// The right edge of the cards' column on `screen`: its bottom-right corner, or left of the text
/// console when the console stands there (as far left as the screen allows).
pub fn card_column_right(screen: Rect, width: f32, console: Option<Rect>) -> f32 {
    let right = screen.right() - 18.0;
    match console.filter(|c| c.left() < right && c.right() > right - width && c.bottom() > screen.top()) {
        Some(c) => (c.left() - 18.0).max(screen.left() + 16.0 + width),
        None => right,
    }
}

fn card_body(ui: &mut egui::Ui, p: &mut DynamicHudPopup, style: &CardStyle, actions: &mut Vec<PopupAction>, dismiss: &mut Vec<u64>, pins: &mut Vec<(u64, bool)>) {
    let header = |ui: &mut egui::Ui, title: &str, dismiss: &mut Vec<u64>, pins: &mut Vec<(u64, bool)>| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong().color(style.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("X").color(style.muted)).frame(false)).on_hover_text("Dismiss").clicked() {
                    dismiss.push(p.id);
                }
                let pin = if p.pinned { "PIN" } else { "PINNED" };
                if ui.add(egui::Button::new(RichText::new(pin).color(if p.pinned { style.accent } else { style.muted })).frame(false)).on_hover_text(if p.pinned { "Unpin" } else { "Pin" }).clicked() {
                    pins.push((p.id, !p.pinned));
                }
            });
        });
    };
    match &p.payload {
        PopupPayload::NowPlayingMediaCard { title, artist, playlist, player_name, progress, is_playing } => {
            if p.collapsed {
                let r = ui.horizontal(|ui| {
                    ui.label(RichText::new(if *is_playing { "PLAY" } else { "PAUSE" }).color(style.accent));
                    ui.label(RichText::new(format!("{title} — {artist}")).color(style.text).small());
                });
                if ui.interact(r.response.rect, Id::new(("popup_pill", p.id)), Sense::click()).clicked() {
                    p.collapsed = false;
                    p.age = 0.0;
                }
                return;
            }
            header(ui, title, dismiss, pins);
            ui.label(RichText::new(artist).color(style.muted));
            let from = playlist.as_ref().map(|l| format!("{player_name} · {l}")).unwrap_or_else(|| player_name.clone());
            ui.label(RichText::new(from).small().color(style.muted));
            let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
            ui.painter().rect_filled(bar, Rounding::same(2.0), style.border);
            ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * progress.clamp(0.0, 1.0), bar.height())), Rounding::same(2.0), style.accent);
            ui.horizontal(|ui| {
                for (label, action) in [("PREV", PopupAction::MediaPrevious), (if *is_playing { "PAUSE" } else { "PLAY" }, PopupAction::MediaPlayPause), ("NEXT", PopupAction::MediaNext)] {
                    if ui.add(egui::Button::new(RichText::new(label).size(16.0).color(style.text)).frame(false)).clicked() {
                        actions.push(action);
                    }
                }
            });
        }
        PopupPayload::VolumeMixerOsd { target_name, percent, is_muted } => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if *is_muted { "MUTED" } else { "SOUND" }).color(style.accent));
                ui.label(RichText::new(target_name).color(style.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{percent} %")).color(style.muted).monospace());
                });
            });
            let mut v = *percent as f32;
            if ui.add(egui::Slider::new(&mut v, 0.0..=100.0).show_value(false)).changed() {
                actions.push(PopupAction::SetVolume { target: target_name.clone(), percent: v.round() as u8 });
            }
        }
        PopupPayload::ScheduleAndCalendarCard { title, entries, highlight_index } => {
            header(ui, title, dismiss, pins);
            for (i, (when, what)) in entries.iter().enumerate().take(10) {
                let hot = *highlight_index == Some(i);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(when).monospace().color(if hot { style.accent } else { style.muted }));
                    ui.label(RichText::new(what).color(style.text));
                });
            }
        }
        PopupPayload::ToolForgeLiveCard { tool_name, steps_summary, status } => {
            header(ui, &format!("New Tool Forged · {tool_name}"), dismiss, pins);
            ui.label(RichText::new(status).color(style.accent).small());
            for step in steps_summary.iter().take(8) {
                ui.label(RichText::new(format!("› {step}")).small().color(style.muted));
            }
        }
        PopupPayload::QuickConfirmModal { title, description, confirm_action_id } => {
            header(ui, title, dismiss, pins);
            ui.label(RichText::new(description).color(style.muted));
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Confirm").color(Color32::BLACK)).fill(style.accent).rounding(Rounding::same(10.0))).clicked() {
                    actions.push(PopupAction::Confirm(confirm_action_id.clone()));
                    dismiss.push(p.id);
                }
                if ui.add(egui::Button::new(RichText::new("Cancel").color(style.text)).stroke(Stroke::new(1.0_f32, style.border)).fill(Color32::TRANSPARENT).rounding(Rounding::same(10.0))).clicked() {
                    actions.push(PopupAction::Cancel(confirm_action_id.clone()));
                    dismiss.push(p.id);
                }
            });
        }
        PopupPayload::VisualDataCard { title, markdown_or_code, chart_points, action_buttons } => {
            header(ui, title, dismiss, pins);
            if !markdown_or_code.is_empty() {
                egui::ScrollArea::vertical().max_height(220.0).id_salt(("popup_text", p.id)).show(ui, |ui| {
                    let mono = markdown_or_code.contains("```") || markdown_or_code.lines().any(|l| l.starts_with("    "));
                    let text = markdown_or_code.replace("```", "");
                    ui.label(if mono { RichText::new(text.trim()).monospace().color(style.text) } else { RichText::new(text.trim()).color(style.text) });
                });
            }
            if chart_points.len() >= 2 {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
                let (lo, hi) = chart_points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
                let span = (hi - lo).max(1e-6);
                let n = chart_points.len() as f32 - 1.0;
                let line: Vec<_> = chart_points.iter().enumerate().map(|(i, v)| pos2(rect.left() + rect.width() * i as f32 / n, rect.bottom() - rect.height() * (v - lo) / span)).collect();
                ui.painter().add(egui::Shape::line(line, Stroke::new(1.5_f32, style.accent)));
            }
            if !action_buttons.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for label in action_buttons {
                        if ui.add(egui::Button::new(RichText::new(format!("[ {label} ]")).color(style.accent)).frame(false)).clicked() {
                            actions.push(PopupAction::Button(label.clone()));
                        }
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(title: &str) -> PopupPayload {
        PopupPayload::NowPlayingMediaCard { title: title.into(), artist: "AC/DC".into(), playlist: Some("Jays Rock".into()), player_name: "Sonora".into(), progress: 0.1, is_playing: true }
    }

    fn run(m: &mut DynamicPopupManager, secs: f32) {
        let mut t = 0.0;
        while t < secs {
            m.step(1.0 / 60.0);
            t += 1.0 / 60.0;
        }
    }

    /// The cards' column never stands on the text console (its input line lies under the
    /// column's bottom-right corner).
    #[test]
    fn cards_keep_off_the_text_console() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0));
        assert_eq!(card_column_right(screen, 340.0, None), 1902.0);
        let console = Rect::from_min_max(pos2(1480.0, 60.0), pos2(1920.0, 1020.0));
        let right = card_column_right(screen, 340.0, Some(console));
        assert!(right <= console.left() - 18.0 + 1e-3 && right - 340.0 >= 16.0, "{right}");
        // A console elsewhere leaves the corner alone.
        assert_eq!(card_column_right(screen, 340.0, Some(Rect::from_min_size(pos2(40.0, 60.0), vec2(400.0, 900.0)))), 1902.0);
        // A screen too narrow for both: as far left as it goes, never off it.
        let narrow = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 800.0));
        assert_eq!(card_column_right(narrow, 340.0, Some(Rect::from_min_max(pos2(160.0, 0.0), pos2(600.0, 800.0)))), 356.0);
    }

    #[test]
    fn spring_animates_entrance_from_088_to_1() {
        let mut m = DynamicPopupManager::default();
        let id = m.spawn(media("Highway to Hell"), None);
        let (scale, alpha, dy) = m.get(id).unwrap().transform();
        assert!((scale - 0.88).abs() < 1e-4 && alpha == 0.0 && (dy - 24.0).abs() < 1e-4);
        m.step(0.05);
        let mid = m.get(id).unwrap().anim.value;
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        run(&mut m, 1.0);
        let (scale, alpha, dy) = m.get(id).unwrap().transform();
        assert!((scale - 1.0).abs() < 0.01 && (alpha - 1.0).abs() < 0.01 && dy.abs() < 0.3, "{scale} {alpha} {dy}");
        assert!(m.get(id).unwrap().anim.settled());
    }

    #[test]
    fn spring_constants_match_the_spec() {
        let s = SpringDamper::new(0.0, 1.0);
        assert_eq!((s.stiffness, s.damping), (220.0, 24.0));
    }

    #[test]
    fn ttl_expires_volume_osd_after_three_seconds() {
        let mut m = DynamicPopupManager::default();
        m.spawn(PopupPayload::VolumeMixerOsd { target_name: "Sonora".into(), percent: 40, is_muted: false }, None);
        run(&mut m, 2.5);
        assert_eq!(m.len(), 1);
        run(&mut m, 1.5);
        assert!(m.is_empty(), "faded out and removed");
    }

    #[test]
    fn pinned_and_hovered_cards_never_expire() {
        let mut m = DynamicPopupManager::default();
        let a = m.spawn(PopupPayload::VisualDataCard { title: "Result".into(), markdown_or_code: "42".into(), chart_points: vec![], action_buttons: vec![] }, Some(1.0));
        let b = m.spawn(PopupPayload::VolumeMixerOsd { target_name: "Master".into(), percent: 50, is_muted: false }, None);
        assert!(m.pin(a, true));
        m.popups.iter_mut().find(|p| p.id == b).unwrap().hovered = true;
        run(&mut m, 5.0);
        assert_eq!(m.len(), 2);
        assert_eq!(m.pin_latest(false), Some(b));
    }

    #[test]
    fn media_card_collapses_into_pill_after_eight_seconds_and_updates_in_place() {
        let mut m = DynamicPopupManager::default();
        let id = m.spawn(media("Highway to Hell"), None);
        run(&mut m, 8.2);
        assert!(m.get(id).unwrap().collapsed);
        let again = m.spawn(media("Back in Black"), None);
        assert_eq!(again, id, "same card, new track");
        assert!(!m.get(id).unwrap().collapsed);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn dismiss_is_instant_on_click_and_all_respects_pins() {
        let mut m = DynamicPopupManager::default();
        let a = m.spawn(PopupPayload::ToolForgeLiveCard { tool_name: "playlist_player".into(), steps_summary: vec!["scan".into()], status: "verified".into() }, None);
        let b = m.spawn(PopupPayload::ScheduleAndCalendarCard { title: "Today".into(), entries: vec![("08:00".into(), "Maths".into())], highlight_index: Some(0) }, None);
        m.pin(b, true);
        assert!(m.dismiss(a));
        run(&mut m, 0.6);
        assert!(m.get(a).is_none());
        assert_eq!(m.dismiss_all(false), 0, "the pinned card stays");
        assert_eq!(m.dismiss_all(true), 1);
        m.spawn(media("x"), None);
        assert!(m.clear() >= 1);
        assert!(m.is_empty());
    }

    #[test]
    fn schedule_card_reads_timed_lines() {
        let entries = schedule_entries("Tomorrow:\n- 08:00 – 09:30 Maths (Room 12)\n9.45: English\nNo time here\n14:30, Dentist");
        assert_eq!(entries, vec![("08:00".into(), "Maths (Room 12)".into()), ("09:45".into(), "English".into()), ("14:30".into(), "Dentist".into())]);
        assert!(schedule_entries("Nothing on your calendar.").is_empty());
    }

    #[test]
    fn tool_and_voice_commands() {
        assert_eq!(voice_command("Jarvis, pin that pop-up").unwrap()["action"], "pin");
        assert_eq!(voice_command("Dismiss pop-ups").unwrap()["action"], "dismiss_all");
        assert_eq!(voice_command("unpin it").unwrap()["action"], "unpin");
        assert!(voice_command("pin the tab in chrome to the left").is_none());
        let out = run_tool("pop_up_ui_card", &json!({"title": "Kinematics", "content": "v = 12 m/s", "duration_secs": 5, "chart_points": [1, 2, 3], "action_buttons": ["Play Clip"]})).unwrap();
        assert!(out.contains("Kinematics"));
        assert!(manager().visible().any(|p| matches!(&p.payload, PopupPayload::VisualDataCard { title, .. } if title == "Kinematics")));
        assert!(run_tool("pop_up_ui_card", &json!({"content": "x"})).is_err());
    }
}
