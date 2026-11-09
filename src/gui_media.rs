//! The picture viewer (`image_viewer`), the live cameras (`webcams`) and street view
//! (`street_view`) as HUD blocks, each detachable into a window of its own, and the Earth's
//! camera layer (`paint_earth_cameras`, called from `gui_earth`).
//!
//! Everything is painted from the modules' state and `image_viewer::pixels`: decoding happens
//! off this thread; a frame only turns a decoded picture into a texture (kept while it is used).
//!
//! * Pictures: fit, wheel zoom about the pointer, drag to pan, double-click to fit again; ← →
//!   (or the buttons) for the previous and next picture; the strip below; SAVE.
//! * Cameras: one camera large with the next nearest beside it, or a 2×2 / 3×3 wall; every frame
//!   shows its place, source and age; frames are fetched again every few seconds.
//! * Street view: a 360° panorama is painted inside a sphere (a textured mesh: drag to look, the
//!   wheel zooms); a flat photo is framed; ← → (or the buttons) step along the sequence.

use eframe::egui::{
    self, epaint::Vertex, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Frame, Key, Layout, Margin, Mesh, Painter, Pos2, Rect, RichText, Rounding, Sense, Shape, Stroke, TextureHandle,
    TextureId, TextureOptions, Ui, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
};
use std::collections::{HashMap, HashSet};
use std::f32::consts::{PI, TAU};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::runtime::Handle;

const PANEL: Color32 = Color32::from_rgb(0x02, 0x05, 0x09);
const LIVE_RED: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x4E);

/// One block: a picture block (one per subject), the cameras or street view. Its window is the
/// viewport `jarvis_<id>`.
#[derive(Clone)]
struct Block {
    id: String,
    title: &'static str,
    window_title: &'static str,
    size: [f32; 2],
    min: [f32; 2],
    open: Arc<dyn Fn() -> bool + Send + Sync>,
    close: Arc<dyn Fn() + Send + Sync>,
    body: Arc<dyn Fn(&mut Ui, &Handle) + Send + Sync>,
}

fn pictures(id: &str) -> Block {
    let (open, close, body) = (id.to_string(), id.to_string(), id.to_string());
    Block {
        id: id.to_string(),
        title: "Pictures",
        window_title: "Pictures — J.A.R.V.I.S.",
        size: [980.0, 680.0],
        min: [520.0, 380.0],
        open: Arc::new(move || crate::image_viewer::block_ids().contains(&open)),
        close: Arc::new(move || crate::image_viewer::close_block(&close)),
        body: Arc::new(move |ui, rt| pictures_body(ui, rt, &body)),
    }
}

fn cameras() -> Block {
    Block {
        id: "webcams".into(),
        title: "Live cameras",
        window_title: "Live cameras — J.A.R.V.I.S.",
        size: [1040.0, 660.0],
        min: [560.0, 380.0],
        open: Arc::new(crate::webcams::is_open),
        close: Arc::new(crate::webcams::close),
        body: Arc::new(cameras_body),
    }
}

fn street() -> Block {
    Block {
        id: "street_view".into(),
        title: "Street view",
        window_title: "Street view — J.A.R.V.I.S.",
        size: [1040.0, 640.0],
        min: [560.0, 380.0],
        open: Arc::new(crate::street_view::is_open),
        close: Arc::new(crate::street_view::close),
        body: Arc::new(street_body),
    }
}

/// Picture blocks drawn lately: a closed one is hosted once more, so its window plays its exit.
static PICTURE_IDS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(Default::default);

/// Registers the blocks while they are open (on the board, or as windows of their own).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    crate::image_viewer::pixels::set_runtime(rt);
    crate::webcams::set_runtime(rt);
    crate::street_view::set_runtime(rt);
    let ids = {
        let mut known = super::lock(&PICTURE_IDS);
        for id in crate::image_viewer::block_ids() {
            if !known.contains(&id) {
                known.push(id);
            }
        }
        known.clone()
    };
    for id in ids {
        if !host(ctx, rt, pictures(&id)) {
            super::lock(&PICTURE_IDS).retain(|k| *k != id);
        }
    }
    host(ctx, rt, cameras());
    host(ctx, rt, street());
    forget_unused_textures();
}

/// Draws block `b`; false once it is closed and gone.
fn host(ctx: &egui::Context, rt: &Handle, b: Block) -> bool {
    let viewport = ViewportId::from_hash_of(format!("jarvis_{}", b.id));
    // (Closed while it was a window of its own: it plays its exit first.)
    if !super::detached_keep(ctx, viewport, (b.open)()) {
        crate::hud_dock::forget(&b.id);
        return false;
    }
    if !crate::hud_dock::adopt(&b.id) {
        let action = super::hud_block_window(ctx, &b.id, b.title, b.size, |ui| (b.body)(ui, rt));
        if action.close {
            (b.close)();
        }
        if action.detach {
            crate::hud_dock::detach(&b.id);
        }
        return true;
    }
    let mut builder = ViewportBuilder::default().with_title(b.window_title).with_app_id("jarvis").with_inner_size(b.size).with_min_inner_size(b.min).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, &b.id) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let rt = rt.clone();
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard(b.title, || {
            if ctx.input(|i| i.viewport().close_requested()) || (!(b.open)() && !super::detached_leaving(ctx)) {
                (b.close)();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(b.title).id(egui::Id::new(("jarvis_embedded_block", &b.id))).show(ctx, |ui| (b.body)(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, b.title) {
                        (b.close)();
                    }
                    (b.body)(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
    true
}

// ------------------------------------------------------------------------------ textures ----

struct Tex {
    version: u64,
    handle: TextureHandle,
    used: Instant,
}

static TEXTURES: LazyLock<Mutex<HashMap<String, Tex>>> = LazyLock::new(Default::default);

/// The texture of picture `key` (made or updated from its latest frame), and its size in pixels.
fn texture(ctx: &egui::Context, key: &str) -> Option<(TextureHandle, Vec2)> {
    let frame = crate::image_viewer::pixels::get(key)?;
    let size = vec2(frame.image.size[0] as f32, frame.image.size[1] as f32);
    let mut textures = super::lock(&TEXTURES);
    let now = Instant::now();
    match textures.get_mut(key) {
        Some(t) => {
            if t.version != frame.version {
                t.handle.set((*frame.image).clone(), TextureOptions::LINEAR);
                t.version = frame.version;
            }
            t.used = now;
            Some((t.handle.clone(), size))
        }
        None => {
            let handle = ctx.load_texture(key, (*frame.image).clone(), TextureOptions::LINEAR);
            textures.insert(key.to_string(), Tex { version: frame.version, handle: handle.clone(), used: now });
            Some((handle, size))
        }
    }
}

/// Textures nobody drew for 20 s are let go (the decoded pictures stay in `pixels` a while).
fn forget_unused_textures() {
    super::lock(&TEXTURES).retain(|_, t| t.used.elapsed() < Duration::from_secs(20));
}

fn hash_of(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

// --------------------------------------------------------------------------- shared bits ----

fn accent() -> Color32 {
    crate::hud_board::active_colour()
}

fn chip(ui: &mut Ui, text: &str, hint: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(text).font(super::strip_font()).color(super::STRIP_INK)).fill(super::rgba([3, 7, 12], 0.9)).stroke(super::stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO))
        .on_hover_text(hint)
        .clicked()
}

fn muted(ui: &mut Ui, text: impl Into<String>) {
    ui.add(egui::Label::new(RichText::new(text.into()).font(super::strip_font()).color(super::STRIP_MUTED)).truncate());
}

/// A line of text in the middle of a dark frame ("Searching…").
fn frame_note(p: &Painter, rect: Rect, text: &str) {
    p.rect_filled(rect, Rounding::ZERO, PANEL);
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::monospace(12.0), super::STRIP_MUTED);
}

/// A small label with a dark backing.
fn tag(p: &Painter, at: Pos2, anchor: Align2, text: &str, colour: Color32) -> Rect {
    let galley = p.layout_no_wrap(text.to_string(), FontId::monospace(10.5), colour);
    let rect = anchor.anchor_size(at, galley.size()).expand2(vec2(5.0, 2.0));
    p.rect_filled(rect, Rounding::ZERO, Color32::from_rgba_unmultiplied(2, 5, 9, 210));
    p.galley(rect.min + vec2(5.0, 2.0), galley, colour);
    rect
}

/// How old a moment is, in words ("12 s", "3 min", "2 h").
fn age(since: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(since).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=89 => format!("{secs} s"),
        90..=5399 => format!("{} min", (secs + 30) / 60),
        _ => format!("{} h", (secs + 1800) / 3600),
    }
}

/// Zoom and pan of a framed picture (egui memory, by the frame's id).
#[derive(Clone, Copy, Debug)]
struct ZoomPan {
    key: u64,
    zoom: f32,
    pan: Vec2,
}

impl Default for ZoomPan {
    fn default() -> Self {
        Self { key: 0, zoom: 1.0, pan: Vec2::ZERO }
    }
}

/// A picture fitted into `rect`: the wheel zooms about the pointer, a drag pans, a double-click
/// fits it again. `key` changes with the picture (its zoom starts afresh).
fn framed(ui: &mut Ui, rect: Rect, id: egui::Id, key: u64, tex: &TextureHandle, px: Vec2) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let mut zp: ZoomPan = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    if zp.key != key {
        zp = ZoomPan { key, ..ZoomPan::default() };
    }
    if resp.hovered() {
        let (scroll, pinch, pointer) = ui.input(|i| (i.raw_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
        let factor = pinch * (scroll * 0.0018).exp();
        if (factor - 1.0).abs() > 1e-4 {
            let new = (zp.zoom * factor).clamp(1.0, 12.0);
            if let Some(p) = pointer {
                let rel = p - (rect.center() + zp.pan);
                zp.pan += rel - rel * (new / zp.zoom);
            }
            zp.zoom = new;
        }
    }
    if resp.dragged() {
        zp.pan += resp.drag_delta();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if resp.hovered() && zp.zoom > 1.01 {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
    if resp.double_clicked() {
        zp = ZoomPan { key, ..ZoomPan::default() };
    }
    let fit = (rect.width() / px.x.max(1.0)).min(rect.height() / px.y.max(1.0));
    let size = px * fit * zp.zoom;
    let room = ((size - rect.size()) * 0.5).max(Vec2::ZERO);
    zp.pan = zp.pan.clamp(-room, room);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, Rounding::ZERO, PANEL);
    painter.image(tex.id(), Rect::from_center_size(rect.center() + zp.pan, size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    ui.data_mut(|d| d.insert_temp(id, zp));
    resp
}

/// A picture fitted into `rect` without interaction (thumbnails, camera frames).
fn fitted(p: &Painter, rect: Rect, tex: &TextureHandle, px: Vec2, cover: bool) {
    let fit = if cover { (rect.width() / px.x.max(1.0)).max(rect.height() / px.y.max(1.0)) } else { (rect.width() / px.x.max(1.0)).min(rect.height() / px.y.max(1.0)) };
    let size = px * fit;
    p.with_clip_rect(rect).image(tex.id(), Rect::from_center_size(rect.center(), size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

/// ← and → while the pointer is over `rect` (and no text field has the keyboard).
fn arrows(ui: &Ui, rect: Rect) -> Option<bool> {
    if !ui.rect_contains_pointer(rect) || ui.ctx().wants_keyboard_input() {
        return None;
    }
    ui.input(|i| if i.key_pressed(Key::ArrowRight) { Some(true) } else if i.key_pressed(Key::ArrowLeft) { Some(false) } else { None })
}

// ------------------------------------------------------------------------------ pictures ----

fn pictures_body(ui: &mut Ui, rt: &Handle, id: &str) {
    let album = crate::image_viewer::album_of(Some(id));
    let current = album.current().cloned();
    let n = album.items.len();
    ui.horizontal(|ui| {
        let title = current.as_ref().map(|p| p.title.clone()).unwrap_or_default();
        ui.add(egui::Label::new(RichText::new(format!("{} // {}", album.label.to_uppercase(), title)).font(super::strip_font()).color(accent())).truncate());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if current.as_ref().is_some_and(|p| p.local.is_none()) && chip(ui, "[ SAVE ]", "Save the full picture to Pictures\\Jarvis") {
                let id = id.to_string();
                rt.spawn(async move {
                    if let Err(e) = crate::image_viewer::save_current(Some(&id)).await {
                        crate::image_viewer::set_note(Some(&id), &e);
                    }
                });
            }
            if n > 1 {
                if chip(ui, "[ NEXT ]", "Next picture (→)") {
                    crate::image_viewer::step(Some(id), 1);
                }
                ui.label(RichText::new(format!("{} / {n}", album.index + 1)).font(super::strip_font()).color(super::STRIP_INK));
                if chip(ui, "[ PREV ]", "Previous picture (←)") {
                    crate::image_viewer::step(Some(id), -1);
                }
            }
        });
    });
    let strip_h = if n > 1 { 70.0 } else { 0.0 };
    let avail = ui.available_size();
    let main_h = (avail.y - strip_h - 26.0).max(120.0);
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, main_h), Sense::hover());
    match &current {
        None => frame_note(&ui.painter_at(rect), rect, if album.note.is_empty() { "No pictures yet." } else { &album.note }),
        Some(pic) => {
            crate::image_viewer::pixels::want(&pic.full_key(), &pic.full_origin(), crate::image_viewer::FULL_SIDE, pic.local.is_none());
            let full = texture(ui.ctx(), &pic.full_key());
            let shown = full.clone().or_else(|| texture(ui.ctx(), &pic.thumb_key()));
            match shown {
                Some((tex, px)) => {
                    framed(ui, rect, egui::Id::new(("jarvis_picture_frame", id)), hash_of(&pic.full_key()), &tex, px);
                    if full.is_none() {
                        tag(&ui.painter_at(rect), rect.right_top() + vec2(-8.0, 8.0), Align2::RIGHT_TOP, "LOADING FULL RESOLUTION", super::STRIP_MUTED);
                    }
                }
                None => {
                    let status = crate::image_viewer::pixels::status(&pic.full_key());
                    let text = match status {
                        Some((false, Some(e))) => format!("This picture could not be loaded ({e})."),
                        _ => "Loading…".to_string(),
                    };
                    frame_note(&ui.painter_at(rect), rect, &text);
                }
            }
            if !album.note.is_empty() {
                tag(&ui.painter_at(rect), rect.left_bottom() + vec2(8.0, -8.0), Align2::LEFT_BOTTOM, &album.note, super::STRIP_INK);
            }
            if let Some(forward) = arrows(ui, rect) {
                crate::image_viewer::step(Some(id), if forward { 1 } else { -1 });
            }
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if let Some(pic) = &current {
            muted(ui, pic.credit());
            if !pic.page.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.hyperlink_to(RichText::new("SOURCE PAGE").font(super::strip_font()).color(accent()), &pic.page);
                });
            }
        }
    });
    if n > 1 {
        strip(ui, &album, id);
    }
}

/// The album's thumbnails; a click shows that picture.
fn strip(ui: &mut Ui, album: &crate::image_viewer::Album, id: &str) {
    egui::ScrollArea::horizontal().id_salt(("jarvis_picture_strip", id)).max_height(66.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, pic) in album.items.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(96.0, 58.0), Sense::click());
                if ui.is_rect_visible(rect) {
                    crate::image_viewer::pixels::want(&pic.thumb_key(), &pic.thumb_origin(), crate::image_viewer::THUMB_SIDE, pic.local.is_none());
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, Rounding::ZERO, PANEL);
                    if let Some((tex, px)) = texture(ui.ctx(), &pic.thumb_key()) {
                        fitted(&p, rect, &tex, px, true);
                    }
                    let edge = if i == album.index { accent() } else { crate::hud_board::border_colour().gamma_multiply(0.6) };
                    p.rect_stroke(rect, Rounding::ZERO, Stroke::new(if i == album.index { 2.0_f32 } else { 1.0 }, edge));
                }
                if resp.on_hover_text(&pic.title).clicked() {
                    crate::image_viewer::select(id, i);
                }
            }
        });
    });
}

// ------------------------------------------------------------------------------- cameras ----

fn cameras_body(ui: &mut Ui, _rt: &Handle) {
    let view = crate::webcams::view();
    // The frames' ages tick on; new frames are asked for on time.
    ui.ctx().request_repaint_after(Duration::from_millis(1000));
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(format!("LIVE // {}", view.label.to_uppercase())).font(super::strip_font()).color(accent())).truncate());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if view.cams.len() > 1 {
                if chip(ui, "[ NEXT ]", "The next camera") {
                    crate::webcams::select(if view.wall > 0 { 0 } else { (view.index + 1) % view.cams.len() });
                }
                if view.cams.len() >= 9 && chip(ui, "[ 3×3 ]", "A wall of nine cameras") {
                    crate::webcams::show_wall(9);
                }
                if chip(ui, "[ 2×2 ]", "A wall of four cameras") {
                    crate::webcams::show_wall(4);
                }
            }
        });
    });
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, (avail.y - 4.0).max(120.0)), Sense::hover());
    if view.cams.is_empty() {
        frame_note(&ui.painter_at(rect), rect, if view.note.is_empty() { "No cameras." } else { &view.note });
        return;
    }
    if view.wall > 0 {
        let cols = if view.wall >= 9 && view.cams.len() >= 5 { 3 } else { 2 };
        let shown = view.cams.len().min(cols * cols);
        let gap = 6.0;
        let cell = vec2((rect.width() - gap * (cols as f32 - 1.0)) / cols as f32, (rect.height() - gap * (cols as f32 - 1.0)) / cols as f32);
        for (i, (cam, km)) in view.cams.iter().take(shown).enumerate() {
            let at = rect.min + vec2((i % cols) as f32 * (cell.x + gap), (i / cols) as f32 * (cell.y + gap));
            let r = Rect::from_min_size(at, cell);
            let resp = ui.interact(r, egui::Id::new(("jarvis_cam_tile", i)), Sense::click());
            camera_frame(ui, r, cam, *km, true, resp.hovered());
            if resp.on_hover_text("Show this camera large").clicked() {
                crate::webcams::select(i);
            }
        }
        return;
    }
    // One camera large, the others beside it.
    let side = if view.cams.len() > 1 && rect.width() > 620.0 { 230.0 } else { 0.0 };
    let main = Rect::from_min_max(rect.min, pos2(rect.right() - side - if side > 0.0 { 8.0 } else { 0.0 }, rect.bottom()));
    if let Some((cam, km)) = view.current() {
        camera_frame(ui, main, cam, *km, false, false);
        if let Some(forward) = arrows(ui, main) {
            let n = view.cams.len();
            crate::webcams::select(if forward { (view.index + 1) % n } else { (view.index + n - 1) % n });
        }
    }
    if side > 0.0 {
        let list = Rect::from_min_max(pos2(rect.right() - side, rect.top()), rect.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).layout(Layout::top_down(Align::Min)));
        egui::ScrollArea::vertical().id_salt("jarvis_cam_list").show(&mut child, |ui| {
            for (i, (cam, km)) in view.cams.iter().enumerate() {
                let text = RichText::new(format!("{}\n{} · {:.1} km", cam.title(), cam.area, km)).font(FontId::monospace(10.5)).color(if i == view.index { accent() } else { super::STRIP_INK });
                if ui.add(egui::SelectableLabel::new(i == view.index, text)).clicked() {
                    crate::webcams::select(i);
                }
            }
        });
    }
}

/// A camera's latest frame, fetched again when it is due, with its name, place, source and age.
fn camera_frame(ui: &Ui, rect: Rect, cam: &crate::webcams::Cam, km: f64, on_wall: bool, hovered: bool) {
    use crate::image_viewer::pixels;
    let key = cam.key();
    match pixels::get(&key) {
        Some(frame) => {
            let since = SystemTime::now().duration_since(frame.fetched).unwrap_or_default();
            if since.as_secs() >= cam.refresh_secs(on_wall) {
                pixels::reload(&key, &cam.origin(), crate::webcams::FRAME_SIDE);
            }
        }
        None => pixels::want(&key, &cam.origin(), crate::webcams::FRAME_SIDE, false),
    }
    crate::webcams::ensure_name(cam);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, Rounding::ZERO, PANEL);
    let frame = pixels::get(&key);
    match texture(ui.ctx(), &key) {
        Some((tex, px)) => fitted(&p, rect, &tex, px, false),
        None => {
            let text = match pixels::status(&key) {
                Some((false, Some(e))) => format!("No frame ({e})"),
                _ => "Connecting…".to_string(),
            };
            p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::monospace(11.0), super::STRIP_MUTED);
        }
    }
    let edge = if hovered { accent() } else { crate::hud_board::border_colour().gamma_multiply(0.7) };
    p.rect_stroke(rect, Rounding::ZERO, Stroke::new(1.0_f32, edge));
    // A red dot, LIVE and the name.
    let top = tag(&p, rect.left_top() + vec2(6.0, 6.0), Align2::LEFT_TOP, &format!("   LIVE  {}", cam.title()), super::STRIP_INK);
    p.circle_filled(pos2(top.left() + 8.0, top.center().y), 3.5, LIVE_RED);
    let age_text = match &frame {
        Some(f) => match f.modified {
            Some(m) => format!("frame {} old", age(m)),
            None => format!("checked {} ago", age(f.fetched)),
        },
        None => "waiting for a frame".to_string(),
    };
    let far = if km >= 0.05 { format!(" · {km:.1} km away") } else { String::new() };
    let line = if on_wall { format!("{} · {age_text}", cam.area) } else { format!("{} · {} · {age_text}{far}", cam.area, cam.source) };
    tag(&p, rect.left_bottom() + vec2(6.0, -6.0), Align2::LEFT_BOTTOM, &line, super::STRIP_MUTED);
}

// --------------------------------------------------------------------------- street view ----

/// Where the panorama looks (egui memory, by the frame's id).
#[derive(Clone, Copy, Debug)]
struct Look {
    key: u64,
    /// Radians from the picture's middle, positive to the right.
    yaw: f32,
    /// Radians, positive up.
    pitch: f32,
    /// The horizontal field of view, degrees.
    fov: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self { key: 0, yaw: 0.0, pitch: 0.0, fov: 90.0 }
    }
}

fn street_body(ui: &mut Ui, _rt: &Handle) {
    let scene = crate::street_view::scene();
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(format!("STREET VIEW // {}", scene.label.to_uppercase())).font(super::strip_font()).color(accent())).truncate());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(shot) = &scene.shot {
                if shot.next.is_some() && chip(ui, "[ FORWARD ]", "One photo further along (→)") {
                    crate::street_view::step_in_background(true);
                }
                if shot.prev.is_some() && chip(ui, "[ BACK ]", "One photo back (←)") {
                    crate::street_view::step_in_background(false);
                }
            }
        });
    });
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, (avail.y - 26.0).max(120.0)), Sense::hover());
    let Some(shot) = &scene.shot else {
        frame_note(&ui.painter_at(rect), rect, if scene.note.is_empty() { "Looking…" } else { &scene.note });
        return;
    };
    shot.want_pixels();
    let full = texture(ui.ctx(), &shot.full_key());
    let shown = full.clone().or_else(|| texture(ui.ctx(), &shot.thumb_key()));
    let key = hash_of(&shot.full_key());
    let mut heading = shot.heading;
    match shown {
        Some((tex, px)) if shot.pano => {
            let look = panorama(ui, rect, egui::Id::new("jarvis_street_pano"), key, scene.start_yaw, tex.id());
            heading = shot.heading.map(|h| (h + look.yaw.to_degrees() as f64).rem_euclid(360.0));
            let _ = px;
        }
        Some((tex, px)) => {
            framed(ui, rect, egui::Id::new("jarvis_street_photo"), key, &tex, px);
        }
        None => {
            let text = match crate::image_viewer::pixels::status(&shot.full_key()) {
                Some((false, Some(e))) => format!("This photo could not be loaded ({e})."),
                _ => "Loading the photo…".to_string(),
            };
            frame_note(&ui.painter_at(rect), rect, &text);
        }
    }
    let p = ui.painter_at(rect);
    if full.is_none() {
        tag(&p, rect.right_top() + vec2(-8.0, 8.0), Align2::RIGHT_TOP, "LOADING FULL RESOLUTION", super::STRIP_MUTED);
    }
    if let Some(h) = heading {
        const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
        let point = POINTS[(((h + 22.5) / 45.0) as usize) % 8];
        tag(&p, rect.center_top() + vec2(0.0, 8.0), Align2::CENTER_TOP, &format!("HEADING {h:03.0}° {point}"), super::STRIP_INK);
    }
    if !scene.note.is_empty() {
        tag(&p, rect.left_bottom() + vec2(8.0, -8.0), Align2::LEFT_BOTTOM, &scene.note, super::STRIP_INK);
    }
    if let Some(forward) = arrows(ui, rect) {
        let can = if forward { shot.next.is_some() } else { shot.prev.is_some() };
        if can {
            crate::street_view::step_in_background(forward);
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let far = scene.target.map(|t| crate::earth::haversine_km(t, (shot.lat, shot.lon)) * 1000.0).map(|m| format!(" · {m:.0} m from {}", scene.label)).unwrap_or_default();
        muted(ui, format!("{}{far}{}", shot.credit(), if shot.pano { " · drag to look around, wheel to zoom" } else { "" }));
        if !shot.page.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.hyperlink_to(RichText::new("SOURCE PAGE").font(super::strip_font()).color(accent()), &shot.page);
            });
        }
    });
}

/// An equirectangular panorama seen from inside its sphere: drag to look, the wheel (or a
/// pinch) zooms between 30° and 110°. Where it looks now.
fn panorama(ui: &mut Ui, rect: Rect, id: egui::Id, key: u64, start_yaw_deg: f32, tex: TextureId) -> Look {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let mut look: Look = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    if look.key != key {
        look = Look { key, yaw: start_yaw_deg.to_radians(), ..Look::default() };
    }
    let focal = focal_px(rect.width(), look.fov);
    if resp.dragged() {
        let d = resp.drag_delta();
        look.yaw = (look.yaw - d.x / focal + PI).rem_euclid(TAU) - PI;
        look.pitch = (look.pitch + d.y / focal).clamp(-1.45, 1.45);
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
        let (scroll, pinch) = ui.input(|i| (i.raw_scroll_delta.y, i.zoom_delta()));
        let factor = (-scroll * 0.0015).exp() / pinch.max(0.1);
        look.fov = (look.fov * factor).clamp(30.0, 110.0);
    }
    if resp.double_clicked() {
        look.fov = 90.0;
        look.pitch = 0.0;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, Rounding::ZERO, PANEL);
    painter.add(Shape::mesh(sphere_mesh(rect, tex, look.yaw, look.pitch, look.fov)));
    ui.data_mut(|d| d.insert_temp(id, look));
    look
}

/// Pixels from the eye to the screen for a horizontal field of view.
fn focal_px(width: f32, fov_deg: f32) -> f32 {
    (width * 0.5) / (fov_deg.to_radians() * 0.5).tan().max(0.05)
}

/// The inside of the panorama's sphere as a textured mesh, projected for a camera at its centre
/// looking `yaw`/`pitch` with field of view `fov`. Cells with a corner behind the eye are left
/// out (they are outside any view up to 110°).
pub(super) fn sphere_mesh(rect: Rect, tex: TextureId, yaw: f32, pitch: f32, fov: f32) -> Mesh {
    const COLS: usize = 64;
    const ROWS: usize = 32;
    let focal = focal_px(rect.width(), fov);
    let c = rect.center();
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let mut mesh = Mesh::with_texture(tex);
    let mut front: Vec<bool> = Vec::with_capacity((COLS + 1) * (ROWS + 1));
    for j in 0..=ROWS {
        let v = j as f32 / ROWS as f32;
        let phi = (0.5 - v) * PI;
        let (s_phi, c_phi) = phi.sin_cos();
        for i in 0..=COLS {
            let u = i as f32 / COLS as f32;
            let theta = (u - 0.5) * TAU;
            let (x, y, z) = (c_phi * theta.sin(), s_phi, c_phi * theta.cos());
            let x1 = x * cy - z * sy;
            let z1 = x * sy + z * cy;
            let y2 = y * cp - z1 * sp;
            let z2 = y * sp + z1 * cp;
            let ok = z2 > 0.03;
            let pos = if ok { pos2(c.x + focal * x1 / z2, c.y - focal * y2 / z2) } else { c };
            mesh.vertices.push(Vertex { pos, uv: pos2(u, v), color: Color32::WHITE });
            front.push(ok);
        }
    }
    for j in 0..ROWS {
        for i in 0..COLS {
            let a = j * (COLS + 1) + i;
            let (b, d) = (a + 1, a + COLS + 1);
            let e = d + 1;
            if front[a] && front[b] && front[d] && front[e] {
                mesh.add_triangle(a as u32, b as u32, d as u32);
                mesh.add_triangle(b as u32, e as u32, d as u32);
            }
        }
    }
    mesh
}

// ------------------------------------------------------------------- the Earth's layer ----

/// The Earth's camera layer (`earth::Layer::Cameras`): a small camera mark per public camera
/// (one per 10 px, so a continent stays legible); the one under the pointer is named, and a click
/// opens its feed. `at` is the Earth's projection (screen point and visibility).
pub(super) fn paint_earth_cameras(p: &Painter, map: Rect, at: &dyn Fn(f64, f64) -> Option<(Pos2, f32)>) {
    if !crate::webcams::earth_layer() {
        return;
    }
    let cams = crate::webcams::all_cams();
    if cams.is_empty() {
        return;
    }
    let ctx = p.ctx().clone();
    let (pointer, clicked) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_clicked()));
    let pointer = pointer.filter(|pt| map.contains(*pt));
    let ink = accent();
    let mut cells: HashSet<(i32, i32)> = HashSet::new();
    let mut best: Option<(f32, usize, Pos2)> = None;
    for (i, cam) in cams.iter().enumerate() {
        let Some((pos, vis)) = at(cam.lat, cam.lon) else { continue };
        if !map.contains(pos) {
            continue;
        }
        if let Some(pt) = pointer {
            let d = pt.distance(pos);
            if d < 9.0 && best.is_none_or(|(b, _, _)| d < b) {
                best = Some((d, i, pos));
            }
        }
        if cells.insert(((pos.x / 10.0).floor() as i32, (pos.y / 10.0).floor() as i32)) {
            camera_mark(p, pos, ink.gamma_multiply(0.45 + 0.55 * vis.clamp(0.0, 1.0)));
        }
    }
    if let Some((_, i, pos)) = best {
        let cam = &cams[i];
        p.circle_stroke(pos, 7.5, Stroke::new(1.5_f32, ink));
        tag(p, pos + vec2(11.0, -9.0), Align2::LEFT_BOTTOM, &format!("{} · {} · click for the live feed", cam.title(), cam.area), super::STRIP_INK);
        ctx.set_cursor_icon(CursorIcon::PointingHand);
        if clicked {
            crate::webcams::open_cam(cam);
        }
    }
}

/// A camera in 7 × 5 px: its body and lens.
fn camera_mark(p: &Painter, at: Pos2, colour: Color32) {
    p.rect_filled(Rect::from_center_size(at + vec2(-0.5, 0.0), vec2(5.0, 4.0)), Rounding::same(0.5), colour);
    p.add(Shape::convex_polygon(vec![at + vec2(2.0, 0.0), at + vec2(4.0, -2.0), at + vec2(4.0, 2.0)], colour, Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sphere_looks_where_it_is_turned() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0));
        let mesh = sphere_mesh(rect, TextureId::default(), 0.0, 0.0, 90.0);
        assert!(!mesh.indices.is_empty());
        // The picture's middle (u = 0.5, v = 0.5) lands in the middle of the view.
        let middle = mesh.vertices.iter().find(|v| (v.uv.x - 0.5).abs() < 1e-4 && (v.uv.y - 0.5).abs() < 1e-4).unwrap();
        assert!((middle.pos - rect.center()).length() < 0.5, "{:?}", middle.pos);
        // Turned a quarter to the right: the picture's three-quarter mark is in the middle.
        let mesh = sphere_mesh(rect, TextureId::default(), PI / 2.0, 0.0, 90.0);
        let right = mesh.vertices.iter().find(|v| (v.uv.x - 0.75).abs() < 1e-4 && (v.uv.y - 0.5).abs() < 1e-4).unwrap();
        assert!((right.pos - rect.center()).length() < 0.5, "{:?}", right.pos);
        // Looking up, the horizon drops below the middle.
        let mesh = sphere_mesh(rect, TextureId::default(), 0.0, 0.3, 90.0);
        let horizon = mesh.vertices.iter().find(|v| (v.uv.x - 0.5).abs() < 1e-4 && (v.uv.y - 0.5).abs() < 1e-4).unwrap();
        assert!(horizon.pos.y > rect.center().y + 50.0, "{:?}", horizon.pos);
        // Nothing behind the eye is drawn: every triangle's corners are in front.
        let behind = mesh.indices.iter().filter(|i| mesh.vertices[**i as usize].pos == rect.center() && mesh.vertices[**i as usize].uv != pos2(0.5, 0.5)).count();
        assert_eq!(behind, 0);
    }

    #[test]
    fn ages_read_naturally() {
        let now = SystemTime::now();
        assert_eq!(age(now - Duration::from_secs(12)), "12 s");
        assert_eq!(age(now - Duration::from_secs(600)), "10 min");
        assert_eq!(age(now - Duration::from_secs(7200)), "2 h");
    }
}
