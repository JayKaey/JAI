//! The NETWORK RADAR block ("check the network"): the sweep drawn as a radar scope, every
//! device placed by how fast it answers and revealed by the beam; the adapter and internet
//! cards, the device list, the latency chart and the speed test on the right. The data comes
//! from `crate::network_radar`; this file draws it and turns clicks into actions.

use super::{bold_family, rgba, stroke, BLOCK_SLATE, STRIP_INK, STRIP_MUTED};
use crate::network_radar::{self as radar, Device, Kind, Phase, Series, Snapshot, Speed};
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontFamily, FontId, Frame, Galley, Margin, Mesh, Painter, Pos2, Rect, Rounding, Sense, Shape, Ui, Vec2, ViewportBuilder, ViewportClass,
    ViewportCommand, ViewportId,
};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "network_radar";
const TITLE: &str = "Network radar";
/// The block on the board; the detached window is a little larger (its own title bar).
const BLOCK_SIZE: [f32; 2] = [1180.0, 720.0];
const WINDOW_SIZE: [f32; 2] = [1200.0, 760.0];
const WINDOW_MIN: [f32; 2] = [700.0, 460.0];
/// The block's content at `BLOCK_SIZE`: everything scales from it.
const DESIGN: Vec2 = vec2(1156.0, 668.0);
/// The beam: degrees a second (clockwise from north) and its width.
const BEAM_SPEED: f64 = 110.0;
const BEAM_WIDTH: f32 = 28.0;
/// Where the beam starts turning: off the bearings a device can stand on (tenths of a degree,
/// the router at north), so none waits a whole turn for its first pass.
const BEAM_START: f64 = 300.05;
/// A device the beam finds fills in over this long; its halo fades over the longer time.
const REVEAL: f64 = 0.15;
const FOUND_HALO: f64 = 0.6;
/// A later pass of the beam lights a device this long.
const PASS_GLOW: f64 = 0.3;
/// The outer ring flashes this long when the sweep completes.
const COMPLETE_FLASH: f64 = 0.3;
/// Device rows appear one after another, this far apart.
const ROW_STAGGER: f64 = 0.05;
/// A status line that changes cross-fades this long.
const TEXT_SWAP: f64 = 0.05;
/// The latency rings on the scope.
const RINGS_MS: [f32; 4] = [1.0, 3.0, 10.0, 30.0];
/// "LIVE", the router, a new device.
const STATUS_RED: Color32 = crate::hud_board::PIN_RED;

static VIEW: LazyLock<Mutex<View>> = LazyLock::new(|| Mutex::new(View::default()));

// ----------------------------------------------------------------------------- window ----

/// The radar's content (the block's and the detached window's).
fn body(ui: &mut Ui, rt: &Handle) {
    // (Closed while it was its own window: its exit shows the scope as it was and takes no
    // clicks.)
    let leaving = !radar::is_open();
    radar::ensure_monitor();
    let snap = radar::snapshot();
    let (actions, busy) = {
        let mut view = VIEW.lock().unwrap_or_else(|e| e.into_inner());
        let actions = draw(ui, &snap, &mut view);
        let key = (snap.generation, snap.devices.len(), snap.phase, snap.devices.iter().filter(|d| !d.host.is_empty()).count());
        if !leaving && view.described != Some(key) {
            view.described = Some(key);
            crate::hud_dock::describe_block(ID, &radar::summary_text(&snap));
        }
        (actions, snap.sweeping() || view.busy(ui.input(|i| i.time)))
    };
    if leaving {
        return;
    }
    if actions.rescan {
        radar::rescan();
    }
    if actions.speed_test {
        radar::start_speed_test(rt);
    }
    if actions.close {
        if crate::hud_dock::close_named(ID).is_empty() {
            radar::close();
        }
        crate::gui::refresh();
    }
    crate::hud_style::set_light(ID, if busy { crate::hud_style::Light::Busy } else { crate::hud_style::Light::Good });
    ui.ctx().request_repaint_after(Duration::from_millis(if busy { 16 } else { 33 }));
}

/// Registers the radar while it is open (a block on the board, or its own window once detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, ViewportId::from_hash_of("jai_network_radar"), crate::network_radar::is_open()) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, BLOCK_SIZE, |ui| body(ui, rt));
        if action.close {
            crate::network_radar::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Network radar — J.A.I.").with_app_id("jai").with_inner_size(WINDOW_SIZE).with_min_inner_size(WINDOW_MIN).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jai_network_radar"), builder, move |ctx, class| {
        crate::crash_guard::guard("network radar", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!crate::network_radar::is_open() && !super::detached_leaving(ctx)) {
                crate::network_radar::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        crate::network_radar::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

// ------------------------------------------------------------------------------- view ----

/// What Sir asked for with the mouse this frame.
#[derive(Clone, Copy, Debug, Default)]
struct Actions {
    rescan: bool,
    speed_test: bool,
    close: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Blip {
    /// When the beam first passed it.
    found: Option<f64>,
    /// When the beam last passed it again.
    pass: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Latency,
    Speed,
}

/// A status line that cross-fades when its phase changes: the old words fade out, then the
/// new ones in (never both at once). Within a phase it just changes.
#[derive(Debug, Default)]
struct Fade {
    key: Option<Phase>,
    text: String,
    old: String,
    at: f64,
}

impl Fade {
    fn set(&mut self, key: Phase, text: &str, now: f64) {
        if self.key != Some(key) {
            if self.key.is_some() && !self.text.is_empty() {
                self.old = std::mem::take(&mut self.text);
                self.at = now;
            }
            self.key = Some(key);
        }
        if self.text != text {
            self.text = text.to_string();
        }
    }

    fn paint(&self, pen: &Pen, at: Pos2, font: &FontId, colour: Color32, spacing: f32, width: f32) {
        let t = ((pen.now - self.at) / TEXT_SWAP) as f32;
        let (text, alpha) = match self.old.is_empty() {
            false if t < 0.5 => (&self.old, 1.0 - 2.0 * t.max(0.0)),
            false if t < 1.0 => (&self.text, 2.0 * t - 1.0),
            _ => (&self.text, 1.0),
        };
        if alpha >= 0.03 && !text.is_empty() {
            pen.put(pen.text(text, font.clone(), colour.gamma_multiply(alpha), spacing, width), at, Align2::LEFT_TOP);
        }
    }

    fn fading(&self, now: f64) -> bool {
        !self.old.is_empty() && now - self.at < TEXT_SWAP
    }
}

/// The radar's own state between frames (the data is the snapshot's).
#[derive(Debug, Default)]
struct View {
    session: Option<u64>,
    generation: u64,
    /// The beam's first frame.
    start: Option<f64>,
    /// The previous frame.
    last: Option<f64>,
    blips: HashMap<Ipv4Addr, Blip>,
    /// Device rows shown so far (they appear one after another).
    shown: usize,
    shown_at: f64,
    /// The sweep was done before the radar saw it: everything shows at once.
    instant: bool,
    done_seen: bool,
    flash_at: Option<f64>,
    subtitle: Fade,
    footer: Fade,
    selected: Option<Ipv4Addr>,
    tab: Tab,
    /// The first device row shown (the wheel scrolls whole rows).
    scroll: usize,
    /// What the block's text was last made from.
    described: Option<(u64, usize, Phase, usize)>,
}

impl View {
    fn sync(&mut self, snap: &Snapshot, now: f64) {
        if self.session != Some(snap.session) {
            *self = View { session: Some(snap.session), tab: self.tab, generation: u64::MAX, ..View::default() };
        }
        if self.generation != snap.generation {
            self.generation = snap.generation;
            self.blips.clear();
            self.shown = 0;
            self.shown_at = now;
            self.flash_at = None;
            self.instant = snap.phase == Phase::Done;
            self.done_seen = snap.phase == Phase::Done;
            self.selected = None;
            self.scroll = 0;
        }
        if self.start.is_none() {
            self.start = Some(now);
        }
        if snap.phase == Phase::Done && !self.done_seen {
            self.done_seen = true;
            self.flash_at = Some(now);
        }
        let n = snap.devices.len();
        if self.instant {
            self.shown = n;
        } else if self.shown < n && now - self.shown_at >= ROW_STAGGER {
            self.shown += 1;
            self.shown_at = now;
        }
        self.shown = self.shown.min(n);
    }

    /// The beam's angle at `t`: degrees clockwise from north.
    fn beam(&self, t: f64) -> f32 {
        (BEAM_START + (t - self.start.unwrap_or(t)) * BEAM_SPEED).rem_euclid(360.0) as f32
    }

    /// Something is moving faster than the beam needs (a flash, a fade, a discovery).
    fn busy(&self, now: f64) -> bool {
        self.flash_at.is_some_and(|t| now - t < COMPLETE_FLASH)
            || self.subtitle.fading(now)
            || self.footer.fading(now)
            || self.blips.values().any(|b| b.found.is_some_and(|t| now - t < FOUND_HALO) || b.pass.is_some_and(|t| now - t < PASS_GLOW))
    }
}

/// Whether the beam passed `angle` going clockwise from `from` to `to` (degrees, 0..360).
fn swept(from: f32, to: f32, angle: f32) -> bool {
    if to >= from {
        angle > from && angle <= to
    } else {
        angle > from || angle <= to
    }
}

/// The beam finds what it passes: a device it has not lit yet appears (a hollow ring that
/// fills), one it found before glows again.
fn reveal(view: &mut View, devices: &[(Ipv4Addr, f32)], now: f64) {
    let prev = view.last.unwrap_or(now);
    let (from, to) = (view.beam(prev), view.beam(now));
    let full_turn = (now - prev) * BEAM_SPEED >= 360.0;
    for (ip, angle) in devices {
        let blip = view.blips.entry(*ip).or_default();
        if view.instant && blip.found.is_none() {
            blip.found = Some(now - 1.0);
            continue;
        }
        if now > prev && (full_turn || swept(from, to, *angle)) {
            if blip.found.is_none() {
                blip.found = Some(now);
            } else {
                blip.pass = Some(now);
            }
        }
    }
    view.last = Some(now);
}

// ------------------------------------------------------------------------------- pen ----

#[derive(Clone, Copy)]
struct Ink {
    accent: Color32,
    second: Color32,
    border: Color32,
    bracket: Color32,
    hot: Color32,
    text: Color32,
    muted: Color32,
}

impl Ink {
    fn now() -> Self {
        let theme = crate::theme_engine::current();
        let [r, g, b] = theme.core_hot_center;
        Self {
            accent: crate::hud_board::active_colour(),
            second: theme.secondary(),
            border: crate::hud_board::border_colour(),
            bracket: crate::hud_board::bracket_colour(),
            hot: Color32::from_rgb(r, g, b),
            text: STRIP_INK,
            muted: STRIP_MUTED,
        }
    }

    /// A device's colour by what it is.
    fn kind(&self, kind: Kind) -> Color32 {
        match kind {
            Kind::Router => STATUS_RED,
            Kind::Phone | Kind::Tablet => Color32::from_rgb(0x3E, 0xE0, 0x7A),
            Kind::SmartHome => Color32::from_rgb(0x45, 0xE0, 0xB4),
            Kind::Pc => self.accent,
            Kind::ThisPc => self.hot,
            Kind::Tv => Color32::from_rgb(0xB5, 0x8C, 0xFF),
            Kind::Printer => Color32::from_rgb(0xFF, 0xB5, 0x47),
            Kind::Console => Color32::from_rgb(0x5A, 0xA8, 0xFF),
            Kind::Unknown => Color32::from_rgb(0x9A, 0xA6, 0xB2),
        }
    }
}

/// Everything a part of the radar draws with: the painter (clipped to the radar), the scale,
/// the colours, the time.
struct Pen<'a> {
    ui: &'a Ui,
    p: Painter,
    /// The layout's scale against `DESIGN`.
    k: f32,
    ink: Ink,
    id: egui::Id,
    now: f64,
}

impl Pen<'_> {
    fn px(&self, v: f32) -> f32 {
        v * self.k
    }

    /// A size that scales but never below `min` (text stays readable in a small window).
    fn size(&self, base: f32, min: f32) -> f32 {
        (base * self.k).max(min)
    }

    fn mono(&self, base: f32, min: f32) -> FontId {
        FontId::monospace(self.size(base, min))
    }

    fn bold(&self, base: f32, min: f32) -> FontId {
        FontId::new(self.size(base, min), bold_family())
    }

    /// Bahnschrift: the condensed DIN of the headings.
    fn display(&self, base: f32, min: f32) -> FontId {
        FontId::new(self.size(base, min), FontFamily::Name("display".into()))
    }

    fn plain(&self, base: f32, min: f32) -> FontId {
        FontId::proportional(self.size(base, min))
    }

    fn row(&self, font: &FontId) -> f32 {
        self.p.ctx().fonts(|f| f.row_height(font))
    }

    /// One line of text, letter-spaced, cut with an ellipsis at `width`.
    fn text(&self, text: &str, font: FontId, colour: Color32, spacing: f32, width: f32) -> Arc<Galley> {
        let mut job = LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font, color: colour, extra_letter_spacing: spacing, ..Default::default() });
        job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
        self.p.layout_job(job)
    }

    /// The text's width in one line, uncut.
    fn measure(&self, text: &str, font: &FontId, spacing: f32) -> f32 {
        let job = LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font.clone(), extra_letter_spacing: spacing, ..Default::default() });
        self.p.layout_job(job).size().x
    }

    fn put(&self, galley: Arc<Galley>, at: Pos2, align: Align2) -> Rect {
        let rect = align.anchor_size(at, galley.size());
        self.p.galley(rect.min, galley, self.ink.text);
        rect
    }

    /// A clickable spot; a right-click on any of them closes the radar.
    fn click(&self, rect: Rect, salt: impl std::hash::Hash, actions: &mut Actions) -> egui::Response {
        let response = self.ui.interact(rect, self.id.with(salt), Sense::click());
        if response.secondary_clicked() {
            actions.close = true;
        }
        response
    }
}

// ------------------------------------------------------------------------------- draw ----

/// The whole radar in `ui`'s free space.
fn draw(ui: &mut Ui, snap: &Snapshot, view: &mut View) -> Actions {
    let mut actions = Actions::default();
    let free = ui.available_rect_before_wrap();
    let rect = if free.is_finite() && free.is_positive() { free } else { Rect::from_min_size(ui.max_rect().min, DESIGN) };
    ui.advance_cursor_after_rect(rect);
    let now = ui.input(|i| i.time);
    view.sync(snap, now);
    let pen = Pen {
        ui,
        p: ui.painter_at(rect),
        k: (rect.width() / DESIGN.x).min(rect.height() / DESIGN.y).clamp(0.5, 1.6),
        ink: Ink::now(),
        id: ui.id().with("network_radar"),
        now,
    };
    // The panel itself (under everything): a click on nothing drops the selection.
    if pen.click(rect, "panel", &mut actions).clicked() {
        view.selected = None;
    }
    let inner = rect.shrink(pen.px(10.0));
    let header_bottom = header(&pen, inner, snap, view);
    let footer_top = footer(&pen, inner, snap, view);
    let top = header_bottom + pen.px(10.0);
    let body = Rect::from_min_max(pos2(inner.left(), top), pos2(inner.right(), footer_top.max(top + 40.0)));
    let split = body.left() + body.width() * 0.47;
    let left = Rect::from_min_max(body.min, pos2(split, body.bottom()));
    let right = Rect::from_min_max(pos2(split + pen.px(16.0), body.top()), body.max);
    let card = selected(snap, view).map(|_| details_rect(&pen, left));
    let floating = scope(&pen, left, snap, view, card, &mut actions);
    column(&pen, right, snap, view, &mut actions);
    if let Some(label) = floating {
        label.paint(&pen);
    }
    details(&pen, left, snap, view, &mut actions);
    corner_brackets(&pen.p, rect.shrink(1.0), pen.px(14.0), pen.ink.accent);
    actions
}

fn subtitle(snap: &Snapshot) -> String {
    match (snap.phase, &snap.adapter) {
        (Phase::Failed, _) => "NO ACTIVE NETWORK CONNECTION".to_string(),
        (Phase::Idle, None) => "STANDING BY".to_string(),
        (_, None) => "LOCATING THE NETWORK..".to_string(),
        (phase, Some(a)) => {
            let state = if matches!(phase, Phase::Locating | Phase::Sweeping) { "SWEEPING".to_string() } else { format!("{} ONLINE", snap.devices.len()) };
            format!("{} \u{b7} GATEWAY {} \u{b7} {state}", a.subnet(), a.gateway)
        }
    }
}

/// The title, LIVE and the status line; the y of the accent line under them.
fn header(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View) -> f32 {
    let ink = pen.ink;
    let live = pen.text("LIVE", pen.mono(12.0, 9.5), STATUS_RED, pen.px(1.5), area.width());
    let title_width = (area.width() - live.size().x - pen.px(48.0)).max(40.0);
    let title = pen.text("NETWORK RADAR", pen.display(34.0, 20.0), ink.text, pen.px(1.2), title_width);
    let title_rect = pen.put(title.clone(), area.left_top(), Align2::LEFT_TOP);
    // Bold: the same words once more, a hair to the right.
    pen.p.galley(title_rect.min + vec2(0.8, 0.0), title, ink.text);
    let live_rect = pen.put(live, pos2(area.right(), title_rect.center().y), Align2::RIGHT_CENTER);
    let pulse = 0.55 + 0.45 * (0.5 + 0.5 * (pen.now * 3.2).sin() as f32);
    let dot = pos2(live_rect.left() - pen.px(10.0), live_rect.center().y);
    let r = pen.size(4.0, 3.0);
    pen.p.circle_filled(dot, r + 3.0, STATUS_RED.gamma_multiply(0.15 * pulse));
    pen.p.circle_filled(dot, r, STATUS_RED.gamma_multiply(pulse));
    let font = pen.mono(11.0, 9.0);
    view.subtitle.set(snap.phase, &subtitle(snap), pen.now);
    let y = title_rect.bottom() + pen.px(3.0);
    view.subtitle.paint(pen, pos2(area.left(), y), &font, ink.text.gamma_multiply(0.85), pen.px(1.2), area.width());
    let bottom = y + pen.row(&font) + pen.px(8.0);
    pen.p.line_segment([pos2(area.left(), bottom), pos2(area.right(), bottom)], stroke(1.0, ink.border));
    pen.p.line_segment([pos2(area.left(), bottom), pos2(area.left() + area.width() * 0.22, bottom)], stroke(2.0, ink.accent));
    bottom
}

/// The footer's line, longest first: the first that fits is shown.
fn footer_texts(snap: &Snapshot) -> Vec<String> {
    match snap.phase {
        Phase::Locating => vec!["LOCATING THE NETWORK..".to_string()],
        Phase::Sweeping => vec![
            format!("SWEEPING {} \u{b7} {} / {} \u{b7} {} FOUND", snap.scope, snap.probed, snap.total, snap.devices.len()),
            format!("SWEEPING \u{b7} {} / {}", snap.probed, snap.total),
        ],
        Phase::Done => {
            let took = snap.took.map_or_else(|| "SWEPT".to_string(), |t| format!("SWEPT IN {t:.1} S"));
            vec![
                format!("{took} \u{b7} CLOSER TO THE CENTRE = FASTER TO ANSWER \u{b7} CLICK A DEVICE FOR DETAILS \u{b7} RIGHT-CLICK: CLOSE"),
                format!("{took} \u{b7} CLOSER TO THE CENTRE = FASTER TO ANSWER \u{b7} RIGHT-CLICK: CLOSE"),
                format!("{took} \u{b7} CLOSER = FASTER \u{b7} RIGHT-CLICK: CLOSE"),
                took,
            ]
        }
        Phase::Failed => vec![format!("{} \u{b7} RIGHT-CLICK: CLOSE", snap.error.as_deref().unwrap_or("No network").to_uppercase()), "NO NETWORK".to_string()],
        Phase::Idle => vec!["RESCAN TO SWEEP \u{b7} RIGHT-CLICK: CLOSE".to_string(), "RIGHT-CLICK: CLOSE".to_string()],
    }
}

/// The footer (and the sweep's progress over it); the y the body ends at.
fn footer(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View) -> f32 {
    let font = pen.mono(10.5, 9.0);
    let spacing = pen.px(1.0);
    let variants = footer_texts(snap);
    let text = variants.iter().find(|t| pen.measure(t, &font, spacing) <= area.width()).or(variants.last()).cloned().unwrap_or_default();
    view.footer.set(snap.phase, &text, pen.now);
    let top = area.bottom() - pen.row(&font);
    view.footer.paint(pen, pos2(area.left(), top), &font, pen.ink.muted, spacing, area.width());
    if snap.phase == Phase::Sweeping && snap.total > 0 {
        let share = (snap.probed as f32 / snap.total as f32).clamp(0.0, 1.0);
        let y = top - pen.px(5.0);
        pen.p.line_segment([pos2(area.left(), y), pos2(area.right(), y)], stroke(1.0, pen.ink.border.gamma_multiply(0.5)));
        pen.p.line_segment([pos2(area.left(), y), pos2(area.left() + area.width() * share, y)], stroke(2.0, pen.ink.accent));
    }
    top - pen.px(8.0)
}

// ------------------------------------------------------------------------------ scope ----

/// The unit vector `degrees` clockwise from north (screen y points down).
fn dir(degrees: f32) -> Vec2 {
    let a = (degrees - 90.0).to_radians();
    vec2(a.cos(), a.sin())
}

/// A colour that adds light to what is under it.
fn additive(colour: Color32, strength: f32) -> Color32 {
    let s = strength.clamp(0.0, 1.0);
    let scale = |c: u8| (f32::from(c) * s).round() as u8;
    Color32::from_rgba_premultiplied(scale(colour.r()), scale(colour.g()), scale(colour.b()), 0)
}

/// The scope's faint glow, brightest in the middle.
fn glow_disc(p: &Painter, c: Pos2, r: f32, colour: Color32) {
    const STEPS: u32 = 64;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(c, additive(colour, 0.20));
    for ring in [(0.75, 0.09), (1.0, 0.035)] {
        for i in 0..STEPS {
            mesh.colored_vertex(c + dir(i as f32 * 360.0 / STEPS as f32) * r * ring.0, additive(colour, ring.1));
        }
    }
    for i in 0..STEPS {
        let j = (i + 1) % STEPS;
        mesh.add_triangle(0, 1 + i, 1 + j);
        let (a, b, a2, b2) = (1 + i, 1 + j, 1 + STEPS + i, 1 + STEPS + j);
        mesh.add_triangle(a, b, a2);
        mesh.add_triangle(b, b2, a2);
    }
    p.add(Shape::mesh(mesh));
}

/// The beam: a wedge in `colour` that fades towards its trailing edge, and a leading line in
/// the hot `edge` colour.
fn beam_wedge(p: &Painter, c: Pos2, r: f32, angle: f32, colour: Color32, edge: Color32) {
    const STEPS: u32 = 24;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(c, additive(colour, 0.10));
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        mesh.colored_vertex(c + dir(angle - BEAM_WIDTH * (1.0 - t)) * r, additive(colour, 0.42 * t.powf(1.6)));
    }
    for i in 0..STEPS {
        mesh.add_triangle(0, 1 + i, 2 + i);
    }
    p.add(Shape::mesh(mesh));
    let tip = c + dir(angle) * r;
    p.line_segment([c, tip], stroke(5.0, additive(colour, 0.22)));
    p.line_segment([c, tip], stroke(1.6, edge));
}

/// Pushes overlapping blips apart (and off the centre's labels), each kept between `inner`
/// and `outer` from the centre. The same input gives the same picture every frame.
fn relax(points: &mut [Pos2], c: Pos2, inner: f32, outer: f32, gap: f32, keep_out: &[Rect]) {
    for _ in 0..12 {
        for i in 0..points.len() {
            for j in i + 1..points.len() {
                let delta = points[j] - points[i];
                let dist = delta.length();
                if dist >= gap {
                    continue;
                }
                let away = points[i] - c;
                let push = if dist > 0.01 {
                    delta / dist
                } else if away.length() > 0.01 {
                    vec2(-away.y, away.x) / away.length()
                } else {
                    vec2(1.0, 0.0)
                };
                let shift = push * ((gap - dist) * 0.5);
                points[i] -= shift;
                points[j] += shift;
            }
        }
        for pt in points.iter_mut() {
            for rect in keep_out {
                let mut steps = 0;
                while rect.expand(gap * 0.5).contains(*pt) && steps < 64 {
                    let out = *pt - c;
                    *pt += if out.length() > 0.01 { out / out.length() } else { vec2(0.0, 1.0) } * 2.0;
                    steps += 1;
                }
            }
            let v = *pt - c;
            let len = v.length();
            let unit = if len > 0.01 { v / len } else { vec2(0.0, -1.0) };
            *pt = c + unit * len.clamp(inner, outer);
        }
    }
}

/// "<1 ms", "NO ECHO" (found through ARP), "LOCAL" (this PC).
fn latency_text(d: &Device) -> String {
    if d.this_pc {
        "LOCAL".to_string()
    } else if d.latency_ms.is_none() {
        "NO ECHO".to_string()
    } else {
        radar::fmt_ms(d.latency_ms)
    }
}

/// A device's label on the scope with no free spot of its own: shown on a backing while the
/// pointer is on the device.
struct Floating {
    rect: Rect,
    name: Arc<Galley>,
    latency: Arc<Galley>,
}

impl Floating {
    fn paint(self, pen: &Pen) {
        let back = self.rect.expand(pen.px(4.0));
        pen.p.rect_filled(back, Rounding::ZERO, BLOCK_SLATE);
        pen.p.rect_stroke(back, Rounding::ZERO, stroke(1.0, pen.ink.border));
        label(pen, self.rect, self.name, self.latency, Align::Center);
    }
}

/// A device's name and latency, stacked in `rect`.
fn label(pen: &Pen, rect: Rect, name: Arc<Galley>, latency: Arc<Galley>, align: Align) {
    let x = match align {
        Align::Min => rect.left(),
        Align::Center => rect.center().x,
        Align::Max => rect.right(),
    };
    let below = rect.top() + name.size().y;
    pen.put(name, pos2(x, rect.top()), Align2([align, Align::Min]));
    pen.put(latency, pos2(x, below), Align2([align, Align::Min]));
}

/// `rect` moved inside `bounds` (as far as it fits).
fn clamp_into(rect: Rect, bounds: Rect) -> Rect {
    let dx = if rect.left() < bounds.left() { bounds.left() - rect.left() } else { (bounds.right() - rect.right()).min(0.0) };
    let dy = if rect.top() < bounds.top() { bounds.top() - rect.top() } else { (bounds.bottom() - rect.bottom()).min(0.0) };
    rect.translate(vec2(dx, dy))
}

/// The scope: rings, ticks, the beam, this PC in the centre, every device the beam has found.
fn scope(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View, card: Option<Rect>, actions: &mut Actions) -> Option<Floating> {
    let (ink, p) = (pen.ink, &pen.p);
    let ticks = pen.k >= 0.7;
    let room = if ticks { pen.px(30.0) } else { pen.px(12.0) };
    let r = (area.width().min(area.height()) / 2.0 - room).max(30.0);
    let c = area.center();
    glow_disc(p, c, r, ink.accent);
    for ms in RINGS_MS {
        p.circle_stroke(c, r * radar::radius_fraction(Some(ms)), stroke(1.0, ink.border.gamma_multiply(0.7)));
    }
    let cross = stroke(1.0, ink.border.gamma_multiply(0.4));
    p.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], cross);
    p.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], cross);
    // Ticks every 10 degrees, a bearing every 30.
    // The details card covers a corner: no label goes under it.
    let mut obstacles: Vec<Rect> = card.into_iter().collect();
    let tick_font = pen.mono(9.0, 8.5);
    for i in 0..36u32 {
        let d = dir(i as f32 * 10.0);
        let major = i % 3 == 0;
        let length = if major { pen.px(9.0) } else { pen.px(6.0) };
        p.line_segment([c + d * (r + pen.px(3.0)), c + d * (r + length)], stroke(1.0, if major { ink.border } else { ink.border.gamma_multiply(0.5) }));
        if major && ticks {
            let g = pen.text(&format!("{:03}", i * 10), tick_font.clone(), ink.muted, 0.0, 100.0);
            let at = c + d * (r + pen.px(19.0));
            let rect = Align2::CENTER_CENTER.anchor_size(at, g.size());
            if area.contains_rect(rect) {
                pen.put(g, at, Align2::CENTER_CENTER);
                obstacles.push(rect);
            }
        }
    }
    // The outer ring glows, and flashes when the sweep completes.
    let flash = view.flash_at.map_or(0.0, |t| 1.0 - ((pen.now - t) / COMPLETE_FLASH).clamp(0.0, 1.0) as f32);
    let ring = ink.accent.lerp_to_gamma(ink.hot, flash);
    p.circle_stroke(c, r + 2.0, stroke(pen.size(8.0, 4.0), additive(ring, 0.10 + 0.30 * flash)));
    p.circle_stroke(c, r, stroke(4.0, ring.gamma_multiply(0.22 + 0.3 * flash)));
    p.circle_stroke(c, r, stroke(1.6 + flash, ring));
    // The rings' latencies, right of the centre.
    let ring_font = pen.mono(9.0, 8.5);
    for ms in RINGS_MS {
        let g = pen.text(&format!("{} ms", ms as u32), ring_font.clone(), ink.muted.gamma_multiply(0.85), 0.0, 100.0);
        let at = c + vec2(r * radar::radius_fraction(Some(ms)) + pen.px(3.0), -pen.px(2.0));
        let rect = Align2::LEFT_BOTTOM.anchor_size(at, g.size());
        if area.contains_rect(rect) && !obstacles.iter().any(|o| o.intersects(rect)) {
            pen.put(g, at, Align2::LEFT_BOTTOM);
            obstacles.push(rect);
        }
    }
    beam_wedge(p, c, r, view.beam(pen.now), ink.accent, ink.hot);
    // This PC in the centre.
    let cr = pen.size(13.0, 9.0);
    p.circle_filled(c, cr + pen.px(7.0), additive(ink.hot, 0.07));
    p.circle_filled(c, cr, BLOCK_SLATE);
    p.circle_stroke(c, cr, stroke(1.5, ink.hot));
    glyph(p, Kind::ThisPc, c, cr * 0.55, ink.hot);
    let mut centre = Vec::new();
    let tag = pen.text("THIS PC", pen.bold(10.0, 8.5), ink.text, pen.px(1.2), 120.0);
    let tag_rect = pen.put(tag, c + vec2(0.0, cr + pen.px(4.0)), Align2::CENTER_TOP);
    centre.push(tag_rect);
    if let Some(a) = &snap.adapter {
        let g = pen.text(&a.ip.to_string(), pen.mono(9.5, 8.5), ink.muted, 0.0, 160.0);
        centre.push(pen.put(g, pos2(c.x, tag_rect.bottom() + 1.0), Align2::CENTER_TOP));
    }
    if snap.sweeping() {
        let pulse = 0.55 + 0.45 * (0.5 + 0.5 * (pen.now * 4.0).sin() as f32);
        let g = pen.text("SWEEPING", pen.mono(9.0, 8.5), ink.accent.gamma_multiply(pulse), pen.px(1.4), 160.0);
        let below = centre.last().map_or(tag_rect.bottom(), |r: &Rect| r.bottom());
        centre.push(pen.put(g, pos2(c.x, below + pen.px(3.0)), Align2::CENTER_TOP));
    }
    obstacles.extend(centre.iter().copied());
    // No device's name over the centre node either.
    obstacles.push(Rect::from_center_size(c, Vec2::splat(2.0 * cr + 4.0)));
    // The devices, where their latency puts them.
    let others: Vec<&Device> = snap.devices.iter().filter(|d| !d.this_pc).collect();
    let angles: Vec<(Ipv4Addr, f32)> = others.iter().map(|d| (d.ip, radar::device_angle(d))).collect();
    reveal(view, &angles, pen.now);
    let br = pen.size(11.0, 8.0);
    let inner = (cr + br + pen.px(6.0)).min(r * 0.9);
    let mut points: Vec<Pos2> = others.iter().zip(&angles).map(|(d, (_, a))| c + dir(*a) * (r * radar::radius_fraction(d.latency_ms)).max(inner)).collect();
    relax(&mut points, c, inner, r * 0.95, br * 2.0 + pen.px(3.0), &centre);
    let mut shown: Vec<Option<bool>> = Vec::with_capacity(others.len());
    for (n, d) in others.iter().enumerate() {
        let Some(blip) = view.blips.get(&d.ip).copied().filter(|b| b.found.is_some()) else {
            shown.push(None);
            continue;
        };
        let found = blip.found.unwrap_or(pen.now);
        let at = points[n];
        let colour = ink.kind(d.kind);
        let response = pen.click(Rect::from_center_size(at, Vec2::splat(br * 2.0 + 6.0)), ("blip", d.ip), actions).on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            view.selected = Some(d.ip);
        }
        let age = pen.now - found;
        let fill = (age / REVEAL).clamp(0.0, 1.0) as f32;
        if age < FOUND_HALO {
            let t = (age / FOUND_HALO) as f32;
            p.circle_stroke(at, br + 2.0 + t * pen.px(14.0), stroke(1.5, colour.gamma_multiply(0.7 * (1.0 - t))));
        }
        if let Some(t) = blip.pass.map(|t| ((pen.now - t) / PASS_GLOW) as f32).filter(|t| *t < 1.0) {
            p.circle_filled(at, br + 2.0 + 6.0 * (1.0 - t), additive(colour, 0.28 * (1.0 - t)));
        }
        p.circle_filled(at, br, BLOCK_SLATE);
        if fill > 0.0 {
            p.circle_filled(at, br, colour.gamma_multiply(0.24 * fill));
        }
        p.circle_stroke(at, br, stroke(1.5, colour));
        if fill > 0.35 {
            glyph(p, d.kind, at, br * 0.55, colour.gamma_multiply(fill));
        }
        if response.hovered() || view.selected == Some(d.ip) {
            p.circle_stroke(at, br + 4.0, stroke(1.0, ink.bracket));
        }
        shown.push(Some(response.hovered()));
    }
    // Their names: under each device, else above, right or left; none where nothing is free.
    let (name_font, latency_font) = (pen.bold(11.5, 9.5), pen.mono(9.5, 8.5));
    let circles: Vec<Rect> = points.iter().map(|pt| Rect::from_center_size(*pt, Vec2::splat(br * 2.0 + 2.0))).collect();
    let bounds = area.shrink(2.0);
    let mut placed: Vec<Rect> = Vec::new();
    let mut floating = None;
    for (n, d) in others.iter().enumerate() {
        let Some(hovered) = shown[n] else { continue };
        let name = pen.text(&d.name(), name_font.clone(), ink.text, 0.0, pen.px(150.0).max(80.0));
        let latency = pen.text(&latency_text(d), latency_font.clone(), ink.kind(d.kind), 0.0, 120.0);
        let size = vec2(name.size().x.max(latency.size().x), name.size().y + latency.size().y);
        let (at, gap) = (points[n], pen.px(3.0));
        let spots = [
            Rect::from_min_size(pos2(at.x - size.x / 2.0, at.y + br + gap), size),
            Rect::from_min_size(pos2(at.x - size.x / 2.0, at.y - br - gap - size.y), size),
            Rect::from_min_size(pos2(at.x + br + gap * 2.0, at.y - size.y / 2.0), size),
            Rect::from_min_size(pos2(at.x - br - gap * 2.0 - size.x, at.y - size.y / 2.0), size),
        ];
        let free = |rect: &Rect| {
            bounds.contains_rect(*rect) && !obstacles.iter().chain(&placed).any(|o| o.intersects(*rect)) && !circles.iter().enumerate().any(|(m, other)| m != n && other.intersects(*rect))
        };
        match spots.iter().position(free) {
            Some(i) => {
                let align = match i {
                    2 => Align::Min,
                    3 => Align::Max,
                    _ => Align::Center,
                };
                label(pen, spots[i], name, latency, align);
                placed.push(spots[i]);
            }
            None if hovered && floating.is_none() => floating = Some(Floating { rect: clamp_into(spots[0], bounds), name, latency }),
            None => {}
        }
    }
    floating
}

/// A small line drawing of what a device is, `g` about half its circle.
fn glyph(p: &Painter, kind: Kind, c: Pos2, g: f32, colour: Color32) {
    let s = stroke((g * 0.16).clamp(1.0, 1.6), colour);
    let dot = |at: Pos2| p.circle_filled(at, (g * 0.11).max(0.8), colour);
    match kind {
        Kind::Router => {
            let body = Rect::from_center_size(c + vec2(0.0, g * 0.35), vec2(g * 1.7, g * 0.7));
            p.rect_stroke(body, Rounding::ZERO, s);
            p.line_segment([body.left_top() + vec2(g * 0.3, 0.0), body.left_top() + vec2(g * 0.05, -g * 0.85)], s);
            p.line_segment([body.right_top() - vec2(g * 0.3, 0.0), body.right_top() + vec2(-g * 0.05, -g * 0.85)], s);
            dot(body.center());
            dot(body.center() + vec2(g * 0.45, 0.0));
        }
        Kind::Phone | Kind::Tablet => {
            let size = if kind == Kind::Phone { vec2(g * 0.95, g * 1.7) } else { vec2(g * 1.45, g * 1.75) };
            let body = Rect::from_center_size(c, size);
            p.rect_stroke(body, Rounding::same(g * 0.18), s);
            dot(pos2(c.x, body.bottom() - g * 0.28));
        }
        Kind::Pc | Kind::ThisPc => {
            let screen = Rect::from_center_size(c - vec2(0.0, g * 0.2), vec2(g * 1.8, g * 1.15));
            p.rect_stroke(screen, Rounding::ZERO, s);
            p.line_segment([pos2(c.x, screen.bottom()), pos2(c.x, screen.bottom() + g * 0.4)], s);
            p.line_segment([pos2(c.x - g * 0.45, screen.bottom() + g * 0.42), pos2(c.x + g * 0.45, screen.bottom() + g * 0.42)], s);
        }
        Kind::Tv => {
            let screen = Rect::from_center_size(c - vec2(0.0, g * 0.15), vec2(g * 2.0, g * 1.2));
            p.rect_stroke(screen, Rounding::ZERO, s);
            p.line_segment([pos2(c.x - g * 0.5, screen.bottom()), pos2(c.x - g * 0.7, screen.bottom() + g * 0.35)], s);
            p.line_segment([pos2(c.x + g * 0.5, screen.bottom()), pos2(c.x + g * 0.7, screen.bottom() + g * 0.35)], s);
        }
        Kind::SmartHome => {
            p.add(Shape::line(vec![c + vec2(-g * 0.9, -g * 0.05), c + vec2(0.0, -g * 0.9), c + vec2(g * 0.9, -g * 0.05)], s));
            p.rect_stroke(Rect::from_min_max(c + vec2(-g * 0.6, -g * 0.25), c + vec2(g * 0.6, g * 0.8)), Rounding::ZERO, s);
            p.rect_stroke(Rect::from_min_max(c + vec2(-g * 0.17, g * 0.3), c + vec2(g * 0.17, g * 0.8)), Rounding::ZERO, s);
        }
        Kind::Printer => {
            p.rect_stroke(Rect::from_center_size(c + vec2(0.0, g * 0.15), vec2(g * 1.8, g * 0.8)), Rounding::ZERO, s);
            p.rect_stroke(Rect::from_min_max(c + vec2(-g * 0.5, -g * 0.85), c + vec2(g * 0.5, -g * 0.25)), Rounding::ZERO, s);
            p.line_segment([c + vec2(-g * 0.5, g * 0.8), c + vec2(g * 0.5, g * 0.8)], s);
        }
        Kind::Console => {
            p.rect_stroke(Rect::from_center_size(c, vec2(g * 1.9, g * 1.05)), Rounding::same(g * 0.4), s);
            p.line_segment([c + vec2(-g * 0.75, 0.0), c + vec2(-g * 0.25, 0.0)], s);
            p.line_segment([c + vec2(-g * 0.5, -g * 0.25), c + vec2(-g * 0.5, g * 0.25)], s);
            dot(c + vec2(g * 0.4, -g * 0.12));
            dot(c + vec2(g * 0.65, g * 0.12));
        }
        Kind::Unknown => {
            p.add(Shape::closed_line(vec![c + vec2(0.0, -g * 0.85), c + vec2(g * 0.85, 0.0), c + vec2(0.0, g * 0.85), c + vec2(-g * 0.85, 0.0)], s));
            dot(c);
        }
    }
}

/// White L-corners, `arm` long.
fn corner_brackets(p: &Painter, rect: Rect, arm: f32, colour: Color32) {
    let s = stroke(1.5, colour);
    for (corner, dx, dy) in [(rect.left_top(), 1.0, 1.0), (rect.right_top(), -1.0, 1.0), (rect.left_bottom(), 1.0, -1.0), (rect.right_bottom(), -1.0, -1.0)] {
        p.line_segment([corner, corner + vec2(arm * dx, 0.0)], s);
        p.line_segment([corner, corner + vec2(0.0, arm * dy)], s);
    }
}

// ----------------------------------------------------------------------------- column ----

/// The right side: the two cards, the device list, the latency chart or the speed test.
fn column(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View, actions: &mut Actions) {
    let gap = pen.px(12.0);
    let mut top = area.top();
    let card_h = card_height(pen);
    if area.height() >= card_h * 3.0 {
        let w = (area.width() - gap) / 2.0;
        adapter_card(pen, Rect::from_min_size(pos2(area.left(), top), vec2(w, card_h)), snap);
        internet_card(pen, Rect::from_min_size(pos2(area.right() - w, top), vec2(w, card_h)), snap);
        top += card_h + gap;
    }
    top += list_header(pen, Rect::from_min_max(pos2(area.left(), top), area.max), snap, actions) + pen.px(6.0);
    let tabs_h = tabs_height(pen, area.height());
    let fits_both = area.bottom() - top - tabs_h - gap >= row_height(pen) * 2.0;
    let list_bottom = if fits_both { area.bottom() - tabs_h - gap } else { area.bottom() };
    device_list(pen, Rect::from_min_max(pos2(area.left(), top), pos2(area.right(), list_bottom)), snap, view, actions);
    if fits_both {
        tabs(pen, Rect::from_min_max(pos2(area.left(), area.bottom() - tabs_h), area.max), snap, view, actions);
    }
}

fn card_height(pen: &Pen) -> f32 {
    2.0 * pen.size(9.0, 5.0) + pen.row(&pen.mono(10.0, 8.5)) + pen.px(3.0) + pen.row(&pen.display(22.0, 14.0)) + pen.px(2.0) + pen.row(&pen.plain(11.0, 9.0))
}

/// A card: a caption with a value in the accent right of it, a big value, a line under it.
fn card(pen: &Pen, rect: Rect, caption: &str, corner: &str, big: &str, big_colour: Color32, sub: &str) {
    let ink = pen.ink;
    pen.p.rect_filled(rect, Rounding::ZERO, rgba([3, 7, 12], 0.85));
    pen.p.rect_stroke(rect, Rounding::ZERO, stroke(1.0, ink.border));
    pen.p.line_segment([rect.left_top(), rect.left_top() + vec2(pen.px(18.0), 0.0)], stroke(2.0, ink.accent));
    let inner = rect.shrink(pen.size(9.0, 5.0));
    let caption = pen.text(caption, pen.mono(10.0, 8.5), ink.muted, pen.px(1.2), inner.width() * 0.6);
    let caption_rect = pen.put(caption, inner.left_top(), Align2::LEFT_TOP);
    let room = inner.width() - caption_rect.width() - pen.px(8.0);
    if !corner.is_empty() && room > 16.0 {
        pen.put(pen.text(corner, pen.mono(10.0, 8.5), ink.accent, pen.px(0.6), room), inner.right_top(), Align2::RIGHT_TOP);
    }
    if big.is_empty() {
        return;
    }
    let big = pen.text(big, pen.display(22.0, 14.0), big_colour, pen.px(0.8), inner.width());
    let big_rect = pen.put(big, pos2(inner.left(), caption_rect.bottom() + pen.px(3.0)), Align2::LEFT_TOP);
    if !sub.is_empty() {
        pen.put(pen.text(sub, pen.plain(11.0, 9.0), ink.muted, 0.0, inner.width()), pos2(inner.left(), big_rect.bottom() + pen.px(2.0)), Align2::LEFT_TOP);
    }
}

/// The link: its kind, the router's latency, the link speed, the adapter's name. Only the
/// caption while the network is being located.
fn adapter_card(pen: &Pen, rect: Rect, snap: &Snapshot) {
    match &snap.adapter {
        Some(a) => {
            let speed = radar::fmt_speed(a.speed_bps);
            let big = if speed.is_empty() { "--" } else { speed.as_str() };
            card(pen, rect, a.link.label(), &format!("ROUTER {}", radar::fmt_ms(snap.router_latency())), big, pen.ink.text, &a.description);
        }
        None if snap.phase == Phase::Failed => card(pen, rect, "NETWORK", "", "OFFLINE", STATUS_RED, snap.error.as_deref().unwrap_or("")),
        None => card(pen, rect, "NETWORK", "", "", pen.ink.muted, ""),
    }
}

fn internet_card(pen: &Pen, rect: Rect, snap: &Snapshot) {
    let (big, colour) = match snap.internet_latency() {
        Some(ms) => (radar::fmt_ms(Some(ms)), pen.ink.text),
        None if snap.internet.samples.len() >= 3 => ("OFFLINE".to_string(), STATUS_RED),
        None if snap.adapter.is_none() => (String::new(), pen.ink.muted),
        None => ("CHECKING..".to_string(), pen.ink.muted),
    };
    let sub = match (&snap.speed, &snap.adapter) {
        (Speed::Done { mbps, .. }, _) => format!("Speed test {}", radar::fmt_mbps(*mbps)),
        (_, Some(a)) => format!("Via gateway {}", a.gateway),
        _ => String::new(),
    };
    card(pen, rect, "INTERNET", &radar::INTERNET_HOST.to_string(), &big, colour, &sub);
}

/// "DEVICES · 6 ONLINE" and the RESCAN button; its height.
fn list_header(pen: &Pen, area: Rect, snap: &Snapshot, actions: &mut Actions) -> f32 {
    let ink = pen.ink;
    let sweeping = snap.sweeping();
    let word = pen.text(if sweeping { "SWEEPING" } else { "RESCAN" }, pen.mono(10.5, 9.0), if sweeping { ink.muted } else { ink.text }, pen.px(1.2), 200.0);
    let icon = pen.size(10.0, 8.0);
    let button_size = vec2(word.size().x + icon + pen.px(26.0), word.size().y + pen.px(8.0));
    let title = pen.text("DEVICES", pen.mono(10.5, 9.0), ink.muted, pen.px(1.4), (area.width() - button_size.x - pen.px(12.0)).max(30.0));
    let h = title.size().y.max(button_size.y);
    let mid = area.top() + h / 2.0;
    let title_rect = pen.put(title, pos2(area.left(), mid), Align2::LEFT_CENTER);
    let button = Rect::from_center_size(pos2(area.right() - button_size.x / 2.0, mid), button_size);
    let response = pen.click(button, "rescan", actions).on_hover_cursor(if sweeping { CursorIcon::Default } else { CursorIcon::PointingHand });
    let lit = response.hovered() && !sweeping;
    pen.p.rect_filled(button, Rounding::ZERO, if lit { ink.accent.gamma_multiply(0.18) } else { rgba([3, 7, 12], 0.9) });
    pen.p.rect_stroke(button, Rounding::ZERO, stroke(1.0, if lit { ink.accent } else { ink.border }));
    let spin = if sweeping { (pen.now * 240.0).rem_euclid(360.0) as f32 } else { 0.0 };
    rescan_icon(&pen.p, pos2(button.left() + pen.px(9.0) + icon / 2.0, mid), icon / 2.0, if sweeping { ink.muted } else { ink.accent }, spin);
    pen.put(word, pos2(button.right() - pen.px(9.0), mid), Align2::RIGHT_CENTER);
    if response.clicked() && !sweeping {
        actions.rescan = true;
    }
    let count = if sweeping { format!("\u{b7} {} FOUND", snap.devices.len()) } else { format!("\u{b7} {} ONLINE", snap.devices.len()) };
    let room = button.left() - title_rect.right() - pen.px(18.0);
    if room > 24.0 {
        pen.put(pen.text(&count, pen.mono(10.5, 9.0), ink.accent, pen.px(1.0), room), pos2(title_rect.right() + pen.px(10.0), mid), Align2::LEFT_CENTER);
    }
    h
}

/// The circular arrow of RESCAN (turning while a sweep runs).
fn rescan_icon(p: &Painter, c: Pos2, r: f32, colour: Color32, spin: f32) {
    let points: Vec<Pos2> = (0..=20).map(|i| c + dir(spin + 40.0 + i as f32 * 14.0) * r).collect();
    let end = c + dir(spin + 320.0) * r;
    p.add(Shape::line(points, stroke(1.4, colour)));
    let (along, out) = (dir(spin + 410.0), dir(spin + 320.0));
    p.add(Shape::convex_polygon(vec![end + along * r * 0.6, end + out * r * 0.45, end - out * r * 0.45], colour, stroke(0.0, Color32::TRANSPARENT)));
}

fn row_height(pen: &Pen) -> f32 {
    2.0 * pen.px(4.0) + pen.row(&pen.bold(13.5, 10.5)) + pen.px(1.0) + pen.row(&pen.plain(11.0, 9.0))
}

/// The device rows (whole rows only; the wheel scrolls them).
fn device_list(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View, actions: &mut Actions) {
    let ink = pen.ink;
    let shown = view.shown.min(snap.devices.len());
    if shown == 0 {
        let text = match snap.phase {
            Phase::Failed => "NO NETWORK TO SWEEP",
            Phase::Done | Phase::Idle if snap.devices.is_empty() => "NO DEVICES FOUND",
            _ => "SWEEPING THE NETWORK..",
        };
        let g = pen.text(text, pen.mono(10.5, 9.0), ink.muted, pen.px(1.2), area.width());
        if area.height() >= g.size().y + pen.px(8.0) {
            pen.put(g, area.left_top() + vec2(0.0, pen.px(8.0)), Align2::LEFT_TOP);
        }
        return;
    }
    let row_h = row_height(pen);
    let fits = ((area.height() / row_h).floor() as usize).max(1);
    let overflow = shown > fits;
    if overflow && pen.ui.rect_contains_pointer(area) {
        let wheel = pen.ui.input(|i| i.raw_scroll_delta.y);
        if wheel < -0.5 {
            view.scroll += 1;
        } else if wheel > 0.5 {
            view.scroll = view.scroll.saturating_sub(1);
        }
    }
    view.scroll = view.scroll.min(shown.saturating_sub(fits));
    let width = if overflow { area.width() - pen.px(10.0) } else { area.width() };
    for (slot, index) in (view.scroll..shown.min(view.scroll + fits)).enumerate() {
        let rect = Rect::from_min_size(pos2(area.left(), area.top() + slot as f32 * row_h), vec2(width, row_h));
        device_row(pen, rect, &snap.devices[index], view, actions);
    }
    if overflow {
        let track = Rect::from_min_max(pos2(area.right() - pen.px(4.0), area.top()), pos2(area.right() - pen.px(1.0), area.top() + fits as f32 * row_h));
        pen.p.rect_filled(track, Rounding::ZERO, ink.border.gamma_multiply(0.35));
        let thumb = Rect::from_min_size(pos2(track.left(), track.top() + track.height() * view.scroll as f32 / shown as f32), vec2(track.width(), track.height() * fits as f32 / shown as f32));
        pen.p.rect_filled(thumb, Rounding::ZERO, ink.accent);
    }
}

fn device_row(pen: &Pen, rect: Rect, d: &Device, view: &mut View, actions: &mut Actions) {
    let ink = pen.ink;
    let response = pen.click(rect, ("row", d.ip), actions).on_hover_cursor(CursorIcon::PointingHand);
    if response.clicked() {
        view.selected = if view.selected == Some(d.ip) { None } else { Some(d.ip) };
    }
    if view.selected == Some(d.ip) {
        pen.p.rect_filled(rect, Rounding::ZERO, ink.accent.gamma_multiply(0.14));
        pen.p.line_segment([rect.left_top(), rect.left_bottom()], stroke(2.0, ink.accent));
    } else if response.hovered() {
        pen.p.rect_filled(rect, Rounding::ZERO, ink.accent.gamma_multiply(0.07));
    }
    pen.p.line_segment([rect.left_bottom(), rect.right_bottom()], stroke(1.0, ink.border.gamma_multiply(0.35)));
    // The kind's line drawing in the accent (the scope shows the kinds' colours).
    let gr = pen.size(11.0, 8.0);
    let gc = pos2(rect.left() + pen.px(6.0) + gr, rect.center().y);
    glyph(&pen.p, d.kind, gc, gr * 0.75, ink.accent);
    // Right: the address over the latency and its bar.
    let pad = pen.px(4.0);
    let right = rect.right() - pen.px(8.0);
    let ip = pen.text(&d.ip.to_string(), pen.mono(10.5, 9.0), ink.text, 0.0, 200.0);
    let right_w = ip.size().x.max(pen.px(100.0));
    pen.put(ip, pos2(right, rect.top() + pad), Align2::RIGHT_TOP);
    let bar_w = pen.size(34.0, 20.0);
    let latency = pen.text(&latency_text(d), pen.mono(9.5, 8.5), ink.muted, 0.0, right_w - bar_w - pen.px(6.0));
    let latency_rect = pen.put(latency, pos2(right - bar_w - pen.px(6.0), rect.bottom() - pad), Align2::RIGHT_BOTTOM);
    let bar = Rect::from_min_size(pos2(right - bar_w, latency_rect.center().y - 1.5), vec2(bar_w, 3.0));
    pen.p.rect_filled(bar, Rounding::ZERO, ink.border.gamma_multiply(0.45));
    let share = if d.this_pc { 0.0 } else { radar::radius_fraction(d.latency_ms) };
    pen.p.rect_filled(Rect::from_min_size(bar.min, vec2(bar_w * share, 3.0)), Rounding::ZERO, ink.accent);
    // Left: the name and its tags over what it is.
    let left = gc.x + gr + pen.px(10.0);
    let text_w = (right - right_w - pen.px(10.0) - left).max(30.0);
    let tag_font = pen.mono(9.0, 8.0);
    let tags: Vec<(Arc<Galley>, Color32)> = [(d.this_pc, "THIS PC", ink.hot), (d.new, "NEW", STATUS_RED)]
        .into_iter()
        .filter(|(on, ..)| *on)
        .map(|(_, tag, c)| (pen.text(tag, tag_font.clone(), c, pen.px(1.0), 100.0), c))
        .collect();
    let tags_w: f32 = pen.px(6.0) + tags.iter().map(|(g, _)| g.size().x + pen.px(16.0)).sum::<f32>();
    let name = pen.text(&d.name(), pen.bold(13.5, 10.5), ink.text, 0.0, (text_w - if tags.is_empty() { 0.0 } else { tags_w }).max(24.0));
    let name_rect = pen.put(name, pos2(left, rect.top() + pad), Align2::LEFT_TOP);
    let mut x = name_rect.right() + pen.px(10.0);
    for (g, c) in tags {
        let r = Align2::LEFT_CENTER.anchor_size(pos2(x + pen.px(4.0), name_rect.center().y), g.size());
        pen.p.rect_stroke(r.expand2(vec2(pen.px(4.0), 1.0)), Rounding::ZERO, stroke(1.0, c));
        pen.put(g, r.min, Align2::LEFT_TOP);
        x = r.right() + pen.px(12.0);
    }
    pen.put(pen.text(&d.subline(), pen.plain(11.0, 9.0), ink.muted, 0.0, text_w), pos2(left, rect.bottom() - pad), Align2::LEFT_BOTTOM);
}

fn legend_fonts(pen: &Pen) -> (FontId, FontId) {
    (pen.mono(10.0, 8.5), pen.bold(12.5, 9.5))
}

fn legend_height(pen: &Pen) -> f32 {
    let (label, value) = legend_fonts(pen);
    pen.row(&label).max(pen.row(&value))
}

fn tabs_height(pen: &Pen, column: f32) -> f32 {
    let chart = (column * 0.2).clamp(48.0, pen.px(120.0).max(48.0));
    pen.row(&pen.mono(11.0, 9.0)) + pen.px(8.0) + pen.px(8.0) + legend_height(pen) + pen.px(8.0) + chart
}

/// LATENCY | SPEED TEST and what the chosen one shows.
fn tabs(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View, actions: &mut Actions) {
    let ink = pen.ink;
    let font = pen.mono(11.0, 9.0);
    let tab_h = pen.row(&font) + pen.px(8.0);
    let pad = pen.px(8.0);
    let mut x = area.left();
    for (tab, word) in [(Tab::Latency, "LATENCY"), (Tab::Speed, "SPEED TEST")] {
        let active = view.tab == tab;
        let g = pen.text(word, font.clone(), if active { ink.text } else { ink.muted }, pen.px(1.4), area.width() / 2.0);
        let rect = Rect::from_min_size(pos2(x, area.top()), vec2(g.size().x + 2.0 * pad, tab_h));
        let response = pen.click(rect, ("tab", word), actions).on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            view.tab = tab;
        }
        if active {
            pen.p.rect_filled(rect, Rounding::ZERO, ink.accent.gamma_multiply(0.16));
        }
        pen.put(g, pos2(rect.left() + pad, rect.center().y), Align2::LEFT_CENTER);
        if active || response.hovered() {
            pen.p.line_segment([rect.left_bottom(), rect.right_bottom()], stroke(2.0, if active { ink.accent } else { ink.border }));
        }
        x = rect.right() + pen.px(4.0);
    }
    let note = if view.tab == Tab::Latency { "LAST 60 S" } else { "25 MB \u{b7} CLOUDFLARE" };
    let room = area.right() - x;
    if room > 40.0 {
        pen.put(pen.text(note, pen.mono(9.5, 8.5), ink.muted, pen.px(1.0), room), area.right_top(), Align2::RIGHT_TOP);
    }
    pen.p.line_segment([pos2(area.left(), area.top() + tab_h), pos2(area.right(), area.top() + tab_h)], stroke(1.0, ink.border.gamma_multiply(0.5)));
    let content = Rect::from_min_max(pos2(area.left(), area.top() + tab_h + pen.px(8.0)), area.max);
    match view.tab {
        Tab::Latency => latency(pen, content, snap),
        Tab::Speed => speed(pen, content, snap, actions),
    }
}

/// One entry of the legend: a dot in the series' colour (or none), a caption, its value.
struct Entry {
    dot: Option<Color32>,
    caption: &'static str,
    value: String,
}

/// The legend over the chart of the last minute: the two series on the left, loss and jitter
/// on the right (as many of those as fit).
fn latency(pen: &Pen, area: Rect, snap: &Snapshot) {
    let ink = pen.ink;
    let (caption_font, value_font) = legend_fonts(pen);
    let series = [
        Entry { dot: Some(ink.accent), caption: "ROUTER", value: radar::fmt_ms(snap.router_latency()) },
        Entry { dot: Some(ink.second), caption: "INTERNET", value: radar::fmt_ms(snap.internet_latency()) },
    ];
    let mut quality = Vec::new();
    if let Some(loss) = snap.internet.loss() {
        quality.push(Entry { dot: None, caption: "LOSS", value: format!("{loss:.0}%") });
    }
    if let Some(jitter) = snap.internet.jitter() {
        quality.push(Entry { dot: None, caption: "JITTER", value: format!("{jitter:.1} ms") });
    }
    let (dot, gap, inner, spacing) = (pen.size(3.5, 2.5), pen.px(18.0), pen.px(6.0), pen.px(0.8));
    let width = |e: &Entry| {
        let marker = if e.dot.is_some() { dot * 2.0 + pen.px(6.0) } else { 0.0 };
        marker + pen.measure(e.caption, &caption_font, spacing) + inner + pen.measure(&e.value, &value_font, 0.0)
    };
    let span = |entries: &[Entry]| entries.iter().map(width).sum::<f32>() + gap * entries.len().saturating_sub(1) as f32;
    let left_w = span(&series);
    while !quality.is_empty() && left_w + gap * 1.5 + span(&quality) > area.width() {
        quality.pop();
    }
    let row = legend_height(pen);
    let cy = area.top() + row / 2.0;
    let draw = |e: &Entry, mut x: f32| {
        if let Some(c) = e.dot {
            pen.p.circle_filled(pos2(x + dot, cy), dot, c);
            x += dot * 2.0 + pen.px(6.0);
        }
        let room = (area.right() - x).max(10.0);
        let caption = pen.put(pen.text(e.caption, caption_font.clone(), ink.muted, spacing, room), pos2(x, cy), Align2::LEFT_CENTER);
        let x = caption.right() + inner;
        if area.right() - x > 10.0 {
            pen.put(pen.text(&e.value, value_font.clone(), ink.text, 0.0, area.right() - x), pos2(x, cy), Align2::LEFT_CENTER);
        }
    };
    let mut x = area.left();
    for e in &series {
        if x >= area.right() - 10.0 {
            break;
        }
        draw(e, x);
        x += width(e) + gap;
    }
    let mut x = area.right() - span(&quality);
    for e in &quality {
        draw(e, x);
        x += width(e) + gap;
    }
    let chart = Rect::from_min_max(pos2(area.left(), area.top() + row + pen.px(8.0)), area.max);
    if chart.height() >= 24.0 {
        line_chart(pen, chart, &[(&snap.router, ink.accent), (&snap.internet, ink.second)]);
    }
}

/// The smallest round number at or above `v` the chart's scale ends at.
fn nice_ceiling(v: f32) -> f32 {
    [5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0].into_iter().find(|n| *n >= v).unwrap_or_else(|| v.ceil())
}

fn line_chart(pen: &Pen, area: Rect, series: &[(&Series, Color32)]) {
    let ink = pen.ink;
    let font = pen.mono(9.0, 8.0);
    let max = series.iter().flat_map(|(s, _)| s.samples.iter().flatten()).fold(0.0f32, |m, v| m.max(*v));
    let top = nice_ceiling(max.max(5.0));
    let label_w = pen.measure(&format!("{top:.0} ms"), &font, 0.0) + pen.px(6.0);
    let half = pen.row(&font) / 2.0 + 1.0;
    let plot = Rect::from_min_max(pos2(area.left() + label_w, area.top() + half), pos2(area.right(), area.bottom() - half));
    if plot.height() < 8.0 || plot.width() < 40.0 {
        return;
    }
    let lines: &[f32] = if plot.height() >= 4.0 * half + 4.0 { &[0.0, 0.5, 1.0] } else { &[0.0, 1.0] };
    for share in lines {
        let y = plot.bottom() - plot.height() * share;
        pen.p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], stroke(1.0, ink.border.gamma_multiply(if *share == 0.0 { 0.6 } else { 0.3 })));
        let value = top * share;
        let text = if *share == 0.0 { "0".to_string() } else if value.fract() == 0.0 { format!("{value:.0} ms") } else { format!("{value:.1} ms") };
        pen.put(pen.text(&text, font.clone(), ink.muted, 0.0, label_w), pos2(plot.left() - pen.px(6.0), y), Align2::RIGHT_CENTER);
    }
    let step = plot.width() / (radar::LATENCY_SAMPLES - 1) as f32;
    for (s, colour) in series {
        let n = s.samples.len();
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            match run.len() {
                0 => {}
                1 => {
                    pen.p.circle_filled(run[0], 1.5, *colour);
                }
                _ => {
                    pen.p.add(Shape::line(std::mem::take(run), stroke(1.6, *colour)));
                }
            }
            run.clear();
        };
        for (i, sample) in s.samples.iter().enumerate() {
            let x = plot.right() - (n - 1 - i) as f32 * step;
            match sample {
                Some(v) => run.push(pos2(x, plot.bottom() - plot.height() * (v / top).min(1.0))),
                None => {
                    flush(&mut run);
                    pen.p.line_segment([pos2(x, plot.bottom()), pos2(x, plot.bottom() - 4.0)], stroke(1.5, STATUS_RED));
                }
            }
        }
        flush(&mut run);
    }
}

/// The speed test: the last result, its state, the button.
fn speed(pen: &Pen, area: Rect, snap: &Snapshot, actions: &mut Actions) {
    let ink = pen.ink;
    let number = |mbps: f32| if mbps >= 100.0 { format!("{mbps:.0}") } else { format!("{mbps:.1}") };
    let (value, status, progress) = match &snap.speed {
        Speed::Idle => ("--".to_string(), "NEVER AUTOMATIC \u{b7} 25 MB WHEN YOU CLICK".to_string(), None),
        Speed::Running { bytes, started } => {
            let secs = started.elapsed().as_secs_f32();
            let value = if secs > 0.3 && *bytes > 0 { number(*bytes as f32 * 8.0 / secs / 1e6) } else { "--".to_string() };
            (value, format!("DOWNLOADING \u{b7} {:.1} / 25 MB", *bytes as f32 / 1e6), Some(*bytes as f32 / radar::SPEED_TEST_BYTES as f32))
        }
        Speed::Done { mbps, bytes, secs } => (number(*mbps), format!("{:.0} MB IN {secs:.1} S", *bytes as f32 / 1e6), None),
        Speed::Failed(e) => ("--".to_string(), format!("FAILED \u{b7} {}", e.to_uppercase()), None),
    };
    let big = pen.text(&value, pen.display(40.0, 20.0), ink.text, pen.px(1.0), area.width() * 0.6);
    let big_rect = pen.put(big, area.left_top(), Align2::LEFT_TOP);
    pen.put(pen.text("Mbps", pen.mono(12.0, 9.0), ink.accent, pen.px(1.0), 80.0), pos2(big_rect.right() + pen.px(8.0), big_rect.bottom() - pen.px(6.0)), Align2::LEFT_BOTTOM);
    let status_rect = pen.put(pen.text(&status, pen.mono(10.0, 8.5), ink.muted, pen.px(0.8), area.width()), pos2(area.left(), big_rect.bottom() + pen.px(4.0)), Align2::LEFT_TOP);
    let mut y = status_rect.bottom() + pen.px(8.0);
    if let Some(share) = progress {
        let bar = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), 3.0));
        pen.p.rect_filled(bar, Rounding::ZERO, ink.border.gamma_multiply(0.45));
        pen.p.rect_filled(Rect::from_min_size(bar.min, vec2(area.width() * share.clamp(0.0, 1.0), 3.0)), Rounding::ZERO, ink.accent);
        y += 3.0 + pen.px(8.0);
    }
    let running = matches!(snap.speed, Speed::Running { .. });
    let word = match snap.speed {
        Speed::Running { .. } => "RUNNING..",
        Speed::Done { .. } => "RUN AGAIN",
        _ => "RUN SPEED TEST",
    };
    let g = pen.text(word, pen.mono(10.5, 9.0), if running { ink.muted } else { ink.text }, pen.px(1.2), area.width());
    let button = Rect::from_min_size(pos2(area.left(), y), g.size() + vec2(pen.px(20.0), pen.px(10.0)));
    if button.bottom() > area.bottom() + 1.0 {
        return;
    }
    let response = pen.click(button, "speed_test", actions).on_hover_cursor(if running { CursorIcon::Default } else { CursorIcon::PointingHand });
    let lit = response.hovered() && !running;
    pen.p.rect_filled(button, Rounding::ZERO, if lit { ink.accent.gamma_multiply(0.18) } else { rgba([3, 7, 12], 0.9) });
    pen.p.rect_stroke(button, Rounding::ZERO, stroke(1.0, if lit { ink.accent } else { ink.border }));
    pen.put(g, button.center(), Align2::CENTER_CENTER);
    if response.clicked() && !running {
        actions.speed_test = true;
    }
}

// ---------------------------------------------------------------------------- details ----

/// "TODAY 14:05", "03 NOV 2026 09:12".
fn seen_text(seen: Option<u64>) -> String {
    let Some(at) = seen.and_then(|s| chrono::DateTime::from_timestamp(i64::try_from(s).ok()?, 0)) else {
        return "THIS SWEEP".to_string();
    };
    let local = at.with_timezone(&chrono::Local);
    if local.date_naive() == chrono::Local::now().date_naive() {
        local.format("TODAY %H:%M").to_string()
    } else {
        local.format("%d %b %Y %H:%M").to_string().to_uppercase()
    }
}

/// The details card's lines under the name: IP, MAC, MAKER, LATENCY, FIRST SEEN.
const DETAIL_ROWS: usize = 5;

/// The details card's fonts: the name, the line under it, the rows.
fn details_fonts(pen: &Pen) -> (FontId, FontId, FontId) {
    (pen.bold(15.0, 11.0), pen.plain(11.0, 9.0), pen.mono(10.5, 9.0))
}

/// Where the details card stands: over the scope's top-left corner (the scope keeps the
/// devices' names out of it).
fn details_rect(pen: &Pen, area: Rect) -> Rect {
    let (name_font, sub_font, font) = details_fonts(pen);
    let pad = pen.size(12.0, 7.0);
    let width = pen.px(300.0).max(230.0).min(area.width() - 8.0);
    let line_h = pen.row(&font) + pen.px(3.0);
    let height = 2.0 * pad + pen.row(&name_font) + pen.px(2.0) + pen.row(&sub_font) + pen.px(10.0) + DETAIL_ROWS as f32 * line_h;
    Rect::from_min_size(area.left_top() + vec2(4.0, 4.0), vec2(width, height))
}

/// The device Sir clicked (dropped when it is gone).
fn selected<'a>(snap: &'a Snapshot, view: &View) -> Option<&'a Device> {
    view.selected.and_then(|ip| snap.devices.iter().find(|d| d.ip == ip))
}

/// The card of the device Sir clicked, over the scope's corner; a click on it closes it.
fn details(pen: &Pen, area: Rect, snap: &Snapshot, view: &mut View, actions: &mut Actions) {
    let Some(d) = selected(snap, view) else {
        view.selected = None;
        return;
    };
    let ink = pen.ink;
    let colour = ink.kind(d.kind);
    let (name_font, sub_font, font) = details_fonts(pen);
    let maker = match (d.vendor.is_empty(), d.private_address()) {
        (false, _) => d.vendor.to_uppercase(),
        (true, true) => "HIDDEN (PRIVATE ADDRESS)".to_string(),
        (true, false) => "UNKNOWN".to_string(),
    };
    let rows: [(&str, String); DETAIL_ROWS] = [
        ("IP", d.ip.to_string()),
        ("MAC", if d.mac.is_some() { d.mac_text() } else { "UNKNOWN".to_string() }),
        ("MAKER", maker),
        ("LATENCY", latency_text(d)),
        ("FIRST SEEN", seen_text(d.first_seen)),
    ];
    let pad = pen.size(12.0, 7.0);
    let rect = details_rect(pen, area);
    let inner_w = rect.width() - 2.0 * pad;
    let label_w = rows.iter().map(|(l, _)| pen.measure(l, &font, pen.px(1.0))).fold(0.0, f32::max) + pen.px(12.0);
    let line_h = pen.row(&font) + pen.px(3.0);
    if pen.click(rect, "details", actions).clicked() {
        view.selected = None;
    }
    pen.p.rect_filled(rect, Rounding::ZERO, BLOCK_SLATE);
    pen.p.rect_stroke(rect, Rounding::ZERO, stroke(1.0, colour));
    corner_brackets(&pen.p, rect, pen.px(8.0), colour);
    let badge = d.new.then(|| pen.text("NEW", pen.mono(9.0, 8.0), STATUS_RED, pen.px(1.0), 60.0));
    let badge_room = badge.as_ref().map_or(0.0, |b| b.size().x + pen.px(16.0));
    let name = pen.text(&d.name(), name_font, ink.text, 0.0, (inner_w - badge_room).max(30.0));
    let name_rect = pen.put(name, rect.left_top() + vec2(pad, pad), Align2::LEFT_TOP);
    if let Some(b) = badge {
        let r = Align2::LEFT_CENTER.anchor_size(pos2(name_rect.right() + pen.px(10.0), name_rect.center().y), b.size());
        pen.p.rect_stroke(r.expand2(vec2(pen.px(4.0), 1.0)), Rounding::ZERO, stroke(1.0, STATUS_RED));
        pen.put(b, r.min, Align2::LEFT_TOP);
    }
    let sub_rect = pen.put(pen.text(&d.subline(), sub_font, ink.muted, 0.0, inner_w), pos2(rect.left() + pad, name_rect.bottom() + pen.px(2.0)), Align2::LEFT_TOP);
    let rule = sub_rect.bottom() + pen.px(5.0);
    pen.p.line_segment([pos2(rect.left() + pad, rule), pos2(rect.right() - pad, rule)], stroke(1.0, ink.border.gamma_multiply(0.6)));
    let mut y = rule + pen.px(5.0);
    for (caption, value) in rows {
        pen.put(pen.text(caption, font.clone(), ink.muted, pen.px(1.0), label_w), pos2(rect.left() + pad, y), Align2::LEFT_TOP);
        pen.put(pen.text(&value, font.clone(), ink.text, 0.0, (inner_w - label_w).max(20.0)), pos2(rect.left() + pad + label_w, y), Align2::LEFT_TOP);
        y += line_h;
    }
}

// ------------------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    /// The radar as the block holds it: the block's slate and margin around it.
    fn paint(ctx: &egui::Context, snap: &Snapshot, view: &mut View, actions: &mut Actions) {
        egui::CentralPanel::default().frame(Frame::none().fill(BLOCK_SLATE).inner_margin(Margin::same(12.0))).show(ctx, |ui| {
            let asked = draw(ui, snap, view);
            actions.rescan |= asked.rescan;
            actions.speed_test |= asked.speed_test;
            actions.close |= asked.close;
        });
    }

    fn clean(frame: &visual::Frame, what: &str) {
        assert_eq!(visual::clipped_text(frame), Vec::<String>::new(), "{what}");
        assert_eq!(visual::text_over_text(frame), Vec::<String>::new(), "{what}");
        assert_eq!(visual::pictographs(frame), Vec::<String>::new(), "{what}");
    }

    #[test]
    fn the_radar_shows_every_part_whole_at_both_sizes() {
        for size in [egui::vec2(1180.0, 720.0), egui::vec2(700.0, 460.0)] {
            let snap = radar::demo_snapshot();
            let (mut view, mut actions) = (View::default(), Actions::default());
            let mut stage = Stage::new(size);
            let frame = stage.frames(8, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
            clean(&frame, &format!("{size:?}"));
            for shown in ["NETWORK RADAR", "DEVICES", "THIS PC", "LIVE", "LATENCY", "SPEED TEST", "192.168.1.0/24", "6 ONLINE", "SWEPT IN 3.0 S", "ROUTER <1 ms", "TP-Link router", "Private device \u{b7}105"] {
                assert!(!frame.find_text(shown).is_empty(), "\"{shown}\" is not on the radar at {size:?}:\n{}", frame.all_text());
            }
            assert!(!actions.close && !actions.rescan && !actions.speed_test);
        }
    }

    #[test]
    fn every_phase_of_a_sweep_draws_cleanly() {
        let demo = radar::demo_snapshot();
        let mut sweeping = demo.clone();
        sweeping.phase = Phase::Sweeping;
        sweeping.devices.truncate(3);
        sweeping.probed = 120;
        sweeping.took = None;
        sweeping.generation = 7;
        let mut locating = Snapshot::default();
        locating.phase = Phase::Locating;
        locating.generation = 8;
        let mut failed = Snapshot::default();
        failed.phase = Phase::Failed;
        failed.generation = 9;
        failed.error = Some("No active network connection".into());
        let mut speed = demo.clone();
        speed.speed = Speed::Done { mbps: 942.0, bytes: radar::SPEED_TEST_BYTES, secs: 0.21 };
        for size in [egui::vec2(1180.0, 720.0), egui::vec2(700.0, 460.0)] {
            for (what, snap, tab, expect) in [
                ("sweeping", &sweeping, Tab::Latency, "SWEEPING 192.168.1.0/24"),
                ("locating", &locating, Tab::Latency, "LOCATING THE NETWORK"),
                ("failed", &failed, Tab::Latency, "NO ACTIVE NETWORK CONNECTION"),
                ("speed test", &speed, Tab::Speed, "942"),
            ] {
                let mut view = View { tab, ..View::default() };
                let mut actions = Actions::default();
                let mut stage = Stage::new(size);
                let frame = stage.frames(8, &mut |ctx| paint(ctx, snap, &mut view, &mut actions));
                clean(&frame, &format!("{what} at {size:?}"));
                assert!(!frame.find_text(expect).is_empty(), "\"{expect}\" missing ({what} at {size:?}):\n{}", frame.all_text());
            }
        }
    }

    #[test]
    fn a_click_opens_the_details_and_a_right_click_closes_the_radar() {
        let snap = radar::demo_snapshot();
        let (mut view, mut actions) = (View::default(), Actions::default());
        let mut stage = Stage::new(egui::vec2(1180.0, 720.0));
        let frame = stage.frames(6, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
        // The phone's row in the list (the rightmost of its names on screen).
        let row = frame.find_text("Private device \u{b7}105").into_iter().max_by(|a, b| a.center().x.total_cmp(&b.center().x)).expect("the phone's row");
        let frame = stage.click(row.center(), egui::PointerButton::Primary, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
        assert_eq!(view.selected, Some(Ipv4Addr::new(192, 168, 1, 105)));
        for shown in ["02:11:22:33:44:55", "FIRST SEEN", "HIDDEN (PRIVATE ADDRESS)", "Phone \u{b7} private address"] {
            assert!(!frame.find_text(shown).is_empty(), "\"{shown}\" is not on the details card:\n{}", frame.all_text());
        }
        clean(&frame, "details open");
        assert!(!actions.close);
        // A click on the card closes it again.
        let card = frame.find_text("FIRST SEEN")[0];
        stage.click(card.center(), egui::PointerButton::Primary, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
        assert_eq!(view.selected, None);
        // RESCAN asks for a sweep.
        let frame = stage.frames(2, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
        let rescan = frame.find_exact("RESCAN")[0];
        stage.click(rescan.center(), egui::PointerButton::Primary, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
        assert!(actions.rescan && !actions.close);
        // A right-click anywhere on the radar closes it.
        stage.click(row.center(), egui::PointerButton::Secondary, &mut |ctx| paint(ctx, &snap, &mut view, &mut actions));
        assert!(actions.close);
    }

    #[test]
    fn the_beam_reveals_devices_as_it_passes() {
        assert!(swept(10.0, 20.0, 15.0));
        assert!(!swept(10.0, 20.0, 25.0));
        assert!(swept(350.0, 5.0, 2.0) && swept(350.0, 5.0, 355.0));
        assert!(!swept(350.0, 5.0, 100.0));
        let ip = Ipv4Addr::new(192, 168, 1, 50);
        // A device 30 degrees ahead of where the beam starts.
        let ahead = ((BEAM_START + 30.0) % 360.0) as f32;
        let mut view = View { start: Some(0.0), ..View::default() };
        reveal(&mut view, &[(ip, ahead)], 0.0);
        reveal(&mut view, &[(ip, ahead)], 0.2);
        assert!(view.blips[&ip].found.is_none(), "the beam has turned 22 degrees");
        reveal(&mut view, &[(ip, ahead)], 0.3);
        assert_eq!(view.blips[&ip].found, Some(0.3));
        assert!(view.blips[&ip].pass.is_none());
        reveal(&mut view, &[(ip, ahead)], 3.6);
        assert_eq!(view.blips[&ip].pass, Some(3.6), "the next turn lights it again");
        // The router stands at north, where no beam starts: its first turn finds it.
        let router = Ipv4Addr::new(192, 168, 1, 1);
        let mut view = View { start: Some(0.0), ..View::default() };
        for frame in 0..=40 {
            reveal(&mut view, &[(router, 0.0)], f64::from(frame) / 60.0);
        }
        assert!(view.blips[&router].found.is_some(), "the router waited for a second turn");
        // A radar opened on a finished sweep shows everything at once.
        let mut view = View { start: Some(0.0), instant: true, ..View::default() };
        reveal(&mut view, &[(ip, 300.0)], 0.1);
        assert!(view.blips[&ip].found.is_some());
    }

    #[test]
    fn rows_appear_one_after_another() {
        let mut snap = radar::demo_snapshot();
        snap.phase = Phase::Sweeping;
        let mut view = View::default();
        view.sync(&snap, 1.0);
        assert_eq!(view.shown, 0);
        view.sync(&snap, 1.06);
        view.sync(&snap, 1.07);
        assert_eq!(view.shown, 1, "one row per stagger");
        view.sync(&snap, 1.2);
        assert_eq!(view.shown, 2);
        snap.phase = Phase::Done;
        view.sync(&snap, 1.3);
        assert_eq!(view.flash_at, Some(1.3), "the ring flashes when the sweep completes");
    }

    #[test]
    fn crowded_blips_are_pushed_apart_the_same_way_every_frame() {
        let c = pos2(200.0, 200.0);
        let start = vec![pos2(250.0, 200.0), pos2(252.0, 201.0), pos2(251.0, 199.0), pos2(200.0, 230.0), pos2(200.0, 230.0)];
        let keep_out = [Rect::from_center_size(pos2(200.0, 225.0), vec2(40.0, 12.0))];
        let (mut a, mut b) = (start.clone(), start);
        relax(&mut a, c, 30.0, 180.0, 24.0, &keep_out);
        relax(&mut b, c, 30.0, 180.0, 24.0, &keep_out);
        assert_eq!(a, b);
        for i in 0..a.len() {
            let from_centre = (a[i] - c).length();
            assert!((29.9..=180.1).contains(&from_centre), "{i}: {from_centre}");
            for j in i + 1..a.len() {
                assert!((a[i] - a[j]).length() >= 20.0, "{i} and {j} still overlap: {a:?}");
            }
        }
    }
}
