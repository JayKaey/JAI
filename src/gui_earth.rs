//! The EARTH block (`crate::earth`): on the board, or in its own window once detached.
//!
//! The globe is drawn on the GPU (`earth_gl`: one paint callback, a ray-cast sphere lit by the
//! sun, its base imagery and the clipmaps of tiles around the view) against a starfield in the
//! theme's colour. Until that callback has drawn (and for good where the driver cannot) the globe
//! is a textured egui mesh: a polar grid from the middle of the view out to the limb whose
//! vertices carry their latitude and longitude as equirectangular UVs, with a fresnel rim. Below
//! ~1 600 km a flat Web Mercator map of tiles takes over, to street level. Over either: the layers
//! (earthquakes, storms, wildfires, the ISS, flights, the peaks, Sir's "YOU"), the info card (the
//! time and the coordinates of the point in view), the place chip (a card with the weather on the
//! flat map), the tour's caption, the legend, the scale, the control column and, after an area
//! query, the drawer of results under the map.

use super::{stroke, STRIP_INK, STRIP_MUTED};
use crate::earth::{self as model, Bbox, Camera, Earth, EventKind, Flight, FlatView, GeoEvent, GlobeView, Iss, Layer, Look, Outlook, Overlay, Quake, TileKey, TileLayer};
use crate::earth_gl as gl;
use eframe::egui::{self, epaint::Vertex, pos2, vec2, Align2, Color32, FontFamily, FontId, Frame, Galley, Margin, Mesh, Painter, Pos2, Rect, Sense, Shape, Stroke, TextureId, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::f32::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "earth";
const TITLE: &str = "Earth";
/// The closed Earth's textures have been let go (once, not every frame it stays closed).
static LET_GO: AtomicBool = AtomicBool::new(true);

/// The globe mesh: rings from the middle of the view out to the limb, segments around.
const RINGS: usize = 40;
const SEGMENTS: usize = 128;
const SPACE: Color32 = Color32::from_rgb(2, 5, 9);
/// Cards, chips and callouts: opaque enough to hide what lies under them.
const CARD: Color32 = Color32::from_rgba_premultiplied(3, 7, 12, 226);
const QUAKE_LOW: Color32 = Color32::from_rgb(0xFF, 0xB0, 0x20);
const QUAKE_MID: Color32 = Color32::from_rgb(0xFF, 0x7A, 0x1A);
const QUAKE_HIGH: Color32 = Color32::from_rgb(0xFF, 0x4A, 0x2A);
/// Storm spirals: the status red's family (the reference's pink).
const STORM: Color32 = Color32::from_rgb(0xFF, 0x3C, 0x70);
const STORM_TEXT: Color32 = Color32::from_rgb(0xFF, 0xA2, 0xBE);
const FIRE: Color32 = Color32::from_rgb(0xFF, 0xA8, 0x2E);
const FIRE_CORE: Color32 = Color32::from_rgb(0xFF, 0xE6, 0x9A);
const WHITE_INK: Color32 = Color32::from_rgb(0xF2, 0xF8, 0xFF);
/// The geomagnetic north pole: the middle of the aurora's ring.
const MAGNETIC_POLE: (f64, f64) = (80.65, -72.68);
/// One beat of the halo around the strongest earthquake.
const PULSE_SECS: f64 = 1.2;
/// Inside a card: the margin, the gap between pieces of a row and between rows, an icon's box.
const PAD: f32 = 7.0;
const PIECE_GAP: f32 = 9.0;
const ROW_GAP: f32 = 2.0;
const ICON: f32 = 10.0;
/// Labels on the map at most (the strongest earthquakes first).
const MAX_LABELS: usize = 48;

fn mono(size: f32) -> FontId {
    FontId::monospace(super::ts(size))
}

fn display(size: f32) -> FontId {
    FontId::new(super::ts(size), FontFamily::Name("display".into()))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// `c` as light added to what lies under it (premultiplied, covering nothing): glows, the aurora.
fn light(c: Color32, a: f32) -> Color32 {
    let a = a.clamp(0.0, 1.0);
    let ch = |x: u8| (x as f32 * a).round() as u8;
    Color32::from_rgba_premultiplied(ch(c.r()), ch(c.g()), ch(c.b()), 0)
}

fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    let [r, g, b] = crate::theme_engine::from_hsv(h, s, v);
    Color32::from_rgb(r, g, b)
}

/// The theme's colours for one frame.
#[derive(Clone, Copy)]
struct Ink {
    /// The rim, the graticule, the footprint: the theme's primary, a little lighter.
    rim: Color32,
    accent: Color32,
    border: Color32,
    aurora: Color32,
    /// Borders and coastlines over the holographic map.
    lines: Color32,
}

fn ink() -> Ink {
    let p = crate::theme_engine::current();
    let (h, s, _) = crate::theme_engine::to_hsv(p.core_primary);
    let [r, g, b] = model::holo_palette(model::theme_hue()).border;
    Ink {
        // (The primary alone: a blend with the secondary is a third hue in a two-hue theme.)
        rim: mix(p.primary(), Color32::WHITE, 0.2),
        accent: crate::hud_board::active_colour(),
        border: crate::hud_board::border_colour(),
        // (A colourless theme gets a colourless aurora.)
        aurora: if s < 0.28 { hsv(h, 0.12, 0.85) } else { hsv(h - 25.0, 0.75, 0.88) },
        lines: Color32::from_rgb(r, g, b),
    }
}

fn quake_radius(mag: f64) -> f32 {
    (3.0 + (mag - 4.5) * 2.2).clamp(2.2, 13.0) as f32
}

fn quake_colour(mag: f64) -> Color32 {
    if mag >= 6.0 {
        QUAKE_HIGH
    } else if mag >= 5.0 {
        QUAKE_MID
    } else {
        QUAKE_LOW
    }
}

// ------------------------------------------------------------------------------ layout ----

/// Where the parts of the block go: the map, the drawer under it, the credits at the bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    map: Rect,
    drawer: Option<Rect>,
    credits: Rect,
}

impl Layout {
    fn new(full: Rect, drawer: bool, credits_h: f32) -> Self {
        let credits = Rect::from_min_max(pos2(full.left(), full.bottom() - credits_h), full.max);
        let above = credits.top() - 4.0;
        let room = (above - full.top()).max(0.0);
        let (bottom, drawer) = if drawer {
            let h = (room * 0.28).clamp(80.0, 190.0).min(room * 0.5);
            (above - h - 6.0, Some(Rect::from_min_max(pos2(full.left(), above - h), pos2(full.right(), above))))
        } else {
            (above, None)
        };
        Self { map: Rect::from_min_max(full.min, pos2(full.right(), bottom.max(full.top() + 40.0))), drawer, credits }
    }
}

/// The camera's two projections for this frame and how far the flat map has taken over.
struct Geo {
    map: Rect,
    globe: GlobeView,
    flat: FlatView,
    /// 0: the globe alone, 1: the flat map alone.
    flatness: f32,
}

impl Geo {
    fn new(cam: Camera, map: Rect) -> Self {
        let f = model::focal_for(map.width().min(map.height()) as f64);
        let centre = (map.center().x as f64, map.center().y as f64);
        Self {
            map,
            globe: GlobeView::new(cam.lat, cam.lon, cam.alt, f, centre),
            flat: FlatView::new(cam.lat, cam.lon, cam.alt, f, centre),
            flatness: model::smoothstep(model::FLAT_FROM_KM, model::FLAT_FULL_KM, cam.alt) as f32,
        }
    }

    /// What the layers are drawn with: the flat map once it is the larger part of the picture.
    fn proj(&self) -> Proj {
        if self.flatness >= 0.5 {
            Proj::Flat(self.flat)
        } else {
            Proj::Globe(self.globe)
        }
    }

    /// The point under a screen position.
    fn at(&self, p: Pos2) -> Option<(f64, f64)> {
        match self.proj() {
            Proj::Globe(g) => g.unproject(p.x as f64, p.y as f64),
            Proj::Flat(f) => Some(f.unproject(p.x as f64, p.y as f64)),
        }
    }

    /// The map in view as a box of lat/lon, while it is small enough to ask OpenSky about.
    fn bbox(&self, cam: Camera) -> Option<Bbox> {
        let km_per_px = 1.0 / self.proj().px_per_km();
        let half_lat = self.map.height() as f64 / 2.0 * km_per_px / 111.2;
        let half_lon = self.map.width() as f64 / 2.0 * km_per_px / (111.2 * cam.lat.to_radians().cos().max(0.05));
        let small = 2.0 * half_lat <= model::FLIGHTS_MAX_SPAN_DEG && 2.0 * half_lon <= model::FLIGHTS_MAX_SPAN_DEG;
        small.then(|| Bbox::around(cam.lat, cam.lon, half_lat, half_lon))
    }
}

#[derive(Clone, Copy)]
enum Proj {
    Globe(GlobeView),
    Flat(FlatView),
}

impl Proj {
    /// Where a point is on screen and how much of it shows (it fades out at the globe's limb).
    fn at(&self, lat: f64, lon: f64) -> Option<(Pos2, f32)> {
        match self {
            Proj::Globe(g) => {
                let h = g.horizon();
                g.project(lat, lon).filter(|(_, _, z)| *z > h + 0.002).map(|(x, y, z)| (pos2(x as f32, y as f32), model::smoothstep(h, h + 0.08, z) as f32))
            }
            Proj::Flat(f) => {
                let (x, y) = f.project(lat, lon);
                Some((pos2(x as f32, y as f32), 1.0))
            }
        }
    }

    fn px_per_km(&self) -> f64 {
        match self {
            Proj::Globe(g) => g.px_per_km(),
            Proj::Flat(f) => f.px_per_km(),
        }
    }

    /// A line of points as runs on screen: broken where it goes behind the globe, or where it
    /// leaves the flat map on one side for the other.
    fn runs(&self, points: &[(f64, f64)]) -> Vec<Vec<Pos2>> {
        let jump = match self {
            Proj::Flat(f) => (f.world / 2.0) as f32,
            Proj::Globe(_) => f32::INFINITY,
        };
        let mut runs: Vec<Vec<Pos2>> = Vec::new();
        let mut run: Vec<Pos2> = Vec::new();
        for &(lat, lon) in points {
            match self.at(lat, lon) {
                Some((p, _)) => {
                    if run.last().is_some_and(|last: &Pos2| (p.x - last.x).abs() > jump) {
                        runs.push(std::mem::take(&mut run));
                    }
                    run.push(p);
                }
                None => runs.push(std::mem::take(&mut run)),
            }
        }
        runs.push(run);
        runs.retain(|r| r.len() >= 2);
        runs
    }
}

/// A line through `points` with `steps` points of the great circle between each two.
fn densify(points: &[(f64, f64)], steps: usize) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(points.len() * steps + 1);
    for pair in points.windows(2) {
        for k in 0..steps {
            out.push(model::great_circle(pair[0], pair[1], k as f64 / steps as f64));
        }
    }
    out.extend(points.last().copied());
    out
}

// ---------------------------------------------------------------------------- the view ----

/// The Earth in the rest of `ui`: the map (the globe or the flat map) with its layers, cards and
/// controls, the drawer of an area query and the credits.
fn view(ui: &mut Ui, st: &mut Earth, rt: &Handle) {
    view_with(ui, st, rt, true);
}

/// `view`; without `fetch` nothing new is asked for (a closed window playing its exit).
fn view_with(ui: &mut Ui, st: &mut Earth, rt: &Handle, fetch: bool) {
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let dt = st.last_time.map_or(0.0, |t| (now - t).clamp(0.0, 0.1));
    st.last_time = Some(now);
    st.frame += 1;
    st.absorb(&ctx, now);
    let mut moving = st.step(now, dt);
    let full = ui.available_rect_before_wrap();
    if full.width() < 80.0 || full.height() < 80.0 {
        return;
    }
    let ink = ink();
    let credits = ui.painter().layout(credits_line(st), mono(9.0), STRIP_MUTED, full.width());
    let lay = Layout::new(full, st.area.is_some(), credits.size().y);
    ui.allocate_rect(full, Sense::hover());
    let map_resp = ui.interact(lay.map, ui.id().with("earth_map"), Sense::click_and_drag());
    if handle_input(&ctx, st, &Geo::new(st.cam, lay.map), &map_resp, now, dt) {
        moving = true;
    }
    let geo = Geo::new(st.cam, lay.map);

    let painter = ui.painter_at(lay.map);
    painter.rect_filled(lay.map, 0.0, SPACE);
    // The GPU draws the globe (and the flat map's base) once it has; the CPU until then, and for
    // good where the shader would not build. The offscreen HUD never runs the callback.
    let gpu = !gl::failed();
    let cpu = !gl::proven();
    let textures = globe_textures(st, now);
    if geo.flatness < 1.0 {
        stars(&painter, lay.map, mix(ink.rim, Color32::WHITE, 0.55), &geo.globe);
        if cpu {
            paint_globe(&painter, &geo.globe, &ink, st.look, textures, now);
        }
    }
    if gpu {
        let frame = gpu_frame(st, &geo, &ink, now, rt);
        painter.add(Shape::Callback(gl::callback(lay.map, frame)));
        if geo.flatness < 1.0 && !cpu {
            globe_marks(&painter, &geo.globe, &ink, st.look, now);
        }
    }
    if geo.flatness > 0.0 {
        paint_flat(&painter, st, &geo, textures.0, &ink, rt, cpu);
    }

    let controls = controls_layout(lay.map);
    let mut placer = Placer { bounds: lay.map.shrink(4.0), taken: vec![controls.area] };
    let cards = plan_cards(&painter, st, &geo, &controls, &mut placer, &ink);
    let mut picker = Picker { pointer: map_resp.hover_pos(), best: None };
    let tour_card = plan_tour_card(&painter, st, &geo, &mut placer);
    let (pulsing, movers) = paint_layers(&painter, st, &geo, &ink, now, &mut placer, &mut picker);
    cards.paint(&painter, &ink);
    if let Some(card) = tour_card {
        card.paint(&painter, &ink);
    }
    paint_controls(ui, &painter, st, &controls, &ink, rt);
    // live layers (C1): a click picks a satellite or a train (or lets go).
    if map_resp.clicked() && !controls.area.contains(map_resp.interact_pointer_pos().unwrap_or_default()) {
        crate::earth_layers::clicked(picker.best.as_ref().and_then(|(_, h)| if let Hover::Live(hit) = h { Some(hit) } else { None }));
    }
    if let (Some((_, hover)), Some(at)) = (picker.best, picker.pointer) {
        callout(&painter, &hover, at, lay.map, &ink);
    }
    painter.rect_stroke(lay.map, 0.0, stroke(1.0, ink.border));
    if let Some(rect) = lay.drawer {
        drawer(ui, rect, st, &ink);
    }
    ui.painter().galley(lay.credits.min, credits, STRIP_MUTED);

    let resting = !moving && now - st.still_since >= model::REST_SECS;
    let outlook = Outlook { lat: st.cam.lat, lon: st.cam.lon, alt: st.cam.alt, bbox: geo.bbox(st.cam), resting };
    if fetch {
        st.schedule(rt, &outlook);
    }
    // Frames only while something moves: 60 a second for the camera and a fade, fewer for the
    // halo's beat and the aircraft, one a second for the clock.
    if moving || st.globe_fade.progress(now) < 1.0 || map_resp.dragged() || (gpu && gl::proven() && gl::busy()) {
        super::next_animation_frame(&ctx);
    } else if pulsing || (st.overlays.radar && st.overlays.animate) {
        ctx.request_repaint_after(Duration::from_millis(90));
    } else if movers {
        ctx.request_repaint_after(Duration::from_millis(250));
    } else {
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// The sources of what is shown, the layers that are off left out.
fn credits_line(st: &Earth) -> String {
    let mut parts = vec![match (st.look, st.historic) {
        (Look::Holo, _) => "Imagery: NASA GIBS / EOSDIS",
        (_, Some(_)) => "Imagery: NASA GIBS / EOSDIS (VIIRS)",
        _ => st.imagery.credit(),
    }];
    if st.look == Look::Satellite {
        parts.push("Globe: NASA GIBS / EOSDIS (Blue Marble, Black Marble)");
        if st.overlays.labels {
            parts.push(crate::earth_sources::REFERENCE_CREDIT);
        }
    }
    if st.overlays.terrain {
        parts.push(crate::earth_sources::TERRAIN_CREDIT);
        parts.push(crate::earth_sources::PEAKS_CREDIT);
    }
    if st.overlays.radar {
        parts.push(crate::earth_sources::RADAR_CREDIT);
    }
    if st.overlays.clouds {
        parts.push(crate::earth_sources::IR_CREDIT);
    }
    if st.layers.quakes {
        parts.push("Quakes: USGS");
    }
    if st.layers.storms || st.layers.fires {
        parts.push("Events: NASA EONET");
    }
    if st.layers.iss {
        parts.push("ISS: wheretheiss.at");
    }
    if st.layers.flights {
        parts.push("Flights: OpenSky Network");
    }
    parts.extend(crate::earth_layers::credits()); // live layers (C1)
    parts.push("Weather & places: Open-Meteo, BigDataCloud");
    parts.join(" \u{b7} ")
}

/// A drag turns the globe (pans the map) and, let go, the map glides on a little; the wheel
/// zooms, a double click flies closer to the point under the pointer. True when the camera moved.
fn handle_input(ctx: &egui::Context, st: &mut Earth, geo: &Geo, resp: &egui::Response, now: f64, dt: f64) -> bool {
    /// The glide's time constant and the speed (points a second) below which it stops.
    const GLIDE_TAU: f64 = 0.32;
    const GLIDE_MIN: f32 = 12.0;
    let mut moved = false;
    if resp.dragged() {
        let d = resp.drag_delta();
        let before = st.inertia.unwrap_or((0.0, 0.0));
        if d != Vec2::ZERO {
            st.grab();
            pan(st, geo, d);
            st.still_since = now;
            moved = true;
        }
        // The drag's speed, smoothed over a few frames (a pause on the spot stills it).
        st.inertia = (dt > 0.0).then(|| {
            let v = d / dt as f32;
            (before.0 + (v.x - before.0) * 0.5, before.1 + (v.y - before.1) * 0.5)
        });
    } else if let Some((vx, vy)) = st.inertia {
        let decay = (-dt / GLIDE_TAU).exp() as f32;
        let v = vec2(vx, vy) * decay;
        if v.length() < GLIDE_MIN || st.motion.is_some() {
            st.inertia = None;
        } else {
            pan(st, geo, v * dt as f32);
            st.inertia = Some((v.x, v.y));
            st.still_since = now;
            moved = true;
        }
    }
    if resp.hovered() {
        let (scroll, zoom) = ctx.input(|i| (i.raw_scroll_delta.y, i.zoom_delta()));
        let mut factor = 1.0f64;
        if scroll.abs() > 0.0 {
            // A notch of the wheel (~50 points) halves or doubles the altitude.
            factor *= model::ZOOM_STEP.powf(-scroll as f64 / 50.0);
        }
        if (zoom - 1.0).abs() > 1e-4 {
            factor /= zoom as f64;
        }
        if (factor - 1.0).abs() > 1e-6 {
            st.zoom_by(factor);
            moved = true;
        }
    }
    if resp.double_clicked() {
        if let Some((lat, lon)) = resp.interact_pointer_pos().and_then(|p| geo.at(p)) {
            let alt = st.target().alt / model::ZOOM_STEP;
            st.fly_to(Camera { lat, lon, alt });
            moved = true;
        }
    }
    moved
}

/// The camera follows a drag: the point under the pointer stays under it (exactly on the flat
/// map, in the middle of the globe).
fn pan(st: &mut Earth, geo: &Geo, d: Vec2) {
    match geo.proj() {
        Proj::Flat(f) => {
            let y = model::merc_y(st.cam.lat.clamp(-80.0, 80.0)) - d.y as f64 / f.world;
            st.cam.lat = model::lat_of_merc_y(y.clamp(0.0, 1.0)).clamp(-80.0, 80.0);
            st.cam.lon = model::wrap_lon(st.cam.lon - d.x as f64 / f.world * 360.0);
        }
        Proj::Globe(g) => {
            let radians_per_px = (g.d - 1.0) / g.f;
            st.cam.lat = (st.cam.lat + (d.y as f64 * radians_per_px).to_degrees()).clamp(-85.0, 85.0);
            st.cam.lon = model::wrap_lon(st.cam.lon - (d.x as f64 * radians_per_px).to_degrees() / st.cam.lat.to_radians().cos().max(0.3));
        }
    }
}

// --------------------------------------------------------------------------- the globe ----

/// The globe's texture this frame: (texture, how much of it shows, the one under it while it
/// fades in). The holographic or the satellite imagery, or the procedural globe until it came.
fn globe_textures(st: &mut Earth, now: f64) -> (TextureId, f32, Option<TextureId>) {
    let procedural = st.procedural.as_ref().map(|t| t.id());
    let holo = st.holo.tex.as_ref().map(|t| t.id()).or(procedural);
    let id = match st.look {
        Look::Holo => holo,
        Look::Satellite => st.sat.tex.as_ref().map(|t| t.id()).or(holo),
    }
    .unwrap_or_default();
    let secs = if now - st.look_changed < 0.3 { model::LOOK_FADE_SECS } else { model::ARRIVAL_FADE_SECS };
    let share = st.globe_fade.show(id, now, secs);
    if share >= 1.0 {
        st.retired.clear();
    }
    (id, share, st.globe_fade.previous.filter(|_| share < 1.0))
}

fn paint_globe(p: &Painter, g: &GlobeView, ink: &Ink, look: Look, (id, share, under): (TextureId, f32, Option<TextureId>), now: f64) {
    let centre = pos2(g.cx as f32, g.cy as f32);
    let r = g.limb_radius() as f32;
    // The atmosphere outside the limb.
    ring(p, centre, r, r * 1.22 + 14.0, light(ink.rim, 0.16), light(ink.rim, 0.0));
    ring(p, centre, r, r * 1.07 + 6.0, light(ink.rim, 0.5), light(ink.rim, 0.0));
    let shade = |z: f32, a: f32| {
        let lit = match look {
            Look::Holo => 0.6 + 0.4 * z.max(0.0).powf(0.6),
            Look::Satellite => 0.74 + 0.26 * z.max(0.0).powf(0.5),
        };
        let v = (lit * a * 255.0).round() as u8;
        Color32::from_rgba_premultiplied(v, v, v, (a * 255.0).round() as u8)
    };
    if let Some(under) = under {
        p.add(Shape::mesh(globe_mesh(g, under, |z| shade(z, 1.0))));
    }
    p.add(Shape::mesh(globe_mesh(g, id, |z| shade(z, share))));
    // The fresnel rim inside the limb, the graticule, the aurora, the limb itself.
    ring(p, centre, r * 0.8, r, light(ink.rim, 0.0), light(ink.rim, 0.42));
    graticule(p, g, ink.rim.gamma_multiply(0.16));
    aurora(p, g, ink.aurora, now);
    p.circle_stroke(centre, r, stroke(1.2, ink.rim.gamma_multiply(0.85)));
}

/// What the GPU globe has no shader for: the hologram's graticule and outline, the aurora.
fn globe_marks(p: &Painter, g: &GlobeView, ink: &Ink, look: Look, now: f64) {
    if look == Look::Holo {
        graticule(p, g, ink.rim.gamma_multiply(0.16));
        p.circle_stroke(pos2(g.cx as f32, g.cy as f32), g.limb_radius() as f32, stroke(1.0, ink.rim.gamma_multiply(0.7)));
    }
    aurora(p, g, ink.aurora, now);
}

fn rgb(c: Color32) -> [f32; 3] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

/// The level of Web Mercator tiles whose pixels are the screen's in the middle of the view.
fn tile_zoom(geo: &Geo) -> f64 {
    (2.0 * std::f64::consts::PI * model::R_KM * geo.proj().px_per_km() / 256.0).max(1.0).log2()
}

/// The GPU globe's frame: the camera, the sun, the look, the textures and the clipmaps (whose
/// tiles are asked for here).
fn gpu_frame(st: &mut Earth, geo: &Geo, ink: &Ink, now: f64, rt: &Handle) -> gl::Frame {
    let g = &geo.globe;
    let photo = st.look == Look::Satellite;
    let (sun_lat, sun_lon) = crate::earth_sources::subsolar_point(chrono::Utc::now());
    let p = model::holo_palette(model::theme_hue());
    let c = |v: [u8; 3]| [v[0] as f32 / 255.0, v[1] as f32 / 255.0, v[2] as f32 / 255.0];
    let day = match (photo, st.historic) {
        (false, _) if gl::ready(gl::Slot::Relief) => gl::Slot::Relief,
        (true, Some(_)) if gl::ready(gl::Slot::Historic) => gl::Slot::Historic,
        _ => gl::Slot::Day,
    };
    // The clipmaps: the imagery once the base texture is magnified, the overlays at any height;
    // none on the flat map (its own tiles cover it) or before the GPU has drawn.
    let z = tile_zoom(geo);
    let base_z = if gl::caps().is_some_and(|c| c.dxt1 && c.max_side >= 8192) { 5.0 } else { 4.0 };
    let clips = if gl::proven() && geo.flatness < 1.0 {
        let detail = (z > base_z - 0.7).then(|| ((z + 0.5).floor().clamp(3.0, 12.0) as u8, model::smoothstep(base_z - 0.7, base_z, z) as f32));
        let overlay = Some(z.round().clamp(3.0, 7.0) as u8);
        st.update_clips(rt, (st.cam.lat, st.cam.lon), detail, overlay, now)
    } else {
        [gl::ClipUse::default(), gl::ClipUse::default()]
    };
    let live = photo && st.historic.is_none();
    gl::Frame {
        centre: [g.cx as f32, g.cy as f32],
        f: g.f as f32,
        d: g.d as f32,
        basis: gl::view_basis(g.lat0, g.lon0),
        flatness: geo.flatness,
        flat: [model::merc_x(geo.flat.lon0) as f32, model::merc_y(geo.flat.lat0) as f32, geo.flat.world as f32],
        sun: gl::world_unit(sun_lat, sun_lon),
        holo: if photo { 0.0 } else { 1.0 },
        night: if live { 1.0 } else { 0.0 },
        borders: if photo { 0.0 } else { 1.0 },
        glint: if live { 1.0 } else { 0.0 },
        rim: rgb(ink.rim),
        palette: [c(p.ocean_deep), c(p.ocean_shallow), c(p.land_dark), c(p.land), c(p.dot), c(p.coast), c(p.border)],
        day,
        detail: clips[0],
        overlay: clips[1],
        time: (now % 10_000.0) as f32,
    }
}

/// The visible cap of the globe as a polar grid around the middle of the view: the outermost
/// ring is the limb. Each vertex samples the equirectangular texture at its own lat/lon.
fn globe_mesh(g: &GlobeView, texture: TextureId, tint: impl Fn(f32) -> Color32) -> Mesh {
    let mut mesh = Mesh::with_texture(texture);
    let theta_max = g.horizon().clamp(-1.0, 1.0).acos();
    let push = |mesh: &mut Mesh, v: [f64; 3]| {
        let (lat, lon) = g.from_view(v);
        let (x, y) = g.screen(v);
        let uv = pos2(((lon + 180.0) / 360.0) as f32, ((90.0 - lat) / 180.0) as f32);
        mesh.vertices.push(Vertex { pos: pos2(x as f32, y as f32), uv, color: tint(v[2] as f32) });
    };
    push(&mut mesh, [0.0, 0.0, 1.0]);
    for i in 1..=RINGS {
        let theta = theta_max * i as f64 / RINGS as f64;
        let (s, c) = theta.sin_cos();
        for j in 0..SEGMENTS {
            let phi = std::f64::consts::TAU * j as f64 / SEGMENTS as f64;
            push(&mut mesh, [s * phi.cos(), s * phi.sin(), c]);
        }
    }
    let at = |i: usize, j: usize| (1 + (i - 1) * SEGMENTS + j % SEGMENTS) as u32;
    for j in 0..SEGMENTS {
        triangle(&mut mesh, [0, at(1, j), at(1, j + 1)]);
    }
    for i in 1..RINGS {
        for j in 0..SEGMENTS {
            let (a, b, c, d) = (at(i, j), at(i, j + 1), at(i + 1, j), at(i + 1, j + 1));
            triangle(&mut mesh, [a, c, b]);
            triangle(&mut mesh, [b, c, d]);
        }
    }
    mesh
}

/// A triangle of the globe. One across the date line gets copies of its western vertices a
/// texture-width on (the texture repeats), or it would smear the whole map backwards.
fn triangle(mesh: &mut Mesh, ids: [u32; 3]) {
    let u = ids.map(|k| mesh.vertices[k as usize].uv.x);
    if u[0].max(u[1]).max(u[2]) - u[0].min(u[1]).min(u[2]) <= 0.5 {
        mesh.add_triangle(ids[0], ids[1], ids[2]);
        return;
    }
    let ids = ids.map(|k| {
        let v = mesh.vertices[k as usize];
        if v.uv.x < 0.5 {
            mesh.vertices.push(Vertex { uv: pos2(v.uv.x + 1.0, v.uv.y), ..v });
            (mesh.vertices.len() - 1) as u32
        } else {
            k
        }
    });
    mesh.add_triangle(ids[0], ids[1], ids[2]);
}

/// A ring between two radii, its colour running from the inner edge to the outer one.
fn ring(p: &Painter, c: Pos2, r_in: f32, r_out: f32, inner: Color32, outer: Color32) {
    const N: u32 = 96;
    let mut mesh = Mesh::default();
    for k in 0..=N {
        let a = TAU * k as f32 / N as f32;
        let dir = vec2(a.cos(), a.sin());
        mesh.colored_vertex(c + dir * r_in, inner);
        mesh.colored_vertex(c + dir * r_out, outer);
    }
    for k in 0..N {
        let b = 2 * k;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 3, b + 2);
    }
    p.add(Shape::mesh(mesh));
}

/// Parallels and meridians every 15 degrees.
fn graticule(p: &Painter, g: &GlobeView, colour: Color32) {
    let proj = Proj::Globe(*g);
    let line = stroke(1.0, colour);
    for lat in (-75..=75).step_by(15) {
        let points: Vec<(f64, f64)> = (0..=120).map(|k| (lat as f64, -180.0 + 3.0 * k as f64)).collect();
        for run in proj.runs(&points) {
            p.add(Shape::line(run, line));
        }
    }
    for lon in (-180..180).step_by(15) {
        let points: Vec<(f64, f64)> = (0..=60).map(|k| (-90.0 + 3.0 * k as f64, lon as f64)).collect();
        for run in proj.runs(&points) {
            p.add(Shape::line(run, line));
        }
    }
}

/// The northern lights: a soft band around the geomagnetic pole, brighter in slow waves.
fn aurora(p: &Painter, g: &GlobeView, colour: Color32, now: f64) {
    const STEPS: usize = 96;
    const COLAT: [f64; 5] = [13.0, 16.0, 18.0, 20.0, 24.0];
    const ALPHA: [f32; 5] = [0.0, 0.26, 0.4, 0.22, 0.0];
    let h = g.horizon();
    let mut mesh = Mesh::default();
    let mut shown = Vec::with_capacity((STEPS + 1) * COLAT.len());
    for k in 0..=STEPS {
        let bearing = 360.0 * k as f64 / STEPS as f64;
        let wave = 0.55 + 0.45 * (0.5 + 0.5 * (bearing.to_radians() * 3.0 + now * 0.4).sin()) as f32;
        for (i, colat) in COLAT.iter().enumerate() {
            let (lat, lon) = model::destination(MAGNETIC_POLE, bearing, colat * 111.195);
            let v = g.view_coords(lat, lon);
            let (x, y) = g.screen(v);
            let fade = model::smoothstep(h, h + 0.12, v[2]) as f32;
            mesh.colored_vertex(pos2(x as f32, y as f32), light(colour, ALPHA[i] * wave * fade));
            shown.push(v[2] > h);
        }
    }
    let n = COLAT.len() as u32;
    for k in 0..STEPS as u32 {
        for i in 0..n - 1 {
            let (a, b, c, d) = (k * n + i, k * n + i + 1, (k + 1) * n + i, (k + 1) * n + i + 1);
            for tri in [[a, b, c], [b, d, c]] {
                if tri.iter().all(|v| shown[*v as usize]) {
                    mesh.add_triangle(tri[0], tri[1], tri[2]);
                }
            }
        }
    }
    p.add(Shape::mesh(mesh));
}

/// A fixed starfield (the same stars every frame) around the globe.
fn stars(p: &Painter, map: Rect, colour: Color32, g: &GlobeView) {
    let mut seed: u32 = 0x9E37_79B9;
    let mut next = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / 16_777_216.0
    };
    let count = ((map.width() * map.height()) / 3_500.0).clamp(40.0, 260.0) as usize;
    let (centre, r) = (pos2(g.cx as f32, g.cy as f32), g.limb_radius() as f32);
    for _ in 0..count {
        let at = pos2(map.left() + next() * map.width(), map.top() + next() * map.height());
        let (size, bright) = (next(), next());
        if (at - centre).length() < r + 2.0 {
            continue;
        }
        p.circle_filled(at, 0.45 + size * size * 1.1, colour.gamma_multiply(0.15 + 0.55 * bright * bright));
    }
}

// ------------------------------------------------------------------------ the flat map ----

/// The flat map: the globe's texture in Web Mercator (drawn by the GPU's pass when it runs, as a
/// mesh by `cpu`) under the tiles: the imagery, the terrain's shading, the clouds and the radar,
/// then the labels and borders, all at `geo.flatness`.
#[allow(clippy::too_many_arguments)]
fn paint_flat(p: &Painter, st: &mut Earth, geo: &Geo, base: TextureId, ink: &Ink, rt: &Handle, cpu: bool) {
    let (map, flat) = (geo.map, &geo.flat);
    let tint = Color32::WHITE.gamma_multiply(geo.flatness);
    if cpu {
        const COLS: u32 = 24;
        const ROWS: u32 = 18;
        let mut mesh = Mesh::with_texture(base);
        for r in 0..=ROWS {
            for c in 0..=COLS {
                let at = pos2(map.left() + map.width() * c as f32 / COLS as f32, map.top() + map.height() * r as f32 / ROWS as f32);
                let (lat, lon) = flat.unproject(at.x as f64, at.y as f64);
                mesh.vertices.push(Vertex { pos: at, uv: pos2(((lon + 180.0) / 360.0) as f32, ((90.0 - lat) / 180.0) as f32), color: tint });
            }
        }
        for r in 0..ROWS {
            for c in 0..COLS {
                let i = r * (COLS + 1) + c;
                mesh.add_triangle(i, i + 1, i + COLS + 1);
                mesh.add_triangle(i + 1, i + COLS + 2, i + COLS + 1);
            }
        }
        p.add(Shape::mesh(mesh));
    }
    let zoom = flat.zoom();
    let level = |layer: TileLayer, bias: f64| (zoom + bias).floor().clamp(0.0, layer.max_level() as f64) as u8;
    let imagery = st.imagery_layer();
    tiles(p, st, flat, map, imagery, level(imagery, 0.3), tint, rt);
    if let Some(terrain) = st.terrain_layer() {
        tiles(p, st, flat, map, terrain, level(terrain, 0.3), tint, rt);
    }
    // The hologram's grid (a satellite photo stays clean once it is there).
    if st.look == Look::Holo || (cpu && st.sat.tex.is_none()) {
        flat_graticule(p, flat, map, ink.rim.gamma_multiply(0.18 * geo.flatness));
    }
    let now = p.ctx().input(|i| i.time);
    for layer in st.overlay_layers(now) {
        match layer {
            // The hologram's lines in its own colour, then its names.
            TileLayer::Labels => {
                let detail = level(TileLayer::Features, 0.5);
                tiles(p, st, flat, map, TileLayer::Features, detail, ink.lines.gamma_multiply(0.85 * geo.flatness), rt);
                tiles(p, st, flat, map, TileLayer::Labels, detail, tint, rt);
            }
            TileLayer::Radar(_) => tiles(p, st, flat, map, layer, level(layer, 0.0), Color32::WHITE.gamma_multiply(0.85 * geo.flatness), rt),
            _ => tiles(p, st, flat, map, layer, level(layer, 0.5), tint, rt),
        }
    }
}

/// Parallels and meridians over the flat map (straight lines in Web Mercator), every 30 degrees
/// down to every quarter of one: as many as stay ~70 px apart.
fn flat_graticule(p: &Painter, flat: &FlatView, map: Rect, colour: Color32) {
    let px_per_deg = flat.world / 360.0;
    if !px_per_deg.is_finite() || px_per_deg <= 0.0 {
        return;
    }
    let step = [0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0].into_iter().find(|s| s * px_per_deg >= 70.0).unwrap_or(30.0);
    let line = stroke(1.0, colour);
    let (north, west) = flat.unproject(map.left() as f64, map.top() as f64);
    let (south, east) = flat.unproject(map.right() as f64, map.bottom() as f64);
    let first = (west / step).ceil() as i64;
    for k in first..=(east / step).floor() as i64 {
        let (x, _) = flat.project(0.0, k as f64 * step);
        p.line_segment([pos2(x as f32, map.top()), pos2(x as f32, map.bottom())], line);
    }
    let first = (south.max(-85.0) / step).ceil() as i64;
    for k in first..=(north.min(85.0) / step).floor() as i64 {
        let (_, y) = flat.project(k as f64 * step, 0.0);
        p.line_segment([pos2(map.left(), y as f32), pos2(map.right(), y as f32)], line);
    }
}

/// One tile layer over the map; a tile not here yet is asked for, and the nearest ancestor that
/// is here stands in for it.
#[allow(clippy::too_many_arguments)]
fn tiles(p: &Painter, st: &mut Earth, flat: &FlatView, map: Rect, layer: TileLayer, z: u8, tint: Color32, rt: &Handle) {
    let n = 1i64 << z;
    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let ctx = p.ctx().clone();
    for (x, y) in flat.tiles([map.left() as f64, map.top() as f64, map.right() as f64, map.bottom() as f64], z) {
        let key = TileKey { layer, z, x: x.rem_euclid(n) as u32, y: y as u32 };
        let r = flat.tile_rect(z, x, y);
        let rect = Rect::from_min_max(pos2(r[0] as f32, r[1] as f32), pos2(r[2] as f32, r[3] as f32));
        if let Some(texture) = st.tile(&ctx, key) {
            p.image(texture, rect, whole, tint);
            continue;
        }
        st.request_tile(key, rt);
        // An ancestor stands in (up to six levels: street level shows the town's tile blurred
        // rather than the globe's texture).
        let (mut k, mut uv) = (key, whole);
        for _ in 0..6 {
            let Some(parent) = k.parent() else { break };
            let (qx, qy) = ((k.x % 2) as f32, (k.y % 2) as f32);
            uv = Rect::from_min_size(pos2((qx + uv.min.x) / 2.0, (qy + uv.min.y) / 2.0), uv.size() / 2.0);
            k = parent;
            if let Some(texture) = st.tile(&ctx, k) {
                p.image(texture, rect, uv, tint);
                break;
            }
        }
    }
}

// -------------------------------------------------------------------------- the layers ----

/// What the pointer is over.
#[derive(Clone)]
enum Hover {
    Quake(Quake),
    Event(GeoEvent),
    Plane(Flight),
    Iss(Iss),
    // live layers (C1)
    Live(crate::earth_layers::Hit),
}

/// The thing nearest the pointer among those drawn.
struct Picker {
    pointer: Option<Pos2>,
    best: Option<(f32, Hover)>,
}

impl Picker {
    fn consider(&mut self, at: Pos2, r: f32, what: impl FnOnce() -> Hover) {
        let Some(pointer) = self.pointer else { return };
        let d = (pointer - at).length();
        if d <= r.max(6.0) + 3.0 && self.best.as_ref().is_none_or(|(best, _)| d < *best) {
            self.best = Some((d, what()));
        }
    }
}

/// Room on the map: the cards and labels placed so far.
struct Placer {
    bounds: Rect,
    taken: Vec<Rect>,
}

impl Placer {
    fn fits(&self, r: Rect) -> bool {
        self.bounds.contains_rect(r) && !self.taken.iter().any(|t| t.intersects(r))
    }

    fn take(&mut self, r: Rect) -> bool {
        let fits = self.fits(r);
        if fits {
            self.taken.push(r);
        }
        fits
    }

    /// A label beside a glyph: right of it, left, above or below, wherever it fits first.
    fn label(&mut self, p: &Painter, text: &Label) {
        let galley = p.layout_no_wrap(text.text.clone(), text.font.clone(), text.colour);
        let (at, gap, s) = (text.at, text.gap, galley.size());
        let spots = [pos2(at.x + gap, at.y - s.y / 2.0), pos2(at.x - gap - s.x, at.y - s.y / 2.0), pos2(at.x - s.x / 2.0, at.y - gap - s.y), pos2(at.x - s.x / 2.0, at.y + gap)];
        for spot in spots {
            let r = Rect::from_min_size(spot, s).expand(1.5);
            if self.take(r) {
                p.rect_filled(r, 0.0, Color32::from_black_alpha(150));
                p.galley(spot, galley, text.colour);
                return;
            }
        }
    }
}

/// A label the layers ask for; the lowest rank is placed first.
struct Label {
    at: Pos2,
    gap: f32,
    text: String,
    font: FontId,
    colour: Color32,
    rank: f32,
}

/// The ISS now: from its last two fixes along their great circle, or its last fix.
fn iss_now(st: &Earth) -> Option<Iss> {
    let last = *st.iss.items.last()?;
    if let [a, b] = st.iss.items.as_slice() {
        let span = (b.at - a.at) as f64;
        if span > 0.5 && span < 120.0 {
            let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
            let t = ((now - a.at as f64) / span).clamp(0.0, 1.0 + 30.0 / span);
            let (lat, lon) = model::great_circle((a.lat, a.lon), (b.lat, b.lon), t);
            return Some(Iss { lat, lon, ..last });
        }
    }
    Some(last)
}

/// The layers on the map; (a halo beats, something moves on its own).
fn paint_layers(p: &Painter, st: &Earth, geo: &Geo, ink: &Ink, now: f64, placer: &mut Placer, picker: &mut Picker) -> (bool, bool) {
    let proj = geo.proj();
    let map = geo.map;
    let inside = |at: Pos2, r: f32| map.expand(r).contains(at);
    super::media_view::paint_earth_cameras(p, map, &|lat, lon| proj.at(lat, lon)); // cameras (C2)
    let (mut pulsing, mut movers) = (false, false);
    let mut labels: Vec<Label> = Vec::new();
    // What no label may cover: the earthquakes, storms, fires, the ISS and the marker.
    let mut glyphs: Vec<Rect> = Vec::new();
    // The area of a query: its circle (filled while all of it shows).
    if let Some(area) = &st.area {
        let points = model::circle_points((area.lat, area.lon), area.radius_km, 120);
        let runs = proj.runs(&points);
        if let [whole] = runs.as_slice() {
            if whole.len() == points.len() {
                p.add(Shape::convex_polygon(whole.clone(), ink.accent.gamma_multiply(0.07), Stroke::NONE));
            }
        }
        for run in runs {
            p.add(Shape::line(run, stroke(1.3, ink.accent)));
        }
    }
    // The ISS's footprint and ground track.
    let iss = if st.layers.iss { iss_now(st) } else { None };
    if let Some(iss) = iss {
        let footprint = model::circle_points((iss.lat, iss.lon), model::footprint_radius_km(iss.alt_km), 96);
        for run in proj.runs(&footprint) {
            p.extend(crate::earth_layers::dashed_on(map, &run, stroke(1.0, ink.rim.gamma_multiply(0.75)), 5.0, 4.0));
        }
        for run in proj.runs(&densify(&st.track.items, 10)) {
            p.add(Shape::line(run, stroke(1.3, WHITE_INK.gamma_multiply(0.55))));
        }
    }
    if st.layers.fires {
        for e in st.events.items.iter().filter(|e| e.kind == EventKind::Fire) {
            let Some((at, vis)) = proj.at(e.lat, e.lon).filter(|(at, _)| inside(*at, 10.0)) else { continue };
            flame(p, at, 11.0, FIRE.gamma_multiply(vis), Some(FIRE_CORE.gamma_multiply(vis)));
            glyphs.push(Rect::from_center_size(at, vec2(8.0, 11.0)));
            picker.consider(at, 6.0, || Hover::Event(e.clone()));
        }
    }
    if st.layers.storms {
        for e in st.events.items.iter().filter(|e| e.kind == EventKind::Storm) {
            let Some((at, vis)) = proj.at(e.lat, e.lon).filter(|(at, _)| inside(*at, 12.0)) else { continue };
            spiral(p, at, 9.0, STORM.gamma_multiply(vis));
            glyphs.push(Rect::from_center_size(at, Vec2::splat(18.0)));
            labels.push(Label { at, gap: 12.0, text: e.label.clone(), font: mono(10.0), colour: STORM_TEXT.gamma_multiply(vis), rank: 1.0 });
            picker.consider(at, 9.0, || Hover::Event(e.clone()));
        }
    }
    if st.layers.quakes {
        let mut list: Vec<&Quake> = st.quakes.items.iter().collect();
        let found = st.area.as_ref().and_then(|a| a.results.as_ref());
        for q in found.into_iter().flatten() {
            if q.id.is_empty() || !list.iter().any(|k| k.id == q.id) {
                list.push(q);
            }
        }
        // The halo: the strongest of the area's quakes when a query found some, of all otherwise.
        let strongest: Option<Quake> = match found.filter(|f| !f.is_empty()) {
            Some(area) => area.iter().max_by(|a, b| a.mag.total_cmp(&b.mag)).cloned(),
            None => list.iter().copied().max_by(|a, b| a.mag.total_cmp(&b.mag)).cloned(),
        };
        list.sort_by(|a, b| a.mag.total_cmp(&b.mag));
        for q in list {
            let r = quake_radius(q.mag);
            let Some((at, vis)) = proj.at(q.lat, q.lon).filter(|(at, _)| inside(*at, r * 2.0)) else { continue };
            let c = quake_colour(q.mag);
            p.circle_filled(at, r * 2.3, c.gamma_multiply(0.10 * vis));
            p.circle_filled(at, r * 1.55, c.gamma_multiply(0.22 * vis));
            p.circle_filled(at, r, c.gamma_multiply(vis));
            p.circle_filled(at, r * 0.42, mix(c, Color32::WHITE, 0.65).gamma_multiply(vis));
            glyphs.push(Rect::from_center_size(at, Vec2::splat(2.0 * r)));
            if strongest.as_ref() == Some(q) {
                for k in 0..2 {
                    let phase = ((now / PULSE_SECS) + k as f64 * 0.5).fract() as f32;
                    p.circle_stroke(at, r * (1.2 + 2.6 * phase), stroke(1.4, crate::hud_board::PIN_RED.gamma_multiply((1.0 - phase) * 0.9 * vis)));
                }
                pulsing = true;
            }
            labels.push(Label { at, gap: r + 4.0, text: format!("{:.1}", q.mag), font: mono(10.0), colour: WHITE_INK.gamma_multiply(0.92 * vis), rank: 10.0 - q.mag as f32 });
            picker.consider(at, r, || Hover::Quake(q.clone()));
        }
    }
    if st.layers.flights && st.cam.alt <= model::FLIGHTS_MAX_ALT_KM * 1.15 {
        let now_s = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        for f in &st.flights.items {
            let (lat, lon) = f.reckoned(now_s - f.at as f64);
            let Some((at, vis)) = proj.at(lat, lon).filter(|(at, _)| inside(*at, 8.0)) else { continue };
            plane(p, at, 6.5, f.track as f32, WHITE_INK.gamma_multiply(vis));
            picker.consider(at, 7.0, || Hover::Plane(f.clone()));
        }
        movers |= !st.flights.items.is_empty();
    }
    // live layers (C1): satellites, trains, traffic (earth_layers.rs paints, the Earth places).
    {
        let view = crate::earth_layers::MapView { map, alt_km: st.cam.alt, centre: (st.cam.lat, st.cam.lon), px_per_km: proj.px_per_km(), at: &|lat, lon| proj.at(lat, lon), runs: &|pts| proj.runs(pts), pointer: picker.pointer, rim: ink.rim, accent: ink.accent, iss_layer: iss.is_some() };
        let live = crate::earth_layers::paint(p, &view);
        glyphs.extend(live.glyphs);
        labels.extend(live.labels.into_iter().map(|l| Label { at: l.at, gap: l.gap, text: l.text, font: mono(10.0), colour: l.colour, rank: l.rank }));
        if let Some((at, r, hit)) = live.hover {
            picker.consider(at, r, || Hover::Live(hit));
        }
        movers |= live.moving;
    }
    if let Some(iss) = iss {
        if let Some((at, vis)) = proj.at(iss.lat, iss.lon).filter(|(at, _)| inside(*at, 12.0)) {
            p.circle_filled(at, 11.0, light(ink.rim, 0.3 * vis));
            iss_glyph(p, at, 7.0, WHITE_INK.gamma_multiply(vis));
            glyphs.push(Rect::from_center_size(at, vec2(14.0, 8.0)));
            labels.push(Label { at, gap: 12.0, text: "ISS".into(), font: mono(10.0), colour: WHITE_INK.gamma_multiply(vis), rank: 0.5 });
            picker.consider(at, 9.0, || Hover::Iss(iss));
        }
        movers = true;
    }
    if let Some(m) = &st.marker {
        if let Some((at, vis)) = proj.at(m.lat, m.lon).filter(|(at, _)| inside(*at, 14.0)) {
            reticle(p, at, WHITE_INK.gamma_multiply(vis));
            glyphs.push(Rect::from_center_size(at, Vec2::splat(28.0)));
            labels.push(Label { at, gap: 16.0, text: m.name.clone(), font: super::bold(11.0), colour: WHITE_INK.gamma_multiply(vis), rank: 0.0 });
        }
    }
    // The peaks in view on the shaded terrain: the highest named first.
    if st.overlays.terrain && st.cam.alt <= model::PEAKS_MAX_ALT_KM {
        let mut shown = 0;
        for peak in &st.peaks.items {
            if shown >= 40 {
                break;
            }
            let Some((at, vis)) = proj.at(peak.lat, peak.lon).filter(|(at, _)| map.shrink(6.0).contains(*at)) else { continue };
            shown += 1;
            let c = WHITE_INK.gamma_multiply(vis);
            p.add(Shape::convex_polygon(vec![at + vec2(0.0, -4.5), at + vec2(4.0, 3.0), at + vec2(-4.0, 3.0)], c, Stroke::NONE));
            glyphs.push(Rect::from_center_size(at, Vec2::splat(9.0)));
            labels.push(Label { at, gap: 7.0, text: format!("{} {} m", peak.name, model::group_digits(peak.ele_m.round() as i64)), font: mono(10.0), colour: c, rank: 2.0 + shown as f32 * 0.01 });
        }
    }
    // Sir himself: a beating dot in the accent, his accuracy around it.
    if let Some(me) = &st.me {
        let radius_km = me.accuracy_m / 1000.0;
        let circle = model::circle_points((me.lat, me.lon), radius_km, 72);
        let r_px = (proj.px_per_km() * radius_km) as f32;
        if r_px > 6.0 {
            for run in proj.runs(&circle) {
                p.extend(crate::earth_layers::dashed_on(map, &run, stroke(1.2, ink.accent.gamma_multiply(0.85)), 6.0, 4.0));
            }
            if let [whole] = proj.runs(&circle).as_slice() {
                if whole.len() == circle.len() {
                    p.add(Shape::convex_polygon(whole.clone(), ink.accent.gamma_multiply(0.08), Stroke::NONE));
                }
            }
        }
        if let Some((at, vis)) = proj.at(me.lat, me.lon).filter(|(at, _)| inside(*at, 16.0)) {
            let phase = (((now - st.me_since) / PULSE_SECS).max(0.0).fract()) as f32;
            p.circle_stroke(at, 6.0 + 14.0 * phase, stroke(1.6, ink.accent.gamma_multiply((1.0 - phase) * vis)));
            p.circle_filled(at, 6.5, Color32::from_black_alpha((170.0 * vis) as u8));
            p.circle_filled(at, 5.0, ink.accent.gamma_multiply(vis));
            p.circle_filled(at, 2.0, WHITE_INK.gamma_multiply(vis));
            glyphs.push(Rect::from_center_size(at, Vec2::splat(16.0)));
            labels.push(Label { at, gap: 12.0, text: "YOU".into(), font: super::bold(11.0), colour: WHITE_INK.gamma_multiply(vis), rank: -1.0 });
            pulsing = true;
        }
    }
    // (Each label's own glyph lies inside its gap: none of its spots touches it.)
    placer.taken.extend(glyphs);
    labels.sort_by(|a, b| a.rank.total_cmp(&b.rank));
    for label in labels.iter().take(MAX_LABELS) {
        placer.label(p, label);
    }
    (pulsing, movers)
}

/// A storm: two spiral arms around an eye.
fn spiral(p: &Painter, c: Pos2, r: f32, colour: Color32) {
    let line = stroke((r * 0.17).clamp(1.0, 1.8), colour);
    for arm in 0..2 {
        let base = arm as f32 * PI;
        let points: Vec<Pos2> = (0..=12)
            .map(|k| {
                let t = k as f32 / 12.0;
                let a = base + t * 1.25 * PI;
                c + vec2(a.cos(), -a.sin()) * (r * (0.28 + 0.72 * t))
            })
            .collect();
        p.add(Shape::line(points, line));
    }
    p.circle_filled(c, r * 0.26, colour);
}

/// A wildfire: a flame `h` tall (a point on top, a round bottom) with a brighter core.
fn flame(p: &Painter, c: Pos2, h: f32, fill: Color32, core: Option<Color32>) {
    let drop = |h: f32, centre: Pos2| -> Vec<Pos2> {
        let w = h * 0.36;
        let middle = centre.y + h * 0.5 - w;
        let mut points = vec![pos2(centre.x, centre.y - h * 0.5)];
        points.extend((0..=10).map(|k| {
            let a = PI * k as f32 / 10.0;
            pos2(centre.x + w * a.cos(), middle + w * a.sin())
        }));
        points
    };
    p.add(Shape::convex_polygon(drop(h, c), fill, Stroke::NONE));
    if let Some(core) = core {
        p.add(Shape::convex_polygon(drop(h * 0.5, c + vec2(0.0, h * 0.2)), core, Stroke::NONE));
    }
}

/// `points` (x right, y forward, in units of `size`) turned to `heading` (degrees clockwise from
/// north, which is up) around `c`.
fn turned(c: Pos2, size: f32, heading: f32, points: &[(f32, f32)]) -> Vec<Pos2> {
    let (s, k) = heading.to_radians().sin_cos();
    points.iter().map(|&(x, y)| c + vec2(x * k + y * s, x * s - y * k) * size).collect()
}

/// An aircraft seen from above, nose along its heading.
fn plane(p: &Painter, c: Pos2, size: f32, heading: f32, colour: Color32) {
    const PARTS: [&[(f32, f32)]; 5] = [
        &[(0.0, 1.1), (0.11, 0.8), (0.11, -0.8), (0.0, -0.95), (-0.11, -0.8), (-0.11, 0.8)],
        &[(0.0, 0.35), (0.95, -0.15), (0.95, -0.3), (0.0, -0.05)],
        &[(0.0, 0.35), (0.0, -0.05), (-0.95, -0.3), (-0.95, -0.15)],
        &[(0.0, -0.62), (0.36, -0.86), (0.36, -0.96), (0.0, -0.8)],
        &[(0.0, -0.62), (0.0, -0.8), (-0.36, -0.96), (-0.36, -0.86)],
    ];
    for part in PARTS {
        p.add(Shape::convex_polygon(turned(c, size, heading, part), colour, Stroke::NONE));
    }
}

/// The space station: a module between two solar wings.
fn iss_glyph(p: &Painter, c: Pos2, size: f32, colour: Color32) {
    const PARTS: [&[(f32, f32)]; 3] = [
        &[(-0.25, 0.25), (0.25, 0.25), (0.25, -0.25), (-0.25, -0.25)],
        &[(0.42, 0.2), (1.0, 0.2), (1.0, -0.2), (0.42, -0.2)],
        &[(-1.0, 0.2), (-0.42, 0.2), (-0.42, -0.2), (-1.0, -0.2)],
    ];
    for part in PARTS {
        p.add(Shape::convex_polygon(turned(c, size, 0.0, part), colour, Stroke::NONE));
    }
    p.line_segment([c - vec2(size * 0.42, 0.0), c + vec2(size * 0.42, 0.0)], stroke(1.0, colour));
}

/// The place JAI flew to: a reticle.
fn reticle(p: &Painter, c: Pos2, colour: Color32) {
    let line = stroke(1.4, colour);
    p.circle_stroke(c, 6.5, line);
    for dir in [vec2(1.0, 0.0), vec2(-1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, -1.0)] {
        p.line_segment([c + dir * 9.0, c + dir * 14.0], line);
    }
    p.circle_filled(c, 1.8, colour);
}

// --------------------------------------------------------------------------- the cards ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Icon {
    Dot(Color32),
    UpDown,
    /// Where the wind comes from (degrees); the arrow shows where it goes.
    Wind(f32),
    /// Sunrise (true) or sunset.
    Sun(bool),
    Quake,
    Storm,
    Fire,
    Iss,
    Plane,
    Plus,
    Minus,
    Home,
    Globe,
    /// A crosshair round a dot: where Sir is.
    You,
    /// "Aa" over a dashed border.
    Labels,
    Mountain,
    Radar,
    Cloud,
}

impl Icon {
    /// The icon in a box of `2 s` around `c`: in its own colours, or all in `mono` (a button's).
    fn paint(self, p: &Painter, c: Pos2, s: f32, mono: Option<Color32>) {
        let ink = mono.unwrap_or(STRIP_INK);
        let line = stroke(1.3, ink);
        match self {
            Icon::Dot(colour) => {
                p.circle_filled(c, s * 0.42, mono.unwrap_or(colour));
            }
            Icon::UpDown => {
                p.line_segment([c - vec2(0.0, s), c + vec2(0.0, s)], line);
                for dir in [-1.0, 1.0] {
                    let tip = c + vec2(0.0, s * dir);
                    p.line_segment([tip, tip + vec2(-s * 0.45, -s * 0.45 * dir)], line);
                    p.line_segment([tip, tip + vec2(s * 0.45, -s * 0.45 * dir)], line);
                }
            }
            Icon::Wind(from) => {
                let a = (from + 180.0).to_radians();
                let dir = vec2(a.sin(), -a.cos());
                let side = vec2(-dir.y, dir.x);
                let tip = c + dir * s;
                p.line_segment([c - dir * s, tip], line);
                p.line_segment([tip, tip - dir * s * 0.55 + side * s * 0.45], line);
                p.line_segment([tip, tip - dir * s * 0.55 - side * s * 0.45], line);
            }
            Icon::Sun(rise) => {
                let ground = c.y + s * 0.45;
                p.line_segment([pos2(c.x - s, ground), pos2(c.x + s, ground)], line);
                let arc: Vec<Pos2> = (0..=10).map(|k| PI * k as f32 / 10.0).map(|a| pos2(c.x - s * 0.55 * a.cos(), ground - s * 0.55 * a.sin())).collect();
                p.add(Shape::line(arc, line));
                let (from, to) = if rise { (ground - s * 0.75, c.y - s) } else { (c.y - s, ground - s * 0.75) };
                p.line_segment([pos2(c.x, from), pos2(c.x, to)], line);
            }
            Icon::Quake => {
                p.circle_filled(c, s * 0.45, mono.unwrap_or(QUAKE_LOW));
                p.circle_stroke(c, s * 0.9, stroke(1.0, mono.unwrap_or(crate::hud_board::PIN_RED)));
            }
            Icon::Storm => spiral(p, c, s, mono.unwrap_or(STORM)),
            Icon::Fire => flame(p, c, s * 2.0, mono.unwrap_or(FIRE), if mono.is_some() { None } else { Some(FIRE_CORE) }),
            Icon::Iss => iss_glyph(p, c, s, mono.unwrap_or(WHITE_INK)),
            Icon::Plane => plane(p, c, s * 0.95, 45.0, mono.unwrap_or(WHITE_INK)),
            Icon::Plus => {
                p.line_segment([c - vec2(s * 0.75, 0.0), c + vec2(s * 0.75, 0.0)], line);
                p.line_segment([c - vec2(0.0, s * 0.75), c + vec2(0.0, s * 0.75)], line);
            }
            Icon::Minus => {
                p.line_segment([c - vec2(s * 0.75, 0.0), c + vec2(s * 0.75, 0.0)], line);
            }
            Icon::Home => {
                p.add(Shape::line(vec![c + vec2(-s, -s * 0.05), c + vec2(0.0, -s * 0.9), c + vec2(s, -s * 0.05)], line));
                p.add(Shape::line(vec![c + vec2(-s * 0.65, -s * 0.3), c + vec2(-s * 0.65, s * 0.8), c + vec2(s * 0.65, s * 0.8), c + vec2(s * 0.65, -s * 0.3)], line));
            }
            Icon::Globe => {
                p.circle_stroke(c, s * 0.85, line);
                let meridian: Vec<Pos2> = (0..=16).map(|k| TAU * k as f32 / 16.0).map(|a| c + vec2(s * 0.38 * a.cos(), s * 0.85 * a.sin())).collect();
                p.add(Shape::line(meridian, line));
                p.line_segment([c - vec2(s * 0.85, 0.0), c + vec2(s * 0.85, 0.0)], line);
            }
            Icon::You => {
                p.circle_stroke(c, s * 0.6, line);
                p.circle_filled(c, s * 0.2, ink);
                for dir in [vec2(1.0, 0.0), vec2(-1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, -1.0)] {
                    p.line_segment([c + dir * s * 0.6, c + dir * s], line);
                }
            }
            Icon::Labels => {
                // A border's dashes under a little "A".
                let base = c.y + s * 0.15;
                p.add(Shape::line(vec![pos2(c.x - s * 0.55, base), pos2(c.x, c.y - s * 0.85), pos2(c.x + s * 0.55, base)], line));
                p.line_segment([pos2(c.x - s * 0.3, c.y - s * 0.25), pos2(c.x + s * 0.3, c.y - s * 0.25)], line);
                let y = c.y + s * 0.7;
                for k in 0..3 {
                    let x = c.x - s + k as f32 * s * 0.75;
                    p.line_segment([pos2(x, y), pos2(x + s * 0.45, y)], line);
                }
            }
            Icon::Mountain => {
                let foot = c.y + s * 0.7;
                p.add(Shape::line(
                    vec![pos2(c.x - s, foot), pos2(c.x - s * 0.25, c.y - s * 0.75), pos2(c.x + s * 0.15, c.y - s * 0.1), pos2(c.x + s * 0.45, c.y - s * 0.45), pos2(c.x + s, foot)],
                    line,
                ));
                p.line_segment([pos2(c.x - s * 0.5, c.y - s * 0.3), pos2(c.x - s * 0.05, c.y - s * 0.3)], line);
            }
            Icon::Radar => {
                for k in 1..=3 {
                    let r = s * 0.3 * k as f32;
                    let arc: Vec<Pos2> = (0..=8).map(|j| -PI * 0.25 + PI * 0.5 * j as f32 / 8.0).map(|a| c + vec2(0.0, s * 0.55) + vec2(a.sin(), -a.cos()) * r * 1.2).collect();
                    p.add(Shape::line(arc, line));
                }
                p.circle_filled(c + vec2(0.0, s * 0.55), s * 0.14, ink);
            }
            Icon::Cloud => {
                let base = c.y + s * 0.45;
                p.circle_stroke(c + vec2(-s * 0.4, s * 0.05), s * 0.4, line);
                p.circle_stroke(c + vec2(s * 0.15, -s * 0.15), s * 0.55, line);
                p.line_segment([pos2(c.x - s * 0.8, base), pos2(c.x + s * 0.7, base)], line);
            }
        }
    }
}

/// A piece of a card's row: an icon, a text, or an icon before a text.
#[derive(Clone)]
struct Piece {
    icon: Option<Icon>,
    text: Option<Arc<Galley>>,
}

impl Piece {
    fn text(galley: Arc<Galley>) -> Self {
        Self { icon: None, text: Some(galley) }
    }

    fn with(icon: Icon, galley: Arc<Galley>) -> Self {
        Self { icon: Some(icon), text: Some(galley) }
    }

    fn width(&self) -> f32 {
        let icon = match (self.icon, &self.text) {
            (Some(_), Some(_)) => ICON + 5.0,
            (Some(_), None) => ICON,
            _ => 0.0,
        };
        icon + self.text.as_ref().map_or(0.0, |g| g.size().x)
    }

    fn height(&self) -> f32 {
        let text = self.text.as_ref().map_or(0.0, |g| g.size().y);
        if self.icon.is_some() {
            text.max(ICON)
        } else {
            text
        }
    }
}

/// A box of rows on the map.
struct Card {
    rect: Rect,
    rows: Vec<Vec<Piece>>,
    edge: Option<Color32>,
    brackets: bool,
}

impl Card {
    fn size(rows: &[Vec<Piece>]) -> Vec2 {
        let width = rows.iter().map(|r| r.iter().map(Piece::width).sum::<f32>() + PIECE_GAP * r.len().saturating_sub(1) as f32).fold(0.0, f32::max);
        let height = rows.iter().map(|r| r.iter().map(Piece::height).fold(0.0, f32::max)).sum::<f32>() + ROW_GAP * rows.len().saturating_sub(1) as f32;
        vec2(width + 2.0 * PAD, height + 2.0 * PAD)
    }

    fn new(rows: Vec<Vec<Piece>>, anchor: Pos2, align: Align2, brackets: bool) -> Self {
        let rect = align.anchor_size(anchor, Self::size(&rows));
        Self { rect, rows, edge: None, brackets }
    }

    fn paint(&self, p: &Painter, ink: &Ink) {
        p.rect(self.rect, 0.0, CARD, stroke(1.0, self.edge.unwrap_or(ink.border)));
        if self.brackets {
            crate::hud_board::brackets(p, self.rect, crate::hud_board::bracket_colour());
        }
        let mut y = self.rect.top() + PAD;
        for row in &self.rows {
            let h = row.iter().map(Piece::height).fold(0.0, f32::max);
            let mut x = self.rect.left() + PAD;
            for piece in row {
                // Texts sit on the row's bottom (a big figure and its small unit share a line).
                let text_h = piece.text.as_ref().map_or(h, |g| g.size().y);
                if let Some(icon) = piece.icon {
                    icon.paint(p, pos2(x + ICON / 2.0, y + h - text_h / 2.0), ICON / 2.0, None);
                    x += ICON + if piece.text.is_some() { 5.0 } else { 0.0 };
                }
                if let Some(galley) = &piece.text {
                    p.galley(pos2(x, y + h - galley.size().y), galley.clone(), STRIP_INK);
                    x += galley.size().x;
                }
                x += PIECE_GAP;
            }
            y += h + ROW_GAP;
        }
    }
}

/// The scale bar: a round distance and its length on screen (in the middle of the view).
struct Scale {
    rect: Rect,
    px: f32,
    label: Arc<Galley>,
}

impl Scale {
    fn paint(&self, p: &Painter) {
        p.rect_filled(self.rect, 0.0, Color32::from_black_alpha(120));
        let r = self.rect.shrink2(vec2(6.0, 4.0));
        p.galley(pos2(r.right() - self.label.size().x, r.top()), self.label.clone(), STRIP_INK);
        let (y, x1) = (r.bottom() - 1.0, r.right());
        let x0 = x1 - self.px;
        let line = stroke(1.3, STRIP_INK);
        p.line_segment([pos2(x0, y), pos2(x1, y)], line);
        for x in [x0, x1] {
            p.line_segment([pos2(x, y - 5.0), pos2(x, y)], line);
        }
    }
}

struct Cards {
    list: Vec<Card>,
    scale: Option<Scale>,
}

impl Cards {
    fn paint(&self, p: &Painter, ink: &Ink) {
        if let Some(scale) = &self.scale {
            scale.paint(p);
        }
        for card in &self.list {
            card.paint(p, ink);
        }
    }
}

/// A text cut to `max_width` with an ellipsis (a long place name).
fn elided(p: &Painter, text: &str, font: FontId, colour: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, colour);
    job.wrap = egui::text::TextWrapping { max_width: max_width.max(12.0), max_rows: 1, break_anywhere: true, overflow_character: Some('\u{2026}') };
    p.layout_job(job)
}

/// The cards over the map, each where it fits: the info card top left, the place top right
/// (left of the controls), the imagery's state, the legend bottom left, the scale bottom right.
fn plan_cards(p: &Painter, st: &Earth, geo: &Geo, controls: &Controls, placer: &mut Placer, ink: &Ink) -> Cards {
    let map = geo.map;
    let mut list: Vec<Card> = Vec::new();
    let compact = map.height() < 250.0;
    let offset = st.offset_secs();
    let local = chrono::Utc::now() + chrono::Duration::seconds(offset as i64);
    let text = |s: String, font: FontId, colour: Color32| p.layout_no_wrap(s, font, colour);

    // The time and place in view.
    let mut rows = vec![vec![Piece::text(text(local.format("%H:%M").to_string(), display(22.0), WHITE_INK)), Piece::text(text(model::utc_offset_label(offset), mono(10.0), STRIP_MUTED))]];
    if !compact {
        rows.push(vec![Piece::text(text(local.format("%a %-d %b %Y").to_string().to_uppercase(), mono(10.0), STRIP_MUTED))]);
    }
    rows.push(vec![Piece::with(Icon::Dot(ink.accent), text(model::coords_label(st.cam.lat, st.cam.lon), mono(10.0), STRIP_INK))]);
    rows.push(vec![Piece::with(Icon::UpDown, text(format!("{} km", model::group_digits(st.cam.alt.round() as i64)), mono(10.0), STRIP_INK))]);
    let info = Card::new(rows, map.min + vec2(10.0, 10.0), Align2::LEFT_TOP, true);
    let info_rect = placer.take(info.rect).then_some(info.rect);
    if info_rect.is_some() {
        list.push(info);
    }

    // The place in view: a chip on the globe, a card with its weather on the flat map.
    let flat = geo.flatness >= 0.5;
    let name_width = (map.width() * 0.4).clamp(110.0, 260.0);
    let name = st.place.as_ref().map(|place| place.name(flat)).filter(|n| !n.trim().is_empty());
    let right = pos2(controls.area.left() - 6.0, map.top() + 10.0);
    let place = if flat && (name.is_some() || st.weather.is_some()) {
        let mut rows = vec![vec![Piece::text(elided(p, name.as_deref().unwrap_or("\u{2014}"), super::bold(13.0), WHITE_INK, name_width))]];
        rows.push(vec![Piece::text(text(format!("{} {}", local.format("%H:%M"), model::utc_offset_label(offset)), mono(10.0), STRIP_MUTED))]);
        if let Some(w) = &st.weather {
            rows.push(vec![Piece::text(text(format!("{:.1} \u{b0}C", w.temp), display(18.0), WHITE_INK)), Piece::text(text(format!("\u{2248} {:.1} \u{b0}C", w.feels), mono(10.0), STRIP_MUTED))]);
            let mut air = Vec::new();
            if let Some(h) = w.humidity {
                air.push(Piece::text(text(format!("{h:.0} %"), mono(10.0), STRIP_INK)));
            }
            if let Some(v) = w.wind_kmh {
                air.push(Piece::with(Icon::Wind(w.wind_from.unwrap_or(0.0) as f32), text(format!("{v:.0} km/h"), mono(10.0), STRIP_INK)));
            }
            if !air.is_empty() {
                rows.push(air);
            }
            if let (Some(rise), Some(set)) = (&w.sunrise, &w.sunset) {
                rows.push(vec![Piece::with(Icon::Sun(true), text(rise.clone(), mono(10.0), STRIP_INK)), Piece::with(Icon::Sun(false), text(set.clone(), mono(10.0), STRIP_INK))]);
            }
        }
        Some(Card::new(rows, right, Align2::RIGHT_TOP, true))
    } else {
        name.map(|n| Card::new(vec![vec![Piece::with(Icon::Dot(ink.accent), elided(p, &n.to_uppercase(), mono(10.0), STRIP_INK, name_width))]], right, Align2::RIGHT_TOP, false))
    };
    if let Some(card) = place.filter(|c| placer.take(c.rect)) {
        list.push(card);
    }

    // How the imagery is doing (or "ZOOM IN FOR FLIGHTS", the day or the radar's time shown).
    if let Some(status) = st.status(p.ctx().input(|i| i.time)) {
        let rows = vec![vec![Piece::text(text(status, mono(10.0), ink.accent))]];
        let mut spots = vec![(pos2(map.center().x, map.top() + 10.0), Align2::CENTER_TOP)];
        if let Some(info) = info_rect {
            spots.push((pos2(info.left(), info.bottom() + 6.0), Align2::LEFT_TOP));
        }
        spots.push((pos2(map.center().x, map.bottom() - 10.0), Align2::CENTER_BOTTOM));
        if let Some(card) = spots.into_iter().map(|(at, align)| Card::new(rows.clone(), at, align, false)).find(|c| placer.fits(c.rect)) {
            placer.take(card.rect);
            list.push(card);
        }
    }

    // The scale, then the legend beside it.
    let max_px = (map.width() * 0.16).clamp(60.0, 130.0) as f64;
    let scale = model::nice_scale(geo.proj().px_per_km(), max_px).and_then(|(km, px)| {
        let label = text(model::distance_label(km), mono(10.0), STRIP_INK);
        let size = vec2((px as f32).max(label.size().x) + 12.0, label.size().y + 14.0);
        let rect = Align2::RIGHT_BOTTOM.anchor_size(pos2(controls.area.left() - 6.0, map.bottom() - 10.0), size);
        placer.take(rect).then_some(Scale { rect, px: px as f32, label })
    });
    let legend = legend(p, st);
    if !legend.is_empty() {
        let at = pos2(map.left() + 10.0, map.bottom() - 10.0);
        let half = legend.len().div_ceil(2);
        let tries = [vec![legend.clone()], vec![legend[..half].to_vec(), legend[half..].to_vec()]];
        if let Some(card) = tries.into_iter().map(|rows| Card::new(rows, at, Align2::LEFT_BOTTOM, false)).find(|c| placer.fits(c.rect)) {
            placer.take(card.rect);
            list.push(card);
        }
    }
    Cards { list, scale }
}

/// The world tour's caption at the bottom of the map: the stop, what it is, where.
fn plan_tour_card(p: &Painter, st: &Earth, geo: &Geo, placer: &mut Placer) -> Option<Card> {
    let tour = st.tour.as_ref()?;
    let stop = tour.current()?;
    let map = geo.map;
    let width = (map.width() * 0.5).clamp(140.0, 420.0);
    let head = format!("WORLD TOUR {}/{} \u{b7} {}", tour.index + 1, tour.stops.len(), stop.kind.label());
    let rows = vec![
        vec![Piece::text(p.layout_no_wrap(head, mono(10.0), STRIP_MUTED))],
        vec![Piece::text(elided(p, &stop.title, super::bold(13.0), WHITE_INK, width))],
        vec![Piece::text(elided(p, &stop.caption, mono(10.0), STRIP_INK, width))],
    ];
    let spots = [(pos2(map.center().x, map.bottom() - 10.0), Align2::CENTER_BOTTOM), (pos2(map.center().x, map.bottom() - 60.0), Align2::CENTER_BOTTOM), (pos2(map.center().x, map.top() + 40.0), Align2::CENTER_TOP)];
    let card = spots.into_iter().map(|(at, align)| Card::new(rows.clone(), at, align, true)).find(|c| placer.fits(c.rect))?;
    placer.take(card.rect);
    Some(card)
}

/// The legend's pieces: each layer that is on with its count.
fn legend(p: &Painter, st: &Earth) -> Vec<Piece> {
    let text = |s: String| p.layout_no_wrap(s, mono(10.0), STRIP_INK);
    let mut pieces = Vec::new();
    if st.layers.quakes {
        let mags = st.quakes.items.iter().map(|q| q.mag);
        let (lo, hi) = mags.fold((f64::MAX, f64::MIN), |(lo, hi), m| (lo.min(m), hi.max(m)));
        let n = st.quakes.items.len();
        let label = if n == 0 { "0".to_string() } else { format!("{n} M{lo:.1}\u{2013}{hi:.1}") };
        pieces.push(Piece::with(Icon::Quake, text(label)));
    }
    let count = |kind: EventKind| st.events.items.iter().filter(|e| e.kind == kind).count();
    if st.layers.storms {
        pieces.push(Piece::with(Icon::Storm, text(count(EventKind::Storm).to_string())));
    }
    if st.layers.fires {
        pieces.push(Piece::with(Icon::Fire, text(count(EventKind::Fire).to_string())));
    }
    if st.layers.iss && !st.iss.items.is_empty() {
        pieces.push(Piece::with(Icon::Iss, text("ISS".into())));
    }
    if st.layers.flights {
        pieces.push(Piece::with(Icon::Plane, text(model::group_digits(st.flights.items.len() as i64))));
    }
    // live layers (C1)
    pieces.extend(crate::earth_layers::legend().into_iter().map(|(c, s)| Piece::with(Icon::Dot(c), text(s))));
    pieces
}

/// The card by the thing under the pointer: an aircraft's call sign, type, altitude and speed,
/// an earthquake's magnitude and place, a storm or a fire, the ISS.
fn callout(p: &Painter, hover: &Hover, at: Pos2, map: Rect, ink: &Ink) {
    let width = (map.width() - 40.0).clamp(80.0, 300.0);
    let line = |s: &str, font: FontId, colour: Color32| elided(p, s, font, colour, width);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let rows: Vec<Vec<Piece>> = match hover {
        Hover::Plane(f) => vec![vec![Piece::with(Icon::Plane, line(&f.callsign, super::bold(12.0), WHITE_INK))], vec![Piece::text(line(&f.details(), mono(10.0), STRIP_INK))]],
        Hover::Quake(q) => vec![
            vec![Piece::with(Icon::Quake, line(&format!("M{:.1} \u{b7} {}", q.mag, model::ago((now_ms - q.time_ms) / 1000)), super::bold(12.0), WHITE_INK))],
            vec![Piece::text(line(if q.place.is_empty() { "Unnamed place" } else { &q.place }, mono(10.0), STRIP_INK))],
            vec![Piece::text(line(&format!("depth {:.0} km", q.depth_km), mono(10.0), STRIP_MUTED))],
        ],
        Hover::Event(e) if e.kind == EventKind::Storm => vec![vec![Piece::with(Icon::Storm, line(&e.label, super::bold(12.0), WHITE_INK))], vec![Piece::text(line(&e.title, mono(10.0), STRIP_INK))]],
        Hover::Event(e) => vec![vec![Piece::with(Icon::Fire, line(&e.title, super::bold(12.0), WHITE_INK))], vec![Piece::text(line("Wildfire \u{b7} NASA EONET", mono(10.0), STRIP_MUTED))]],
        Hover::Iss(iss) => vec![
            vec![Piece::with(Icon::Iss, line("ISS", super::bold(12.0), WHITE_INK))],
            vec![Piece::text(line(&format!("{} km \u{b7} {} km/h", iss.alt_km.round(), model::group_digits(iss.speed_kmh.round() as i64)), mono(10.0), STRIP_INK))],
        ],
        // live layers (C1): a satellite, a train or a road incident.
        Hover::Live(hit) => hit.lines().iter().enumerate().map(|(i, s)| vec![if i == 0 { Piece::with(Icon::Dot(ink.accent), line(s, super::bold(12.0), WHITE_INK)) } else { Piece::text(line(s, mono(10.0), STRIP_INK)) }]).collect(),
    };
    let size = Card::size(&rows);
    let mut min = at + vec2(14.0, 14.0);
    if min.x + size.x > map.right() - 4.0 {
        min.x = at.x - 14.0 - size.x;
    }
    if min.y + size.y > map.bottom() - 4.0 {
        min.y = at.y - 14.0 - size.y;
    }
    let min = min.max(map.min + vec2(4.0, 4.0));
    Card { rect: Rect::from_min_size(min, size), rows, edge: Some(ink.accent), brackets: false }.paint(p, ink);
}

// ------------------------------------------------------------------------ the controls ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Control {
    ZoomIn,
    ZoomOut,
    Home,
    Locate,
    Layer(Layer),
    Overlay(Overlay),
    Satellite,
}

const CONTROLS: [Control; 14] = [
    Control::ZoomIn,
    Control::ZoomOut,
    Control::Home,
    Control::Locate,
    Control::Layer(Layer::Quakes),
    Control::Layer(Layer::Storms),
    Control::Layer(Layer::Fires),
    Control::Layer(Layer::Iss),
    Control::Layer(Layer::Flights),
    Control::Overlay(Overlay::Labels),
    Control::Overlay(Overlay::Terrain),
    Control::Overlay(Overlay::Radar),
    Control::Overlay(Overlay::Clouds),
    Control::Satellite,
];
/// Where the column's groups start (the camera, the live layers, the overlays).
const GROUP_STARTS: [usize; 2] = [4, 9];

impl Control {
    fn icon(self) -> Icon {
        match self {
            Control::ZoomIn => Icon::Plus,
            Control::ZoomOut => Icon::Minus,
            Control::Home => Icon::Home,
            Control::Locate => Icon::You,
            Control::Layer(Layer::Quakes) => Icon::Quake,
            Control::Layer(Layer::Storms) => Icon::Storm,
            Control::Layer(Layer::Fires) => Icon::Fire,
            Control::Layer(Layer::Iss) => Icon::Iss,
            Control::Layer(Layer::Flights) => Icon::Plane,
            Control::Layer(Layer::Cameras) => Icon::Globe, // cameras (C2)
            Control::Overlay(Overlay::Labels) => Icon::Labels,
            Control::Overlay(Overlay::Terrain) => Icon::Mountain,
            Control::Overlay(Overlay::Radar) => Icon::Radar,
            Control::Overlay(Overlay::Clouds) => Icon::Cloud,
            Control::Satellite => Icon::Globe,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Control::ZoomIn => "Zoom in (or the mouse wheel)",
            Control::ZoomOut => "Zoom out",
            Control::Home => "Back to the whole globe",
            Control::Locate => "Where am I? (the Windows location service, else your connection's city)",
            Control::Layer(layer) => layer.hint(),
            Control::Overlay(Overlay::Labels) => "Place names and borders (Esri)",
            Control::Overlay(Overlay::Terrain) => "Shaded terrain and the peaks' names (AWS Terrain Tiles, OpenStreetMap)",
            Control::Overlay(Overlay::Radar) => "Rain radar, the past two hours (RainViewer)",
            Control::Overlay(Overlay::Clouds) => "Live cloud cover: infrared from GOES and Himawari (NASA GIBS)",
            Control::Satellite => "The photo globe; again for the holographic one",
        }
    }
}

/// The round buttons down the map's right edge (in two or three columns on a short map).
struct Controls {
    area: Rect,
    buttons: Vec<(Control, Rect)>,
}

fn controls_layout(map: Rect) -> Controls {
    const GAP: f32 = 5.0;
    const GROUP: f32 = 8.0;
    let avail = (map.height() - 20.0).max(30.0);
    let n = CONTROLS.len();
    let size_for = |cols: usize| {
        let rows = n.div_ceil(cols);
        let groups = if cols == 1 { 2.0 * GROUP } else { 0.0 };
        ((avail - (rows - 1) as f32 * GAP - groups) / rows as f32).min(28.0)
    };
    let cols = (1..=3).find(|cols| size_for(*cols) >= 17.0).unwrap_or(3);
    let (rows, size) = (n.div_ceil(cols), size_for(cols).max(10.0));
    let buttons: Vec<(Control, Rect)> = CONTROLS
        .into_iter()
        .enumerate()
        .map(|(i, control)| {
            let (col, row) = (i / rows, i % rows);
            let groups = if cols == 1 { GROUP * (u8::from(i >= GROUP_STARTS[0]) + u8::from(i >= GROUP_STARTS[1])) as f32 } else { 0.0 };
            let x = map.right() - 10.0 - size - (cols - 1 - col) as f32 * (size + GAP);
            let y = map.top() + 10.0 + row as f32 * (size + GAP) + groups;
            (control, Rect::from_min_size(pos2(x, y), vec2(size, size)))
        })
        .collect();
    let area = buttons.iter().fold(Rect::NOTHING, |a, (_, r)| a.union(*r)).expand(4.0);
    Controls { area, buttons }
}

/// The control column: filled in the accent while a layer (or the satellite view) is on.
fn paint_controls(ui: &mut Ui, p: &Painter, st: &mut Earth, controls: &Controls, ink: &Ink, rt: &Handle) {
    for &(control, rect) in &controls.buttons {
        let resp = ui.interact(rect, ui.id().with(("earth_control", control)), Sense::click());
        let on = match control {
            Control::Layer(layer) => st.layers.get(layer),
            Control::Overlay(overlay) => st.overlays.get(overlay),
            Control::Locate => st.me.is_some(),
            Control::Satellite => st.look == Look::Satellite,
            _ => false,
        };
        let (c, r) = (rect.center(), rect.width() / 2.0);
        let icon = if on {
            p.circle_filled(c, r, ink.accent);
            SPACE
        } else {
            p.circle(c, r, CARD, stroke(1.0, if resp.hovered() { ink.accent } else { ink.border }));
            if matches!(control, Control::Layer(_) | Control::Overlay(_) | Control::Satellite) { ink.accent } else { STRIP_INK }
        };
        control.icon().paint(p, c, r * 0.5, Some(icon));
        if resp.on_hover_text(control.hint()).clicked() {
            match control {
                Control::ZoomIn => st.zoom_by(1.0 / model::ZOOM_STEP),
                Control::ZoomOut => st.zoom_by(model::ZOOM_STEP),
                Control::Home => {
                    st.area = None;
                    st.go_home();
                }
                Control::Locate => model::locate_in_background(rt),
                Control::Layer(layer) => st.layers.set(layer, !on),
                Control::Overlay(overlay) => {
                    st.overlays.set(overlay, !on);
                    if overlay == Overlay::Radar {
                        st.radar_since = f64::NAN;
                    }
                }
                Control::Satellite => st.set_look(if on { Look::Holo } else { Look::Satellite }),
            }
        }
    }
}

// -------------------------------------------------------------------------- the drawer ----

/// The results of an area query under the map: its header ("0 - 500 KM - 7 D", when it was
/// asked, a close key) and a row per earthquake, strongest first (a click flies there).
fn drawer(ui: &mut Ui, rect: Rect, st: &mut Earth, ink: &Ink) {
    let Some(area) = st.area.clone() else { return };
    let p = ui.painter_at(rect);
    p.rect(rect, 0.0, CARD, stroke(1.0, ink.border));
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), 24.0));
    let mid = header.center().y;
    p.circle_filled(pos2(header.left() + 12.0, mid), 3.5, ink.accent);
    let title = p.layout_no_wrap(area.header(), mono(10.0), ink.accent);
    let title_right = header.left() + 22.0 + title.size().x;
    p.galley(pos2(header.left() + 22.0, mid - title.size().y / 2.0), title, ink.accent);
    let close = Rect::from_center_size(pos2(header.right() - 14.0, mid), vec2(18.0, 18.0));
    let close_resp = ui.interact(close, ui.id().with("earth_area_close"), Sense::click());
    let x_ink = stroke(1.4, if close_resp.hovered() { ink.accent } else { STRIP_INK });
    let k = 4.5;
    p.line_segment([close.center() + vec2(-k, -k), close.center() + vec2(k, k)], x_ink);
    p.line_segment([close.center() + vec2(-k, k), close.center() + vec2(k, -k)], x_ink);
    let count = area.results.as_ref().map(|list| format!("{} FOUND  ", list.len())).unwrap_or_default();
    let asked = p.layout_no_wrap(format!("{count}{}", area.asked), mono(10.0), STRIP_MUTED);
    let asked_x = close.left() - 8.0 - asked.size().x;
    if asked_x > title_right + 12.0 {
        p.galley(pos2(asked_x, mid - asked.size().y / 2.0), asked, STRIP_MUTED);
    }
    p.line_segment([pos2(rect.left(), header.bottom()), pos2(rect.right(), header.bottom())], stroke(1.0, ink.border));
    let body = Rect::from_min_max(pos2(rect.left() + 8.0, header.bottom() + 4.0), pos2(rect.right() - 8.0, rect.bottom() - 4.0));
    let note = |text: &str, colour: Color32| {
        let galley = elided(&p, text, mono(10.0), colour, body.width());
        p.galley(pos2(body.center().x - galley.size().x / 2.0, body.top() + 6.0), galley, colour);
    };
    match (&area.results, &area.failed) {
        (_, Some(e)) => note(&format!("USGS: {e}"), STRIP_MUTED),
        (None, None) => note("QUERYING USGS..", STRIP_MUTED),
        (Some(list), _) if list.is_empty() => note("\u{2014}", STRIP_INK),
        (Some(list), _) => {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut child = ui.new_child(UiBuilder::new().max_rect(body));
            egui::ScrollArea::vertical().id_salt("earth_area_rows").auto_shrink([false; 2]).show(&mut child, |ui| {
                let width = ui.available_width();
                for q in list {
                    let (row, resp) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
                    let p = ui.painter();
                    if resp.hovered() {
                        p.rect_filled(row, 0.0, ink.accent.gamma_multiply(0.12));
                    }
                    let mid = row.center().y;
                    p.circle_filled(pos2(row.left() + 6.0, mid), 4.0, quake_colour(q.mag));
                    let mag = p.layout_no_wrap(format!("{:.1}", q.mag), mono(10.0), WHITE_INK);
                    p.galley(pos2(row.left() + 16.0, mid - mag.size().y / 2.0), mag, WHITE_INK);
                    let ago = p.layout_no_wrap(model::ago((now_ms - q.time_ms) / 1000), mono(10.0), STRIP_MUTED);
                    let ago_x = row.right() - ago.size().x - 4.0;
                    p.galley(pos2(ago_x, mid - ago.size().y / 2.0), ago, STRIP_MUTED);
                    let place = elided(p, if q.place.is_empty() { "Unnamed place" } else { &q.place }, mono(10.0), STRIP_INK, ago_x - 10.0 - (row.left() + 50.0));
                    p.galley(pos2(row.left() + 50.0, mid - place.size().y / 2.0), place, STRIP_INK);
                    if resp.on_hover_text("Fly there").clicked() {
                        let alt = st.target().alt;
                        st.fly_to(Camera { lat: q.lat, lon: q.lon, alt });
                    }
                }
            });
        }
    }
    if close_resp.on_hover_text("Close the results").clicked() {
        st.area = None;
    }
}

// ----------------------------------------------------------------------------- the block ----

/// What the block shows, for the board's search and "copy".
fn summary(st: &Earth) -> String {
    let round = |v: f64| (v * 10.0).round() / 10.0;
    let mut lines = vec![format!("Earth: {} from {} km", model::coords_label(round(st.cam.lat), round(st.cam.lon)), model::group_digits((st.cam.alt / 100.0).round() as i64 * 100))];
    if let Some(place) = &st.place {
        lines.push(place.name(false));
    }
    if st.layers.quakes {
        lines.push(format!("{} earthquakes of magnitude 4.5 and more in the past week", st.quakes.items.len()));
    }
    if st.layers.storms {
        let storms: Vec<&str> = st.events.items.iter().filter(|e| e.kind == EventKind::Storm).map(|e| e.label.as_str()).collect();
        lines.push(format!("storms: {}", if storms.is_empty() { "none".to_string() } else { storms.join(", ") }));
    }
    if st.layers.fires {
        lines.push(format!("{} wildfires", st.events.items.iter().filter(|e| e.kind == EventKind::Fire).count()));
    }
    if let Some(area) = &st.area {
        lines.push(format!("area query {} around {}: {}", area.header(), model::coords_label(area.lat, area.lon), area.results.as_ref().map_or("waiting".to_string(), |r| format!("{} found", r.len()))));
    }
    // What the imagery shows: its source or day, the overlays, the tour.
    let o = st.overlays;
    let overlays: Vec<&str> = [
        (o.labels, "labels and borders"),
        (o.terrain && !o.exaggerate, "terrain"),
        (o.terrain && o.exaggerate, "terrain (exaggerated)"),
        (o.radar && !o.animate, "rain radar"),
        (o.radar && o.animate, "rain radar (past two hours)"),
        (o.clouds, "clouds"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    let imagery = match (st.look, st.historic) {
        (Look::Holo, _) => "holographic".to_string(),
        (_, Some(day)) => format!("NASA imagery of {}", day.format("%Y-%m-%d")),
        _ => st.imagery.credit().trim_start_matches("Imagery: ").to_string(),
    };
    lines.push(format!("imagery: {imagery}{}", if overlays.is_empty() { String::new() } else { format!("; overlays: {}", overlays.join(", ")) }));
    if st.me.is_some() {
        lines.push("Sir's position is marked (YOU)".into());
    }
    if let Some(tour) = &st.tour {
        if let Some(stop) = tour.current() {
            lines.push(format!("world tour, stop {} of {}: {}", tour.index + 1, tour.stops.len(), stop.title));
        }
    }
    lines.join("\n")
}

fn body(ui: &mut Ui, rt: &Handle) {
    // (Closed while it was its own window: its exit shows the Earth as it was; nothing is
    // fetched or told.)
    if !crate::earth::is_open() {
        view_with(ui, &mut crate::earth::state(), rt, false);
        return;
    }
    LET_GO.store(false, Ordering::Release);
    crate::earth::set_waker(ui.ctx());
    let changed = {
        let mut st = crate::earth::state();
        view(ui, &mut st, rt);
        let summary = summary(&st);
        (st.described != summary).then(|| {
            st.described = summary.clone();
            summary
        })
    };
    // (Told with the Earth let go: the dock has locks of its own.)
    if let Some(summary) = changed {
        crate::hud_dock::describe_block(ID, &summary);
    }
}

/// Registers the Earth while it is open (a block on the board, or its own window once detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    // (Closed while it was its own window: it plays its exit first, then its textures go.)
    if !super::detached_keep(ctx, ViewportId::from_hash_of("jai_earth"), crate::earth::is_open()) {
        crate::hud_dock::forget(ID);
        if !LET_GO.swap(true, Ordering::AcqRel) {
            crate::earth::let_go();
        }
        // (The globe's GPU textures go in a paint callback: the GL context is the painter's.)
        if gl::release_pending() {
            ctx.layer_painter(egui::LayerId::background()).add(Shape::Callback(gl::janitor(pos2(0.0, 0.0))));
            ctx.request_repaint();
        }
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, [1180.0, 720.0], |ui| body(ui, rt));
        if action.close {
            crate::earth::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Earth — J.A.I.").with_app_id("jai").with_inner_size([1200.0, 740.0]).with_min_inner_size([700.0, 460.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jai_earth"), builder, move |ctx, class| {
        crate::crash_guard::guard("earth", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!crate::earth::is_open() && !super::detached_leaving(ctx)) {
                crate::earth::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        crate::earth::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};
    use eframe::egui::PointerButton;

    /// The panel's body filling a stage of `size` (the block's content), `frames` frames.
    fn stage(size: Vec2) -> Stage {
        Stage::new(size)
    }

    fn draw(stage: &mut Stage, earth: &mut Earth, rt: &Handle, frames: usize) -> visual::Frame {
        stage.frames(frames, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| view(ui, earth, rt));
        })
    }

    #[test]
    fn street_level_costs_no_more_than_space() {
        // At 250 m the ISS footprint was millions of pixels of dashes: 710 ms a frame (2026-11-09).
        let rt = runtime();
        for alt in [12_742.0, 20.0, 0.25] {
            let mut earth = Earth::demo();
            earth.cam = crate::earth::Camera { lat: 40.71, lon: -74.0, alt };
            let mut stage = stage(vec2(1180.0, 720.0));
            let _ = draw(&mut stage, &mut earth, rt.handle(), 3);
            let t = std::time::Instant::now();
            let _ = draw(&mut stage, &mut earth, rt.handle(), 10);
            assert!(t.elapsed() / 10 < std::time::Duration::from_millis(40), "{alt} km: {:?} a frame", t.elapsed() / 10);
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().expect("a runtime")
    }

    fn clean(frame: &visual::Frame) {
        assert_eq!(visual::clipped_text(frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(frame), Vec::<String>::new());
        assert_eq!(visual::pictographs(frame), Vec::<String>::new());
    }

    #[test]
    fn the_globe_with_its_cards_layers_and_controls() {
        let rt = runtime();
        let mut earth = Earth::demo();
        let mut stage = stage(vec2(1180.0, 720.0));
        let frame = draw(&mut stage, &mut earth, rt.handle(), 4);
        clean(&frame);
        for shown in ["UTC", "28.0000, -78.0000", "12 742 km", "UNITED STATES", "DEMO DATA", "M4.5\u{2013}5.2", "ODALYS-26", "POLO-26", "FAY-26", "5.2", "ISS", "Globe: NASA GIBS"] {
            assert!(!frame.find_text(shown).is_empty(), "{shown} is not on the Earth:\n{}", frame.all_text());
        }
        // The globe is drawn: its middle is lit, the corner beyond it is space.
        let canvas = stage.raster(&frame);
        let middle = canvas.brightness(Rect::from_center_size(pos2(590.0, 330.0), vec2(80.0, 80.0)));
        let corner = canvas.brightness(Rect::from_min_size(pos2(4.0, 300.0), vec2(30.0, 30.0)));
        assert!(middle > corner + 20.0, "globe {middle} vs space {corner}");
        assert!(frame.mesh_vertices > 600, "the rim, the aurora and the rings are meshes");
    }

    #[test]
    fn the_area_drawer_under_the_flat_map_on_a_small_block() {
        let rt = runtime();
        let mut earth = Earth::demo();
        earth.begin_area((40.7128, -74.006), model::DEFAULT_RADIUS_KM);
        earth.cam = earth.target();
        earth.motion = None;
        earth.area.as_mut().expect("the query").results = Some(Vec::new());
        assert!(earth.cam.alt < model::FLAT_FULL_KM, "the area is shown on the flat map");
        let mut stage = stage(vec2(700.0, 460.0));
        let frame = draw(&mut stage, &mut earth, rt.handle(), 4);
        clean(&frame);
        for shown in ["0 - 500 KM - 7 D", "\u{2014}", "UTC", "New York", "18.1 \u{b0}C", "06:58", "19:10", "km"] {
            assert!(!frame.find_text(shown).is_empty(), "{shown} is not on the Earth:\n{}", frame.all_text());
        }
        // The drawer's close key closes the query.
        let head = frame.find_text("0 - 500 KM - 7 D")[0];
        let frame = stage.click(pos2(700.0 - 14.0, head.center().y), PointerButton::Primary, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| view(ui, &mut earth, rt.handle()));
        });
        assert!(earth.area.is_none(), "the drawer stays open");
        assert!(frame.find_text("0 - 500 KM - 7 D").is_empty());
        clean(&frame);
    }

    #[test]
    fn a_drag_turns_the_globe_and_the_buttons_switch_layers() {
        let rt = runtime();
        let mut earth = Earth::demo();
        let mut stage = stage(vec2(1180.0, 720.0));
        draw(&mut stage, &mut earth, rt.handle(), 2);
        let before = earth.cam;
        stage.drag(pos2(560.0, 330.0), vec2(160.0, 0.0), 8, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| view(ui, &mut earth, rt.handle()));
        });
        assert!(earth.cam.lon < before.lon - 10.0, "dragged east, the view turns west: {} -> {}", before.lon, earth.cam.lon);
        assert!((earth.cam.lat - before.lat).abs() < 1.0);
        // The flights key switches the flights on.
        let column = controls_layout(Rect::from_min_size(pos2(0.0, 0.0), vec2(1180.0, 640.0)));
        let (_, flights) = column.buttons.iter().find(|(c, _)| *c == Control::Layer(Layer::Flights)).copied().expect("a flights key");
        assert!(!earth.layers.flights);
        let frame = stage.click(flights.center(), PointerButton::Primary, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| view(ui, &mut earth, rt.handle()));
        });
        assert!(earth.layers.flights, "the flights key did nothing");
        assert!(!frame.find_text("Flights: OpenSky Network").is_empty());
        // The photo globe is the default: its key switches to the hologram.
        let (_, satellite) = column.buttons.iter().find(|(c, _)| *c == Control::Satellite).copied().expect("a satellite key");
        assert_eq!(earth.look, Look::Satellite);
        stage.click(satellite.center(), PointerButton::Primary, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| view(ui, &mut earth, rt.handle()));
        });
        assert_eq!(earth.look, Look::Holo);
        // The radar key switches the rain radar on (its credit shows).
        let (_, radar) = column.buttons.iter().find(|(c, _)| *c == Control::Overlay(Overlay::Radar)).copied().expect("a radar key");
        let frame = stage.click(radar.center(), PointerButton::Primary, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| view(ui, &mut earth, rt.handle()));
        });
        assert!(earth.overlays.radar, "the radar key did nothing");
        assert!(!frame.find_text("Radar: RainViewer").is_empty());
        clean(&frame);
    }

    /// The theme the test found, back however it ends.
    struct ThemeBack(String);

    impl Drop for ThemeBack {
        fn drop(&mut self) {
            crate::theme_engine::follow(&self.0);
            crate::theme_engine::finish_fade();
        }
    }

    /// The verifier's colour check on the globe (every layer on) and on the flat map (an area,
    /// its results and the flights) in the theme on screen.
    fn off_theme(label: &str, rt: &Handle) -> Vec<String> {
        let palette = crate::theme_engine::current();
        let mut earth = Earth::demo();
        earth.layers.set(Layer::Flights, true);
        let mut stage = stage(vec2(1000.0, 640.0));
        let mut findings = visual::off_theme(&draw(&mut stage, &mut earth, rt, 2), label, &palette);
        earth.begin_area((40.7128, -74.006), model::DEFAULT_RADIUS_KM);
        earth.cam = earth.target();
        earth.motion = None;
        earth.layers.set(Layer::Flights, true);
        // (Every quake of the demo as the area's, for rows in the drawer.)
        let found = earth.local_area((40.7128, -74.006), 20_000.0);
        earth.area.as_mut().expect("the query").results = Some(found);
        findings.extend(visual::off_theme(&draw(&mut stage, &mut earth, rt, 2), label, &palette));
        findings
    }

    #[test]
    fn every_outline_and_mesh_keeps_to_the_theme() {
        let _theme = crate::theme_engine::test_lock();
        let _back = ThemeBack(crate::theme_engine::active_name());
        let rt = runtime();
        let mut findings = Vec::new();
        for (name, label) in visual::themes() {
            crate::theme_engine::follow(&name);
            crate::theme_engine::finish_fade();
            findings.extend(off_theme(&label, rt.handle()));
        }
        assert_eq!(findings, Vec::<String>::new());
    }

    #[test]
    fn layouts_keep_the_map_the_drawer_and_the_controls_apart() {
        let full = Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 460.0));
        let lay = Layout::new(full, true, 24.0);
        let drawer = lay.drawer.expect("a drawer");
        assert!(lay.map.bottom() < drawer.top() && drawer.bottom() < lay.credits.top());
        assert!(lay.map.height() >= 250.0, "{:?}", lay.map);
        assert_eq!(Layout::new(full, false, 24.0).drawer, None);
        // A short map gets two columns of keys, each inside the map.
        for map in [lay.map, Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 150.0)), Rect::from_min_size(pos2(0.0, 0.0), vec2(1180.0, 690.0))] {
            let c = controls_layout(map);
            assert_eq!(c.buttons.len(), CONTROLS.len());
            for (i, (_, a)) in c.buttons.iter().enumerate() {
                assert!(map.contains_rect(*a), "{a:?} outside {map:?}");
                for (_, b) in &c.buttons[i + 1..] {
                    assert!(!a.intersects(*b), "{a:?} on {b:?}");
                }
            }
        }
        assert!(controls_layout(Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 150.0))).area.width() > 50.0, "two columns");
    }

    #[test]
    fn lines_break_behind_the_globe_and_across_the_flat_map_edge() {
        let g = GlobeView::new(0.0, 0.0, model::HOME_ALT_KM, 700.0, (400.0, 300.0));
        let equator: Vec<(f64, f64)> = (0..=72).map(|k| (0.0, -180.0 + 5.0 * k as f64)).collect();
        let runs = Proj::Globe(g).runs(&equator);
        assert_eq!(runs.len(), 1, "only the front half shows");
        let flat = FlatView::new(0.0, 179.0, 1_000.0, 700.0, (400.0, 300.0));
        let line = [(0.0, 170.0), (0.0, 175.0), (0.0, 179.5), (0.0, -179.5), (0.0, -175.0)];
        let runs = Proj::Flat(flat).runs(&line);
        assert_eq!(runs.len(), 1, "the date line is not an edge of the flat map around it");
        assert!(runs[0].windows(2).all(|w| w[1].x > w[0].x));
        assert_eq!(densify(&[(0.0, 0.0), (0.0, 10.0)], 5).len(), 6);
    }
}
