//! THE EARTH ("show me the earth", "switch to satellite view", "earthquakes in this area"): a
//! globe on the board with live earthquakes, storms, wildfires, the ISS and flights.
//!
//! * `gui_earth` draws it (no web view): on the GPU (`earth_gl`, a ray-cast sphere lit by the real
//!   sun, night lights, an atmosphere in the theme's tint) when the driver allows, as egui meshes
//!   otherwise. The photo look is the default; the holographic one is drawn from the same data.
//!   Below ~1 600 km the camera looks at a flat Web Mercator map of tiles, to street level.
//! * Imagery: Esri World Imagery (default), Sentinel-2 cloudless or NASA Blue Marble; a day of
//!   NASA's daily true colour by date ("the clouds on 1 March 2024"). Overlays: labels and
//!   borders, shaded terrain with the peaks' names, the rain radar (animated on request) and
//!   infrared cloud tops (`earth_sources` has the URLs and the pixel work).
//! * "Where am I?" (`earth_locate`) puts a "YOU" marker on Sir's position; "what's happening in
//!   the world?" (`earth_tour`) flies from event to event.
//! * Live layers: USGS earthquakes (M4.5+ of the past week; area queries through the FDSN
//!   service), NASA EONET storms and wildfires, the ISS (wheretheiss.at) with its footprint and
//!   ground track, OpenSky flights over the map in view, and the place in view (BigDataCloud)
//!   with its weather and local time (Open-Meteo).
//! * The network runs in the background with ~8 s timeouts; results land in `INBOX` and the HUD
//!   picks them up on its next frame. Imagery and tiles are cached under `config/earth_cache/`
//!   (bounded: the oldest files go first).
//! * Verifier and dry runs (`live_self_tester`) and the unit tests never touch the network: the
//!   Earth opens with a deterministic demo (a procedural globe, a few quakes and storms, the ISS)
//!   and area queries are answered from it.
//!
//! The maths (projections, great circles, Web Mercator tiles), the parsers and the spoken
//! answers are pure and unit-tested at the end of this file.

use crate::earth_gl::{self as gl, Atlas, Clip, ClipUse, GpuImage};
use crate::earth_sources::{self as src, Imagery, Peak, RadarFrame};
use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use eframe::egui::{self, Color32, ColorImage, TextureFilter, TextureHandle, TextureId, TextureOptions, TextureWrapMode};
use serde_json::Value;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tracing::{debug, info, warn};

// ------------------------------------------------------------------------- constants ----

/// Mean Earth radius.
pub const R_KM: f64 = 6371.0;
/// The resting view: the whole planet, the camera one diameter above the surface.
pub const HOME_ALT_KM: f64 = 12_742.0;
/// Street level (Esri's level 19 needs ~250 m).
pub const MIN_ALT_KM: f64 = 0.25;
pub const MAX_ALT_KM: f64 = 60_000.0;
/// The flat map starts to show below `FLAT_FROM_KM` and is all there is below `FLAT_FULL_KM`.
pub const FLAT_FROM_KM: f64 = 1_750.0;
pub const FLAT_FULL_KM: f64 = 1_450.0;
/// The open's camera fly-in: from 2.4x the distance, smootherstep (the reference: ~450 ms).
pub const INTRO_SECS: f64 = 0.45;
pub const INTRO_DISTANCE: f64 = 2.4;
/// Satellite <-> holographic texture switch (the reference: 33-67 ms).
pub const LOOK_FADE_SECS: f64 = 0.05;
/// Fresh imagery fading in over the procedural globe.
pub const ARRIVAL_FADE_SECS: f64 = 0.25;
/// Mouse-wheel zoom: time constant of the smoothing (a notch is about done after 0.4 s).
pub const ZOOM_TAU_SECS: f64 = 0.12;
/// One wheel notch halves or doubles the altitude.
pub const ZOOM_STEP: f64 = 2.0;
pub const SPIN_DEG_PER_SEC: f64 = 6.0;
/// The globe's radius at the home view, as a share of the map's shorter side.
pub const HOME_RADIUS_SHARE: f64 = 0.42;
pub const DEFAULT_RADIUS_KM: f64 = 500.0;
pub const AREA_DAYS: u32 = 7;
/// Flights only below this altitude (the box in view stays small enough for OpenSky).
pub const FLIGHTS_MAX_ALT_KM: f64 = 2_600.0;
/// The widest box (degrees either way) the flights are asked for.
pub const FLIGHTS_MAX_SPAN_DEG: f64 = 40.0;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// The background retry of an area query that took longer than Sir's answer could wait.
const SLOW_TIMEOUT: Duration = Duration::from_secs(25);
const USER_AGENT: &str = "JARVIS/3 desktop assistant";
const CACHE_DIR: &str = "config/earth_cache";
const CACHE_LIMIT: u64 = 1024 * 1024 * 1024;
const CACHE_TRIM_TO: u64 = 850 * 1024 * 1024;
/// Decoded tiles kept in memory (256 KB each): the flat map's next uploads and the globe's
/// clipmaps are made from them.
const PIXEL_CACHE: usize = 192;
/// The radar's frame list is asked for again after this.
const RADAR_EVERY: Duration = Duration::from_secs(300);
/// The radar loop: each past frame this long, the latest a while longer.
pub const RADAR_STEP_SECS: f64 = 0.45;
pub const RADAR_HOLD_SECS: f64 = 1.6;
/// Infrared cloud tiles are fetched again every half hour.
const CLOUD_SLOT_SECS: i64 = 1800;
/// Peaks: only up close, at most one Overpass query this often.
pub const PEAKS_MAX_ALT_KM: f64 = 1_200.0;
const PEAKS_GAP: Duration = Duration::from_secs(20);
/// Place and weather lookups stay this far from Sir's fix.
const PRIVATE_KM: f64 = 60.0;
/// The equirectangular globe textures.
pub const BASE_W: usize = 2048;
pub const BASE_H: usize = 1024;
/// Tile textures kept on the GPU (least recently drawn go first).
pub const TILE_TEXTURES: usize = 200;
const TILES_IN_FLIGHT: usize = 6;
const UPLOADS_PER_FRAME: usize = 8;
const QUAKES_EVERY: Duration = Duration::from_secs(300);
const EVENTS_EVERY: Duration = Duration::from_secs(900);
const ISS_EVERY: Duration = Duration::from_secs(5);
const TRACK_EVERY: Duration = Duration::from_secs(300);
const FLIGHTS_EVERY: Duration = Duration::from_secs(30);
const FLIGHTS_MIN: Duration = Duration::from_secs(15);
const PLACE_GAP: Duration = Duration::from_millis(1200);
const WEATHER_GAP: Duration = Duration::from_millis(2000);
const WEATHER_TTL: Duration = Duration::from_secs(900);
/// The camera rests this long before the place in view is looked up.
pub const REST_SECS: f64 = 0.5;

// ------------------------------------------------------------------------------ maths ----

/// 6t^5 - 15t^4 + 10t^3 on 0..1 (zero first and second derivative at both ends).
pub fn smootherstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// 0 at `edge0`, 1 at `edge1` (either order), smooth in between.
pub fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    if (edge1 - edge0).abs() < f64::EPSILON {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Exponential approach of `current` to `target` (time constant `tau`) after `dt` seconds.
pub fn smooth_towards(current: f64, target: f64, dt: f64, tau: f64) -> f64 {
    if tau <= 0.0 {
        return target;
    }
    target + (current - target) * (-dt.max(0.0) / tau).exp()
}

pub fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

/// lat/lon (degrees) as a unit vector: x towards 0°N 0°E, y towards 0°N 90°E, z north.
pub fn unit(lat: f64, lon: f64) -> [f64; 3] {
    let (la, lo) = (lat.to_radians(), lon.to_radians());
    [la.cos() * lo.cos(), la.cos() * lo.sin(), la.sin()]
}

pub fn lat_lon(v: [f64; 3]) -> (f64, f64) {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    ((v[2] / n).clamp(-1.0, 1.0).asin().to_degrees(), v[1].atan2(v[0]).to_degrees())
}

/// The angle between two points seen from the Earth's centre (radians).
pub fn central_angle(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (p1, p2) = (a.0.to_radians(), b.0.to_radians());
    let (dp, dl) = (p2 - p1, (b.1 - a.1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * h.sqrt().min(1.0).asin()
}

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    R_KM * central_angle(a, b)
}

/// The point a fraction `t` of the way from `a` to `b` on the great circle through both.
pub fn great_circle(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let (va, mut vb) = (unit(a.0, a.1), unit(b.0, b.1));
    let mut dot = va[0] * vb[0] + va[1] * vb[1] + va[2] * vb[2];
    if dot < -0.999_999 {
        // Antipodes: every great circle joins them; take the one over the nearer pole.
        vb = unit(b.0 + if b.0 >= 0.0 { -0.01 } else { 0.01 }, b.1 + 0.01);
        dot = va[0] * vb[0] + va[1] * vb[1] + va[2] * vb[2];
    }
    let omega = dot.clamp(-1.0, 1.0).acos();
    if omega < 1e-9 {
        return a;
    }
    let s = omega.sin();
    let (wa, wb) = (((1.0 - t) * omega).sin() / s, (t * omega).sin() / s);
    lat_lon([wa * va[0] + wb * vb[0], wa * va[1] + wb * vb[1], wa * va[2] + wb * vb[2]])
}

/// Where `dist_km` on the initial bearing `bearing_deg` (clockwise from north) leads.
pub fn destination(from: (f64, f64), bearing_deg: f64, dist_km: f64) -> (f64, f64) {
    let (p1, l1) = (from.0.to_radians(), from.1.to_radians());
    let (d, th) = (dist_km / R_KM, bearing_deg.to_radians());
    let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * th.cos()).clamp(-1.0, 1.0).asin();
    let l2 = l1 + (th.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
    (p2.to_degrees(), wrap_lon(l2.to_degrees()))
}

/// A closed ring of points `radius_km` around `centre` (the first point repeated at the end).
pub fn circle_points(centre: (f64, f64), radius_km: f64, steps: usize) -> Vec<(f64, f64)> {
    let steps = steps.max(3);
    (0..=steps).map(|i| destination(centre, 360.0 * i as f64 / steps as f64, radius_km)).collect()
}

/// How far a satellite at `alt_km` sees: the radius on the ground of its footprint (to its horizon).
pub fn footprint_radius_km(alt_km: f64) -> f64 {
    R_KM * (R_KM / (R_KM + alt_km.max(0.0))).acos()
}

/// The camera distance from the Earth's centre in Earth radii.
pub fn distance_of(alt_km: f64) -> f64 {
    1.0 + alt_km.max(1.0) / R_KM
}

/// The focal length (px) that shows the globe `HOME_RADIUS_SHARE` of `min_side` large at home.
pub fn focal_for(min_side: f64) -> f64 {
    let d = distance_of(HOME_ALT_KM);
    HOME_RADIUS_SHARE * min_side.max(1.0) * (d * d - 1.0).sqrt()
}

/// The altitude that shows a circle of `radius_km` around the middle of the view whole, its
/// radius ~85 % of half the map's shorter side (on any map: the focal length grows with it).
pub fn area_altitude(radius_km: f64) -> f64 {
    (radius_km * 2.8).clamp(400.0, 14_000.0)
}

/// A perspective camera above the globe: the target (`lat0`, `lon0`) in the middle of the
/// screen, north up, the eye `d` Earth radii from the centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobeView {
    pub lat0: f64,
    pub lon0: f64,
    pub d: f64,
    /// Focal length in px.
    pub f: f64,
    pub cx: f64,
    pub cy: f64,
}

impl GlobeView {
    pub fn new(lat0: f64, lon0: f64, alt_km: f64, f: f64, centre: (f64, f64)) -> Self {
        Self { lat0, lon0, d: distance_of(alt_km), f, cx: centre.0, cy: centre.1 }
    }

    /// The cosine of the angle from the view centre beyond which the globe is hidden.
    pub fn horizon(&self) -> f64 {
        1.0 / self.d
    }

    /// The point in view space: x right, y up, z towards the eye (the unit sphere).
    pub fn view_coords(&self, lat: f64, lon: f64) -> [f64; 3] {
        let (la, la0) = (lat.to_radians(), self.lat0.to_radians());
        let dl = (lon - self.lon0).to_radians();
        let (sl, cl, s0, c0) = (la.sin(), la.cos(), la0.sin(), la0.cos());
        [cl * dl.sin(), sl * c0 - cl * s0 * dl.cos(), sl * s0 + cl * c0 * dl.cos()]
    }

    /// A view-space direction back to lat/lon.
    pub fn from_view(&self, v: [f64; 3]) -> (f64, f64) {
        let la0 = self.lat0.to_radians();
        let (s0, c0) = (la0.sin(), la0.cos());
        let lat = (v[1] * c0 + v[2] * s0).clamp(-1.0, 1.0).asin();
        let lon = self.lon0.to_radians() + v[0].atan2(v[2] * c0 - v[1] * s0);
        (lat.to_degrees(), wrap_lon(lon.to_degrees()))
    }

    /// Screen position of a view-space point (whether or not it is visible).
    pub fn screen(&self, v: [f64; 3]) -> (f64, f64) {
        let depth = (self.d - v[2]).max(1e-6);
        (self.cx + self.f * v[0] / depth, self.cy - self.f * v[1] / depth)
    }

    /// (x, y, z) on screen when the point faces the camera; None behind the globe.
    pub fn project(&self, lat: f64, lon: f64) -> Option<(f64, f64, f64)> {
        let v = self.view_coords(lat, lon);
        (v[2] > self.horizon()).then(|| {
            let (x, y) = self.screen(v);
            (x, y, v[2])
        })
    }

    /// The point of the globe under a screen position; None off the globe.
    pub fn unproject(&self, sx: f64, sy: f64) -> Option<(f64, f64)> {
        let (u, w) = ((sx - self.cx) / self.f, -(sy - self.cy) / self.f);
        let a = u * u + w * w + 1.0;
        let b = -2.0 * self.d;
        let c = self.d * self.d - 1.0;
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let t = (-b - disc.sqrt()) / (2.0 * a);
        Some(self.from_view([u * t, w * t, self.d - t]))
    }

    /// The globe's outline radius on screen.
    pub fn limb_radius(&self) -> f64 {
        self.f / (self.d * self.d - 1.0).max(1e-9).sqrt()
    }

    /// Screen px per km on the ground in the middle of the view.
    pub fn px_per_km(&self) -> f64 {
        self.f / ((self.d - 1.0) * R_KM).max(1e-6)
    }
}

pub const MERC_MAX_LAT: f64 = 85.051_128_78;

/// Web Mercator x of a longitude, 0..1 west to east.
pub fn merc_x(lon: f64) -> f64 {
    (lon + 180.0) / 360.0
}

/// Web Mercator y of a latitude, 0 at the top (85.05°N) to 1 at the bottom.
pub fn merc_y(lat: f64) -> f64 {
    let la = lat.clamp(-MERC_MAX_LAT, MERC_MAX_LAT).to_radians();
    (1.0 - (la.tan() + 1.0 / la.cos()).ln() / PI) / 2.0
}

pub fn lat_of_merc_y(y: f64) -> f64 {
    (PI * (1.0 - 2.0 * y)).sinh().atan().to_degrees()
}

/// The flat map: Web Mercator around (`lat0`, `lon0`), `world` px for the whole width of the
/// world, at the same scale in the middle as the globe at the same altitude.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlatView {
    pub lat0: f64,
    pub lon0: f64,
    pub world: f64,
    pub cx: f64,
    pub cy: f64,
}

impl FlatView {
    pub fn new(lat0: f64, lon0: f64, alt_km: f64, f: f64, centre: (f64, f64)) -> Self {
        let lat0 = lat0.clamp(-80.0, 80.0);
        let world = 2.0 * PI * R_KM * lat0.to_radians().cos() * f / alt_km.max(0.05);
        Self { lat0, lon0, world, cx: centre.0, cy: centre.1 }
    }

    /// Screen position of a point (the copy of the world nearest the view).
    pub fn project(&self, lat: f64, lon: f64) -> (f64, f64) {
        let mut dx = merc_x(lon) - merc_x(self.lon0);
        dx -= dx.round();
        (self.cx + dx * self.world, self.cy + (merc_y(lat) - merc_y(self.lat0)) * self.world)
    }

    /// The point under a screen position (its longitude unwrapped).
    pub fn unproject(&self, sx: f64, sy: f64) -> (f64, f64) {
        let x = merc_x(self.lon0) + (sx - self.cx) / self.world;
        let y = merc_y(self.lat0) + (sy - self.cy) / self.world;
        (lat_of_merc_y(y.clamp(0.0, 1.0)), x * 360.0 - 180.0)
    }

    pub fn px_per_km(&self) -> f64 {
        self.world / (2.0 * PI * R_KM * self.lat0.to_radians().cos().max(1e-6))
    }

    /// The (fractional) Web Mercator zoom level of the view.
    pub fn zoom(&self) -> f64 {
        (self.world / 256.0).log2()
    }

    /// The tiles of level `z` over the screen rectangle `[x0, y0, x1, y1]`, nearest the middle
    /// first. Their x runs on past the antimeridian (`rem_euclid(2^z)` is the tile itself).
    pub fn tiles(&self, rect: [f64; 4], z: u8) -> Vec<(i64, i64)> {
        let n = 1i64 << z;
        let size = self.world / n as f64;
        let gx = |sx: f64| merc_x(self.lon0) * self.world + (sx - self.cx);
        let gy = |sy: f64| merc_y(self.lat0) * self.world + (sy - self.cy);
        let (x0, x1) = ((gx(rect[0]) / size).floor() as i64, (gx(rect[2]) / size).floor() as i64);
        let (y0, y1) = (((gy(rect[1]) / size).floor() as i64).max(0), ((gy(rect[3]) / size).floor() as i64).min(n - 1));
        let (mx, my) = (gx(self.cx) / size, gy(self.cy) / size);
        let mut out: Vec<(i64, i64)> = (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| (x, y))).collect();
        out.sort_by(|a, b| {
            let da = (a.0 as f64 + 0.5 - mx).powi(2) + (a.1 as f64 + 0.5 - my).powi(2);
            let db = (b.0 as f64 + 0.5 - mx).powi(2) + (b.1 as f64 + 0.5 - my).powi(2);
            da.total_cmp(&db)
        });
        out
    }

    /// A tile's rectangle on screen `[x0, y0, x1, y1]` (x unwrapped as `tiles` gives it).
    pub fn tile_rect(&self, z: u8, x: i64, y: i64) -> [f64; 4] {
        let size = self.world / (1i64 << z) as f64;
        let ox = self.cx - merc_x(self.lon0) * self.world;
        let oy = self.cy - merc_y(self.lat0) * self.world;
        [ox + x as f64 * size, oy + y as f64 * size, ox + (x + 1) as f64 * size, oy + (y + 1) as f64 * size]
    }
}

// --------------------------------------------------------------------------- formatting ----

/// "12 742": digits in groups of three.
pub fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// "UTC+03:00", "UTC−04:00" (a real minus sign).
pub fn utc_offset_label(secs: i32) -> String {
    let sign = if secs < 0 { '\u{2212}' } else { '+' };
    let minutes = secs.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The nautical time zone of a longitude, when no real one is known.
pub fn estimate_offset_secs(lon: f64) -> i32 {
    ((wrap_lon(lon) / 15.0).round() as i32) * 3600
}

/// The longest round distance (1, 2 or 5 x 10^n km) whose bar fits `max_px`: (km, px).
pub fn nice_scale(px_per_km: f64, max_px: f64) -> Option<(f64, f64)> {
    if !(px_per_km > 0.0 && max_px > 0.0) {
        return None;
    }
    let max_km = max_px / px_per_km;
    let power = 10f64.powf(max_km.log10().floor());
    let km = [5.0, 2.0, 1.0].iter().map(|m| m * power).find(|km| *km <= max_km)?;
    Some((km, km * px_per_km))
}

/// "500 km", "1 000 km", "200 m".
pub fn distance_label(km: f64) -> String {
    if km >= 1.0 {
        format!("{} km", group_digits(km.round() as i64))
    } else {
        format!("{} m", (km * 1000.0).round() as i64)
    }
}

/// "just now", "12 min ago", "5 h ago", "3 d ago".
pub fn ago(secs: i64) -> String {
    let s = secs.max(0);
    if s < 90 {
        "just now".to_string()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else if s < 48 * 3600 {
        format!("{} h ago", s / 3600)
    } else {
        format!("{} d ago", s / 86_400)
    }
}

/// The same for Jarvis to say.
pub fn spoken_ago(secs: i64) -> String {
    let s = secs.max(0);
    let unit = |n: i64, one: &str| if n == 1 { format!("1 {one} ago") } else { format!("{n} {one}s ago") };
    if s < 90 {
        "just now".to_string()
    } else if s < 3600 {
        unit(s / 60, "minute")
    } else if s < 48 * 3600 {
        unit(s / 3600, "hour")
    } else {
        unit(s / 86_400, "day")
    }
}

/// "Three", "Twelve", "23".
pub fn count_word(n: usize) -> String {
    const WORDS: [&str; 13] = ["No", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Eleven", "Twelve"];
    WORDS.get(n).map_or_else(|| n.to_string(), |w| w.to_string())
}

/// "40.7127, -74.0060".
pub fn coords_label(lat: f64, lon: f64) -> String {
    format!("{lat:.4}, {:.4}", wrap_lon(lon))
}

/// "12 degrees south, 30 degrees west" for Jarvis to say.
pub fn spoken_coords(lat: f64, lon: f64) -> String {
    let lon = wrap_lon(lon);
    let ns = if lat >= 0.0 { "north" } else { "south" };
    let ew = if lon >= 0.0 { "east" } else { "west" };
    format!("{} degrees {ns}, {} degrees {ew}", lat.abs().round() as i64, lon.abs().round() as i64)
}

// ---------------------------------------------------------------------------- the data ----

#[derive(Debug, Clone, PartialEq)]
pub struct Quake {
    pub id: String,
    pub mag: f64,
    /// USGS: "12 km SSW of Tadotsu, Japan".
    pub place: String,
    /// Unix milliseconds.
    pub time_ms: i64,
    pub lat: f64,
    pub lon: f64,
    pub depth_km: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Storm,
    Fire,
}

/// A NASA EONET event: a storm (named on the globe) or a wildfire.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoEvent {
    pub id: String,
    pub kind: EventKind,
    pub title: String,
    /// "ODALYS-26" for a storm, the title for a fire.
    pub label: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iss {
    pub lat: f64,
    pub lon: f64,
    pub alt_km: f64,
    pub speed_kmh: f64,
    /// Unix seconds of the fix.
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flight {
    pub callsign: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    /// Heading, degrees clockwise from north.
    pub track: f64,
    /// Unix seconds of the position.
    pub at: i64,
    /// The aircraft type when a source knows it ("A320").
    pub kind: Option<String>,
}

impl Flight {
    /// Where the aircraft is `secs` after its fix, flying on (capped at a minute).
    pub fn reckoned(&self, secs: f64) -> (f64, f64) {
        let Some(v) = self.speed_kmh.filter(|v| *v > 0.0) else { return (self.lat, self.lon) };
        let dist = v / 3600.0 * secs.clamp(0.0, 60.0);
        destination((self.lat, self.lon), self.track, dist)
    }

    /// "A320 · 10 668 m · 866 km/h" (what is known of it).
    pub fn details(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(kind) = self.kind.as_ref().filter(|k| !k.trim().is_empty()) {
            parts.push(kind.trim().to_string());
        }
        if let Some(alt) = self.alt_m {
            parts.push(format!("{} m", group_digits(alt.round() as i64)));
        }
        if let Some(v) = self.speed_kmh {
            parts.push(format!("{} km/h", group_digits(v.round() as i64)));
        }
        if parts.is_empty() {
            "no altitude or speed reported".to_string()
        } else {
            parts.join(" \u{b7} ")
        }
    }
}

/// The place in view (BigDataCloud).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Place {
    pub country: String,
    pub region: String,
    pub city: String,
    /// An ocean or sea when no country is there.
    pub water: String,
}

impl Place {
    /// The chip's text: the country on the globe, the region (state) on the map.
    pub fn name(&self, close: bool) -> String {
        let pick = if close { [&self.region, &self.country, &self.water] } else { [&self.country, &self.water, &self.region] };
        pick.into_iter().find(|s| !s.trim().is_empty()).cloned().unwrap_or_default()
    }

    /// For Jarvis: "the North Atlantic Ocean", "Brazil".
    pub fn spoken(&self) -> String {
        if !self.country.is_empty() {
            self.country.clone()
        } else if !self.water.is_empty() {
            if self.water.to_lowercase().starts_with("the ") { self.water.clone() } else { format!("the {}", self.water) }
        } else {
            self.region.clone()
        }
    }
}

/// Weather and local time of the place in view (Open-Meteo).
#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    pub temp: f64,
    pub feels: f64,
    pub humidity: Option<f64>,
    pub wind_kmh: Option<f64>,
    /// Where the wind comes from, degrees.
    pub wind_from: Option<f64>,
    pub code: Option<i64>,
    pub offset_secs: i32,
    /// "06:58", "19:10".
    pub sunrise: Option<String>,
    pub sunset: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceKind {
    City,
    Region,
    Country,
    Other,
    Continent,
}

impl PlaceKind {
    /// How high the camera flies to show one.
    pub fn altitude_km(self) -> f64 {
        match self {
            Self::City => 2_600.0,
            Self::Region => 3_600.0,
            Self::Country => 5_500.0,
            Self::Other => 4_000.0,
            Self::Continent => 6_500.0,
        }
    }
}

/// A continent or an ocean by name ("Europe", "Europa", "the Pacific"): the geocoder knows
/// towns called that, not the continents.
pub fn continent(name: &str) -> Option<GeoHit> {
    const CONTINENTS: [(&[&str], &str, f64, f64); 10] = [
        (&["europe", "europa"], "Europe", 50.0, 12.0),
        (&["africa", "afrika"], "Africa", 3.0, 20.0),
        (&["asia", "asien"], "Asia", 38.0, 92.0),
        (&["north america", "nordamerika"], "North America", 45.0, -100.0),
        (&["south america", "südamerika", "suedamerika"], "South America", -18.0, -60.0),
        (&["australia", "australien", "oceania", "ozeanien"], "Australia", -25.0, 134.0),
        (&["antarctica", "antarktis"], "Antarctica", -78.0, 20.0),
        (&["the atlantic", "atlantic", "atlantic ocean", "atlantik"], "the Atlantic", 20.0, -38.0),
        (&["the pacific", "pacific", "pacific ocean", "pazifik"], "the Pacific", 5.0, -150.0),
        (&["the indian ocean", "indian ocean", "indischer ozean"], "the Indian Ocean", -15.0, 78.0),
    ];
    let key = name.trim().to_lowercase();
    CONTINENTS.iter().find(|(names, ..)| names.contains(&key.as_str())).map(|(_, n, lat, lon)| GeoHit { name: n.to_string(), lat: *lat, lon: *lon, kind: PlaceKind::Continent })
}

/// A geocoded place.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoHit {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub kind: PlaceKind,
}

/// A place the camera flew to ("New York"): a white reticle with its name.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// "Earthquakes in this area": the circle on the map and the drawer under it.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaQuery {
    pub id: u64,
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
    pub days: u32,
    /// When it was asked, "22:40:21".
    pub asked: String,
    pub results: Option<Vec<Quake>>,
    pub failed: Option<String>,
}

impl AreaQuery {
    /// "0 - 500 KM - 7 D".
    pub fn header(&self) -> String {
        format!("0 - {} KM - {} D", group_digits(self.radius_km.round() as i64), self.days)
    }
}

/// A box of lat/lon (OpenSky's query).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Bbox {
    pub fn around(lat: f64, lon: f64, half_lat: f64, half_lon: f64) -> Self {
        Self { south: (lat - half_lat).max(-85.0), north: (lat + half_lat).min(85.0), west: (lon - half_lon).max(-180.0), east: (lon + half_lon).min(180.0) }
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        (self.south..=self.north).contains(&lat) && (self.west..=self.east).contains(&wrap_lon(lon))
    }

    pub fn centre(&self) -> (f64, f64) {
        ((self.south + self.north) / 2.0, (self.west + self.east) / 2.0)
    }

    pub fn span(&self) -> (f64, f64) {
        (self.north - self.south, self.east - self.west)
    }
}

// ---------------------------------------------------------------------------- parsers ----

/// USGS GeoJSON (the summary feeds and the FDSN query alike).
pub fn parse_quakes(v: &Value) -> Vec<Quake> {
    let Some(features) = v["features"].as_array() else { return Vec::new() };
    features
        .iter()
        .filter_map(|f| {
            let p = &f["properties"];
            let c = f["geometry"]["coordinates"].as_array()?;
            let (lon, lat) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
            Some(Quake {
                id: f["id"].as_str().unwrap_or_default().to_string(),
                mag: p["mag"].as_f64()?,
                place: p["place"].as_str().unwrap_or_default().trim().to_string(),
                time_ms: p["time"].as_i64().unwrap_or(0),
                lat,
                lon,
                depth_km: c.get(2).and_then(Value::as_f64).unwrap_or(0.0),
            })
        })
        .collect()
}

/// "Tropical Storm Odalys" in 2026 → "ODALYS-26".
pub fn storm_label(title: &str, year: Option<i32>) -> String {
    const WORDS: [&str; 15] = ["super", "severe", "very", "intense", "tropical", "subtropical", "post-tropical", "extratropical", "storm", "hurricane", "typhoon", "cyclone", "depression", "potential", "cyclonic"];
    let name: Vec<&str> = title.split_whitespace().filter(|w| !WORDS.contains(&w.to_lowercase().as_str())).collect();
    let name = if name.is_empty() { title.trim().to_uppercase() } else { name.join(" ").to_uppercase() };
    match year {
        Some(y) => format!("{name}-{:02}", y.rem_euclid(100)),
        None => name,
    }
}

/// NASA EONET v3 events: storms and wildfires at their latest position.
pub fn parse_events(v: &Value) -> Vec<GeoEvent> {
    let Some(events) = v["events"].as_array() else { return Vec::new() };
    events
        .iter()
        .filter_map(|e| {
            let categories = e["categories"].as_array()?;
            let kind = categories.iter().find_map(|c| match c["id"].as_str()? {
                "severeStorms" => Some(EventKind::Storm),
                "wildfires" => Some(EventKind::Fire),
                _ => None,
            })?;
            let geometry = e["geometry"].as_array()?;
            let latest = geometry.iter().max_by(|a, b| a["date"].as_str().unwrap_or("").cmp(b["date"].as_str().unwrap_or("")))?;
            let (lat, lon) = event_point(latest)?;
            let title = e["title"].as_str().unwrap_or_default().trim().to_string();
            let year = latest["date"].as_str().and_then(|d| DateTime::parse_from_rfc3339(d).ok()).map(|d| d.year());
            let label = match kind {
                EventKind::Storm => storm_label(&title, year),
                EventKind::Fire => title.clone(),
            };
            Some(GeoEvent { id: e["id"].as_str().unwrap_or_default().to_string(), kind, title, label, lat, lon })
        })
        .collect()
}

/// A point of an EONET geometry: the point itself, or the middle of a polygon's outer ring.
fn event_point(g: &Value) -> Option<(f64, f64)> {
    let c = &g["coordinates"];
    match g["type"].as_str()? {
        "Point" => Some((c.get(1)?.as_f64()?, c.get(0)?.as_f64()?)),
        "Polygon" => {
            let ring = c.get(0)?.as_array()?;
            let points: Vec<(f64, f64)> = ring.iter().filter_map(|p| Some((p.get(1)?.as_f64()?, p.get(0)?.as_f64()?))).collect();
            (!points.is_empty()).then(|| {
                let n = points.len() as f64;
                (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n)
            })
        }
        _ => None,
    }
}

/// wheretheiss.at: the station now.
pub fn parse_iss(v: &Value) -> Option<Iss> {
    Some(Iss {
        lat: v["latitude"].as_f64()?,
        lon: v["longitude"].as_f64()?,
        alt_km: v["altitude"].as_f64().unwrap_or(420.0),
        speed_kmh: v["velocity"].as_f64().unwrap_or(27_600.0),
        at: v["timestamp"].as_i64().unwrap_or(0),
    })
}

/// wheretheiss.at positions: its ground track, oldest first.
pub fn parse_iss_track(v: &Value) -> Vec<(f64, f64)> {
    let Some(list) = v.as_array() else { return Vec::new() };
    let mut points: Vec<(i64, f64, f64)> = list.iter().filter_map(|p| Some((p["timestamp"].as_i64().unwrap_or(0), p["latitude"].as_f64()?, p["longitude"].as_f64()?))).collect();
    points.sort_by_key(|p| p.0);
    points.into_iter().map(|p| (p.1, p.2)).collect()
}

/// OpenSky `states/all`: the aircraft in the air with a position.
pub fn parse_flights(v: &Value) -> Vec<Flight> {
    let time = v["time"].as_i64().unwrap_or(0);
    let Some(states) = v["states"].as_array() else { return Vec::new() };
    states
        .iter()
        .filter_map(|s| {
            let s = s.as_array()?;
            if s.get(8).and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let (lon, lat) = (s.get(5)?.as_f64()?, s.get(6)?.as_f64()?);
            let callsign = s.get(1).and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).or_else(|| s.first().and_then(Value::as_str).map(str::to_uppercase))?;
            Some(Flight {
                callsign,
                lat,
                lon,
                alt_m: s.get(7).and_then(Value::as_f64).or_else(|| s.get(13).and_then(Value::as_f64)),
                speed_kmh: s.get(9).and_then(Value::as_f64).map(|v| v * 3.6),
                track: s.get(10).and_then(Value::as_f64).unwrap_or(0.0),
                at: s.get(3).and_then(Value::as_i64).unwrap_or(time),
                kind: None,
            })
        })
        .collect()
}

/// "2026-09-21T06:58" → "06:58".
fn clock_of(stamp: &str) -> Option<String> {
    let t = stamp.split('T').nth(1)?;
    (t.len() >= 5).then(|| t[..5].to_string())
}

/// Open-Meteo forecast: the current weather, the time zone's offset, sunrise and sunset.
pub fn parse_weather(v: &Value) -> Option<Weather> {
    let c = &v["current"];
    let temp = c["temperature_2m"].as_f64()?;
    let daily = &v["daily"];
    Some(Weather {
        temp,
        feels: c["apparent_temperature"].as_f64().unwrap_or(temp),
        humidity: c["relative_humidity_2m"].as_f64(),
        wind_kmh: c["wind_speed_10m"].as_f64(),
        wind_from: c["wind_direction_10m"].as_f64(),
        code: c["weather_code"].as_i64(),
        offset_secs: v["utc_offset_seconds"].as_i64().unwrap_or(0) as i32,
        sunrise: daily["sunrise"].get(0).and_then(Value::as_str).and_then(clock_of),
        sunset: daily["sunset"].get(0).and_then(Value::as_str).and_then(clock_of),
    })
}

/// Open-Meteo geocoding: the first hit.
pub fn parse_geocode(v: &Value) -> Option<GeoHit> {
    let hit = v["results"].as_array()?.first()?;
    let code = hit["feature_code"].as_str().unwrap_or("");
    let kind = if code.starts_with("PPL") {
        PlaceKind::City
    } else if code.starts_with("PCL") {
        PlaceKind::Country
    } else if code.starts_with("ADM") {
        PlaceKind::Region
    } else {
        PlaceKind::Other
    };
    Some(GeoHit { name: hit["name"].as_str()?.to_string(), lat: hit["latitude"].as_f64()?, lon: hit["longitude"].as_f64()?, kind })
}

/// "United States of America (the)" → "United States".
pub fn clean_country(name: &str) -> String {
    let n = name.trim();
    let n = n.strip_suffix(" (the)").unwrap_or(n).trim();
    let known = [
        ("United States of America", "United States"),
        ("United Kingdom of Great Britain and Northern Ireland", "United Kingdom"),
        ("Russian Federation", "Russia"),
        ("Korea (the Republic of)", "South Korea"),
        ("Korea (the Democratic People's Republic of)", "North Korea"),
        ("Viet Nam", "Vietnam"),
        ("Syrian Arab Republic", "Syria"),
        ("Lao People's Democratic Republic", "Laos"),
        ("Congo (the Democratic Republic of the)", "DR Congo"),
        ("Tanzania, the United Republic of", "Tanzania"),
        ("Taiwan (Province of China)", "Taiwan"),
    ];
    if let Some((_, short)) = known.iter().find(|(long, _)| long.eq_ignore_ascii_case(n)) {
        return short.to_string();
    }
    // "Iran (Islamic Republic of)", "Bolivia (Plurinational State of)".
    match n.find(" (") {
        Some(i) if n.ends_with(')') => n[..i].to_string(),
        _ => n.to_string(),
    }
}

/// BigDataCloud reverse geocoding.
pub fn parse_reverse(v: &Value) -> Option<Place> {
    let s = |key: &str| v[key].as_str().unwrap_or_default().trim().to_string();
    let country = clean_country(&s("countryName"));
    let water = if country.is_empty() {
        let informative = v["localityInfo"]["informative"].as_array();
        informative
            .and_then(|list| {
                list.iter().filter_map(|i| i["name"].as_str()).find(|n| {
                    let l = n.to_lowercase();
                    l.contains("ocean") || l.contains(" sea") || l.ends_with("sea") || l.contains("gulf") || l.contains("bay")
                })
            })
            .map(str::to_string)
            .unwrap_or_else(|| s("locality"))
    } else {
        String::new()
    };
    let place = Place { country, region: s("principalSubdivision"), city: { let c = s("city"); if c.is_empty() { s("locality") } else { c } }, water };
    (!place.name(false).is_empty()).then_some(place)
}

// ------------------------------------------------------------------------ spoken lines ----

/// "12 km SSW of Tadotsu, Japan" → "Tadotsu, Japan".
pub fn near_of(place: &str) -> String {
    let p = place.trim();
    match p.find(" of ") {
        Some(i) => p[i + 4..].trim().to_string(),
        None => p.to_string(),
    }
}

/// Where an area query looked.
#[derive(Debug, Clone, PartialEq)]
pub enum AreaName {
    /// "In this area": the point in view.
    Here,
    /// Sir's own (approximate) location.
    Mine,
    Named(String),
}

/// The spoken answer to an area query: none, one or several quakes.
pub fn quake_answer(found: &[Quake], radius_km: f64, area: &AreaName, now_ms: i64) -> String {
    let km = group_digits(radius_km.round() as i64);
    if found.is_empty() {
        return match area {
            AreaName::Here => "There have been no recent earthquakes in this area, Sir.".to_string(),
            AreaName::Mine => format!("There have been no earthquakes within {km} kilometres of you in the past week, Sir."),
            AreaName::Named(name) => format!("There have been no earthquakes within {km} kilometres of {name} in the past week, Sir."),
        };
    }
    let strongest = found.iter().max_by(|a, b| a.mag.total_cmp(&b.mag)).expect("not empty");
    let near = near_phrase(&strongest.place);
    if found.len() == 1 {
        let when = spoken_ago((now_ms - strongest.time_ms) / 1000);
        return format!("One in the last week, Sir: a magnitude {:.1}{near}, {when}.", strongest.mag);
    }
    format!("{} in the last week, Sir; the strongest a magnitude {:.1}{near}.", count_word(found.len()), strongest.mag)
}

/// The spoken line for the week's quakes on the whole globe.
pub fn global_quake_answer(all: &[Quake]) -> String {
    let Some(strongest) = all.iter().max_by(|a, b| a.mag.total_cmp(&b.mag)) else {
        return "No earthquakes of magnitude 4.5 or more in the past week, Sir.".to_string();
    };
    let near = near_phrase(&strongest.place);
    format!("The past week's earthquakes are on the globe, Sir: {} of magnitude 4.5 or more, the strongest a {:.1}{near}.", all.len(), strongest.mag)
}

/// Where a quake was, said after its magnitude: " near Pisco, Peru" (USGS "23 km SSW of Pisco,
/// Peru"), " in the Fiji region", " in the Banda Sea", " on the central Mid-Atlantic Ridge",
/// " near the South Sandwich Islands"; nothing for no place.
pub fn near_phrase(place: &str) -> String {
    let p = place.trim();
    if p.is_empty() {
        return String::new();
    }
    if p.contains(" of ") {
        return format!(" near {}", near_of(p));
    }
    let lower = p.to_ascii_lowercase();
    let the = if lower.starts_with("the ") { "" } else { "the " };
    match lower.rsplit(' ').next().unwrap_or_default() {
        "region" | "sea" | "ocean" | "basin" | "trench" | "strait" | "gulf" | "channel" | "passage" => format!(" in {the}{p}"),
        "ridge" | "rise" | "zone" => format!(" on {the}{p}"),
        "islands" => format!(" near {the}{p}"),
        _ => format!(" near {p}"),
    }
}

/// "ODALYS-26" → "Odalys".
fn storm_name(label: &str) -> String {
    let name = label.rsplit_once('-').filter(|(_, y)| y.len() == 2 && y.chars().all(|c| c.is_ascii_digit())).map_or(label, |(n, _)| n);
    name.split_whitespace()
        .map(|w| {
            let lower = w.to_lowercase();
            let mut chars = lower.chars();
            chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "Odalys, Polo and Fay".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub fn storm_answer(storms: &[GeoEvent]) -> String {
    let names: Vec<String> = storms.iter().map(|s| storm_name(&s.label)).collect();
    match names.len() {
        0 => "No storms are being tracked right now, Sir.".to_string(),
        1 => format!("One storm is active, Sir: {}.", names[0]),
        n if n <= 6 => format!("{} storms are active, Sir: {}.", count_word(n), join_names(&names)),
        n => format!("{} storms are active, Sir; they are marked on the globe.", count_word(n)),
    }
}

pub fn fire_answer(fires: usize) -> String {
    match fires {
        0 => "No wildfires are being tracked right now, Sir.".to_string(),
        1 => "NASA is tracking one active wildfire, Sir; it is marked on the globe.".to_string(),
        n => format!("NASA is tracking {n} active wildfires, Sir; they are marked on the globe."),
    }
}

// ------------------------------------------------------------------------------ actions ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    Quakes,
    Storms,
    Fires,
    Iss,
    Flights,
    Cameras, // cameras (C2)
}

impl Layer {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_lowercase().as_str() {
            "quakes" | "quake" | "earthquakes" | "earthquake" => Self::Quakes,
            "storms" | "storm" | "hurricanes" => Self::Storms,
            "fires" | "fire" | "wildfires" | "wildfire" => Self::Fires,
            "iss" | "space station" | "satellite" => Self::Iss,
            "flights" | "flight" | "planes" | "aircraft" => Self::Flights,
            "cameras" | "camera" | "webcams" | "webcam" => Self::Cameras, // cameras (C2)
            _ => return None,
        })
    }

    /// For Jarvis: "The flights are off, Sir."
    pub fn noun(self) -> &'static str {
        match self {
            Self::Quakes => "earthquakes",
            Self::Storms => "storms",
            Self::Fires => "wildfires",
            Self::Iss => "space station",
            Self::Flights => "flights",
            Self::Cameras => "cameras", // cameras (C2)
        }
    }

    /// The control column's hint.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Quakes => "Earthquakes: magnitude 4.5 and more, the past week (USGS)",
            Self::Storms => "Storms (NASA EONET)",
            Self::Fires => "Wildfires (NASA EONET)",
            Self::Iss => "The ISS, its footprint and ground track (wheretheiss.at)",
            Self::Flights => "Flights over the map in view (OpenSky Network)",
            Self::Cameras => "Live public cameras (click one for its feed)", // cameras (C2)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layers {
    pub quakes: bool,
    pub storms: bool,
    pub fires: bool,
    pub iss: bool,
    pub flights: bool,
}

impl Layers {
    pub const DEFAULT: Layers = Layers { quakes: true, storms: true, fires: true, iss: true, flights: false };

    pub fn get(&self, layer: Layer) -> bool {
        match layer {
            Layer::Quakes => self.quakes,
            Layer::Storms => self.storms,
            Layer::Fires => self.fires,
            Layer::Iss => self.iss,
            Layer::Flights => self.flights,
            Layer::Cameras => crate::webcams::earth_layer(), // cameras (C2)
        }
    }

    pub fn set(&mut self, layer: Layer, on: bool) {
        match layer {
            Layer::Quakes => self.quakes = on,
            Layer::Storms => self.storms = on,
            Layer::Fires => self.fires = on,
            Layer::Iss => self.iss = on,
            Layer::Flights => self.flights = on,
            Layer::Cameras => crate::webcams::set_earth_layer(on), // cameras (C2)
        }
    }
}

/// What `open_jarvis_view {"view": "earth", …}` asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Show,
    Satellite,
    Holo,
    Place(String),
    Quakes { place: Option<String>, radius_km: Option<f64> },
    Flights(Option<String>),
    Storms,
    Fires,
    Iss,
    Hide(Layer),
    HideAll,
    ZoomIn,
    ZoomOut,
    Home,
    Spin,
    StopSpin,
    /// "Where am I?": the "YOU" marker on Sir's position.
    Locate,
    /// Shaded terrain and the peaks' names; over a place, or the nearest mountains.
    Mountains(Option<String>),
    /// The relief steeper than life (or back).
    Exaggerate(bool),
    /// The rain radar; `animate`: the past two hours in a loop.
    Radar { animate: bool },
    /// Live infrared cloud tops.
    Clouds,
    Overlay(Overlay, bool),
    Imagery(Imagery),
    /// NASA's daily true colour of a day (over a place).
    Historic { date: NaiveDate, place: Option<String> },
    /// Back to today's imagery.
    Today,
    Tour,
    StopTour,
    Cameras, // cameras (C2)
    // live layers (C1): satellites, trains, traffic (earth_layers.rs).
    Live(crate::earth_layers::LiveAction),
}

/// The map's overlays (on top of the imagery, not live layers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Overlay {
    /// Place names and borders.
    Labels,
    /// Shaded relief and the peaks.
    Terrain,
    Radar,
    Clouds,
}

impl Overlay {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_lowercase().as_str() {
            "labels" | "label" | "borders" | "border" | "names" | "places" | "boundaries" => Self::Labels,
            "terrain" | "relief" | "mountains" | "peaks" | "hillshade" | "elevation" => Self::Terrain,
            "radar" | "rain" | "rain radar" | "weather radar" | "precipitation" => Self::Radar,
            "clouds" | "cloud" | "infrared" | "cloud cover" | "satellite clouds" => Self::Clouds,
            _ => return None,
        })
    }

    pub fn noun(self) -> &'static str {
        match self {
            Self::Labels => "labels and borders",
            Self::Terrain => "terrain",
            Self::Radar => "rain radar",
            Self::Clouds => "clouds",
        }
    }
}

/// Which overlays are on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overlays {
    pub labels: bool,
    pub terrain: bool,
    pub exaggerate: bool,
    pub radar: bool,
    pub animate: bool,
    pub clouds: bool,
}

impl Overlays {
    pub const DEFAULT: Overlays = Overlays { labels: true, terrain: false, exaggerate: false, radar: false, animate: false, clouds: false };

    pub fn get(&self, o: Overlay) -> bool {
        match o {
            Overlay::Labels => self.labels,
            Overlay::Terrain => self.terrain,
            Overlay::Radar => self.radar,
            Overlay::Clouds => self.clouds,
        }
    }

    pub fn set(&mut self, o: Overlay, on: bool) {
        match o {
            Overlay::Labels => self.labels = on,
            Overlay::Terrain => {
                self.terrain = on;
                self.exaggerate &= on;
            }
            Overlay::Radar => {
                self.radar = on;
                self.animate &= on;
            }
            Overlay::Clouds => self.clouds = on,
        }
    }
}

/// The tool's arguments as an action.
pub fn parse_action(args: &Value) -> Result<Action, String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let place = text("place");
    let radius = args.get("radius_km").and_then(|r| r.as_f64().or_else(|| r.as_str().and_then(|s| s.trim().parse().ok()))).filter(|r| *r > 0.0);
    // A place and no action means flying there (2026-11-09: {"place":"New York","view":"earth"}
    // from the model opened the globe and stayed put).
    let action = text("action").unwrap_or_else(|| if place.is_some() { "show_place" } else { "show" }.to_string()).to_lowercase();
    // live layers (C1)
    if let Some(live) = crate::earth_layers::parse_action(&action, args) {
        return live.map(Action::Live);
    }
    let today = Local::now().date_naive();
    let date = match text("date") {
        Some(d) => Some(src::parse_date(&d, today).ok_or_else(|| format!("'{d}' is not a past date the imagery has (\"yesterday\", \"1 March 2024\", 2024-03-01)"))?),
        None => None,
    };
    let overlay = |name: Option<String>| -> Result<Overlay, String> {
        let name = name.ok_or("hide_layer needs a layer")?;
        Overlay::parse(&name).ok_or_else(|| format!("'{name}' is not a layer of the Earth (quakes, flights, storms, fires, iss, labels, terrain, radar, clouds)"))
    };
    Ok(match action.as_str() {
        // (The "show_" names pass the question guard: "where am I?" only looks.)
        "show_my_location" | "locate_me" | "locate" | "where_am_i" | "show_me" | "my_location" => Action::Locate,
        "show_terrain" | "terrain" | "show_mountains" | "mountains" | "show_relief" => Action::Mountains(place),
        "exaggerate_terrain" | "exaggerate" => Action::Exaggerate(true),
        "flatten_terrain" | "normal_terrain" => Action::Exaggerate(false),
        "show_radar" | "radar" | "rain_radar" | "weather_radar" => Action::Radar { animate: false },
        "animate_radar" | "radar_loop" | "play_radar" => Action::Radar { animate: true },
        "show_clouds" | "clouds" | "infrared" => match date {
            Some(date) => Action::Historic { date, place },
            None => Action::Clouds,
        },
        "show_labels" | "labels" | "show_borders" | "borders" => Action::Overlay(Overlay::Labels, true),
        "hide_labels" | "hide_borders" => Action::Overlay(Overlay::Labels, false),
        "set_imagery" | "imagery" | "switch_imagery" => {
            let name = text("imagery").or(place).ok_or("set_imagery needs an imagery (esri, sentinel or nasa)")?;
            Action::Imagery(Imagery::parse(&name).ok_or_else(|| format!("'{name}' is not an imagery of the Earth (esri, sentinel, nasa)"))?)
        }
        "show_date" | "historic" | "show_history" | "history" | "past_imagery" => Action::Historic { date: date.ok_or("show_date needs a date (\"yesterday\", \"1 March 2024\")")?, place },
        "show_today" | "today" | "live_imagery" | "present" => Action::Today,
        "show_world_tour" | "world_tour" | "tour" | "world_briefing" | "whats_happening" => Action::Tour,
        "stop_tour" | "end_tour" => Action::StopTour,
        "hide_layer" if text("layer").as_deref().and_then(Layer::parse).is_none() && text("layer").as_deref().and_then(Overlay::parse).is_some() => Action::Overlay(overlay(text("layer"))?, false),
        "show" | "open" => Action::Show,
        "show_satellite" | "satellite" => Action::Satellite,
        "show_holo" | "holo" | "show_holographic" => Action::Holo,
        "show_place" | "fly_to" | "place" => Action::Place(place.ok_or("show_place needs a place to fly to")?),
        "show_quakes" | "quakes" | "earthquakes" => Action::Quakes { place, radius_km: radius },
        "show_flights" | "flights" => Action::Flights(place),
        "show_storms" | "storms" => Action::Storms,
        "show_fires" | "fires" | "wildfires" => Action::Fires,
        "show_iss" | "iss" => Action::Iss,
        "hide_layer" => {
            let layer = text("layer").ok_or("hide_layer needs a layer (quakes, flights, storms, fires, iss)")?;
            Action::Hide(Layer::parse(&layer).ok_or_else(|| format!("'{layer}' is not a layer of the Earth (quakes, flights, storms, fires, iss)"))?)
        }
        "hide_layers" | "hide_all" => Action::HideAll,
        "zoom_in" => Action::ZoomIn,
        "zoom_out" => Action::ZoomOut,
        "home" | "reset" => Action::Home,
        "spin" => Action::Spin,
        "stop_spin" => Action::StopSpin,
        "show_cameras" | "cameras" | "webcams" => Action::Cameras, // cameras (C2)
        other => return Err(format!("'{other}' is not something the Earth does (show, show_satellite, show_holo, show_place, show_quakes, show_flights, show_storms, show_fires, show_iss, show_cameras, hide_layer, hide_layers, zoom_in, zoom_out, home, spin, stop_spin, show_my_location, show_mountains, show_terrain, exaggerate_terrain, show_radar, animate_radar, show_clouds, show_labels, hide_labels, set_imagery, show_date, show_today, show_world_tour, stop_tour).")),
    })
}

// ------------------------------------------------------------------------------ imagery ----

/// The holographic texture's own colours, from the theme's hue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoloPalette {
    pub ocean_deep: [u8; 3],
    pub ocean_shallow: [u8; 3],
    pub land_dark: [u8; 3],
    pub land: [u8; 3],
    pub dot: [u8; 3],
    pub coast: [u8; 3],
    pub border: [u8; 3],
}

/// The holo look in the theme's hue (the reference blue for Arc Cyan).
pub fn holo_palette(hue: f32) -> HoloPalette {
    let c = |dh: f32, s: f32, v: f32| crate::theme_engine::from_hsv(hue + dh, s, v);
    HoloPalette {
        ocean_deep: c(10.0, 0.90, 0.30),
        ocean_shallow: c(6.0, 0.86, 0.50),
        land_dark: c(-2.0, 0.66, 0.66),
        land: c(-2.0, 0.60, 0.88),
        dot: c(-6.0, 0.40, 1.0),
        coast: c(-10.0, 0.20, 1.0),
        border: c(-6.0, 0.36, 0.94),
    }
}

/// The globe's hue: the theme's primary turned a little towards blue (Arc Cyan → the reference's
/// #5AB0E8 continents); a colourless theme gets the reference blue.
pub fn theme_hue() -> f32 {
    let p = crate::theme_engine::current();
    let (h, s, _) = crate::theme_engine::to_hsv(p.core_primary);
    if s < 0.2 {
        206.0
    } else {
        (h + 20.0).rem_euclid(360.0)
    }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

pub const LAND: u8 = 1;
pub const COAST: u8 = 2;
pub const BORDER: u8 = 4;

/// Per texel of the holographic texture: what is there (`LAND`, `COAST`, `BORDER` bits) and how
/// bright the source imagery was (relief and sea depth).
#[derive(Debug, Clone, PartialEq)]
pub struct HoloMask {
    pub w: usize,
    pub h: usize,
    pub texels: Vec<[u8; 2]>,
}

/// The Blue Marble shaded relief: blue is water (deep or shallow), everything else land (ice,
/// desert, forest, rock). The deepest basins are nearly black, rgb(2, 5, 20): read as land, a
/// quarter of the open Atlantic lost its sun glint to black blots (2026-11-09).
pub fn is_water([r, g, b]: [u8; 3]) -> bool {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    (b > r + 18 || (b < 48 && b >= r + 8)) && b + 12 >= g
}

/// Ocean and land from the relief imagery; coastlines and borders from the GIBS line layers
/// when they came (any opaque pixel is a line), otherwise the coast is where land meets water.
pub fn build_mask(relief: &image::RgbImage, coast: Option<&image::RgbaImage>, borders: Option<&image::RgbaImage>) -> HoloMask {
    let (w, h) = (relief.width() as usize, relief.height() as usize);
    let mut land = vec![false; w * h];
    let mut shade = vec![0u8; w * h];
    for (x, y, p) in relief.enumerate_pixels() {
        let i = y as usize * w + x as usize;
        land[i] = !is_water(p.0);
        shade[i] = if land[i] { ((p.0[0] as u32 + p.0[1] as u32 + p.0[2] as u32) / 3) as u8 } else { p.0[2] };
    }
    // One lone texel among four of the other kind takes theirs (a speck of blue shadow in the
    // mountains is no lake).
    if w > 2 && h > 2 {
        let source = land.clone();
        for y in 1..h - 1 {
            for x in 0..w {
                let i = y * w + x;
                let n = [source[y * w + (x + w - 1) % w], source[y * w + (x + 1) % w], source[(y - 1) * w + x], source[(y + 1) * w + x]];
                if n.iter().all(|v| *v == n[0]) && n[0] != source[i] {
                    land[i] = n[0];
                }
            }
        }
    }
    let line = |img: Option<&image::RgbaImage>, x: usize, y: usize| {
        img.is_some_and(|m| {
            let (mx, my) = ((x * m.width() as usize / w) as u32, (y * m.height() as usize / h) as u32);
            m.get_pixel(mx.min(m.width() - 1), my.min(m.height() - 1)).0[3] > 96
        })
    };
    let mut texels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut class = if land[i] { LAND } else { 0 };
            let is_coast = match coast {
                Some(_) => line(coast, x, y),
                None => {
                    land[i] && [(x + w - 1) % w + y * w, (x + 1) % w + y * w, x + y.saturating_sub(1) * w, x + (y + 1).min(h - 1) * w].iter().any(|j| !land[*j])
                }
            };
            if is_coast {
                class |= COAST;
            } else if line(borders, x, y) {
                class |= BORDER;
            }
            texels.push([class, shade[i]]);
        }
    }
    HoloMask { w, h, texels }
}

/// The mask in the palette's colours: deep-blue ocean (lighter over the shelves), light-blue
/// land with a fine dot grid, bright thin coastlines and dimmer borders.
pub fn colour_holo(mask: &HoloMask, p: &HoloPalette) -> ColorImage {
    let mut pixels = Vec::with_capacity(mask.w * mask.h);
    for (i, t) in mask.texels.iter().enumerate() {
        let (x, y) = (i % mask.w, i / mask.w);
        let c = if t[0] & COAST != 0 {
            p.coast
        } else if t[0] & BORDER != 0 {
            p.border
        } else if t[0] & LAND != 0 {
            if x % 3 == 0 && y % 3 == 0 {
                p.dot
            } else {
                mix(p.land_dark, p.land, 0.55 + 0.45 * t[1] as f32 / 255.0)
            }
        } else {
            mix(p.ocean_deep, p.ocean_shallow, ((t[1] as f32 - 60.0) / 160.0).clamp(0.0, 1.0) * 0.7)
        };
        pixels.push(Color32::from_rgb(c[0], c[1], c[2]));
    }
    ColorImage { size: [mask.w, mask.h], pixels }
}

/// The globe before (or without) imagery: a deep-blue ball with a faint dot lattice; the
/// graticule and the rim are drawn over it.
pub fn procedural_texture(w: usize, h: usize, hue: f32) -> ColorImage {
    let p = holo_palette(hue);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let lat = 90.0 - (y as f32 + 0.5) / h as f32 * 180.0;
        let base = mix(p.ocean_deep, p.ocean_shallow, 0.35 * (1.0 - (lat.abs() / 90.0)));
        for x in 0..w {
            let dot = (y % 4 == 0) && ((x + if y % 8 == 0 { 0 } else { 2 }) % 4 == 0);
            let c = if dot { mix(base, p.dot, 0.22) } else { base };
            pixels.push(Color32::from_rgb(c[0], c[1], c[2]));
        }
    }
    ColorImage { size: [w, h], pixels }
}

/// A GIBS overlay tile as white light with the tile's own alpha (tinted when drawn).
fn as_light(img: &image::RgbaImage) -> ColorImage {
    let pixels = img.pixels().map(|p| Color32::from_rgba_premultiplied(p.0[3], p.0[3], p.0[3], p.0[3])).collect();
    ColorImage { size: [img.width() as usize, img.height() as usize], pixels }
}

fn to_color_image(img: &image::RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

pub fn globe_texture_options() -> TextureOptions {
    TextureOptions { magnification: TextureFilter::Linear, minification: TextureFilter::Linear, wrap_mode: TextureWrapMode::Repeat, mipmap_mode: Some(TextureFilter::Linear) }
}

// -------------------------------------------------------------------------------- tiles ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TileLayer {
    /// The Blue Marble shaded relief, classified into the holo look.
    Holo,
    /// The Blue Marble Next Generation true-colour mosaic.
    Satellite,
    /// Borders and coastlines.
    Features,
    /// Place names.
    Labels,
    /// Esri World Imagery (to street level).
    Esri,
    /// EOX Sentinel-2 cloudless.
    Sentinel,
    /// Esri's place names and borders.
    Reference,
    /// Terrarium elevation shaded on the fly (the exaggeration in tenths).
    Terrain(u8),
    /// RainViewer radar of one frame (its Unix time).
    Radar(u32),
    /// Infrared cloud tops (the half hour since 1970).
    Clouds(u32),
    /// NASA's daily true colour of a day (days since 1970).
    Daily(i32),
}

impl TileLayer {
    fn gibs(self) -> &'static str {
        match self {
            Self::Holo => "BlueMarble_ShadedRelief_Bathymetry",
            Self::Satellite => "BlueMarble_NextGeneration",
            Self::Features => "Reference_Features_15m",
            Self::Labels => "Reference_Labels_15m",
            Self::Daily(_) => src::DAILY_LAYER,
            _ => "",
        }
    }

    /// GIBS layers with a fixed time whose URL form is probed once.
    fn probed(self) -> bool {
        matches!(self, Self::Holo | Self::Satellite | Self::Features | Self::Labels)
    }

    pub fn max_level(self) -> u8 {
        match self {
            Self::Holo | Self::Satellite => 8,
            Self::Features | Self::Labels => 13,
            Self::Esri => Imagery::Esri.max_level(),
            Self::Sentinel => Imagery::Sentinel.max_level(),
            Self::Reference => src::REFERENCE_MAX_LEVEL,
            Self::Terrain(_) => src::TERRAIN_MAX_LEVEL,
            Self::Radar(_) => src::RADAR_MAX_LEVEL,
            Self::Clouds(_) => src::IR_MAX_LEVEL,
            Self::Daily(_) => src::DAILY_MAX_LEVEL,
        }
    }

    /// The imagery layer of a choice.
    pub fn of(imagery: Imagery) -> Self {
        match imagery {
            Imagery::Esri => Self::Esri,
            Imagery::Sentinel => Self::Sentinel,
            Imagery::Nasa => Self::Satellite,
        }
    }

    fn dir(self) -> String {
        match self {
            Self::Holo => "holo".into(),
            Self::Satellite => "satellite".into(),
            Self::Features => "features".into(),
            Self::Labels => "labels".into(),
            Self::Esri => "esri".into(),
            Self::Sentinel => "sentinel".into(),
            Self::Reference => "reference".into(),
            // (The elevation itself, whatever the shading.)
            Self::Terrain(_) => "terrain".into(),
            Self::Radar(t) => format!("radar/{t}"),
            Self::Clouds(t) => format!("clouds/{t}"),
            Self::Daily(d) => format!("daily/{d}"),
        }
    }

    fn photo(self) -> bool {
        matches!(self, Self::Holo | Self::Satellite | Self::Esri | Self::Sentinel | Self::Daily(_))
    }

    /// Live layers stay in memory only (stale by the next half hour).
    fn cached_on_disk(self) -> bool {
        !matches!(self, Self::Radar(_) | Self::Clouds(_))
    }

    /// The day of a daily layer.
    pub fn day(date: NaiveDate) -> Self {
        Self::Daily((date - NaiveDate::from_ymd_opt(1970, 1, 1).expect("a date")).num_days() as i32)
    }

    fn date(self) -> Option<NaiveDate> {
        match self {
            Self::Daily(d) => NaiveDate::from_ymd_opt(1970, 1, 1).map(|e| e + chrono::Duration::days(d as i64)),
            _ => None,
        }
    }
}

/// The URLs of a tile of the layers that need no probing (several for the infrared clouds: one per
/// satellite, merged).
pub fn source_urls(key: TileKey, radar: Option<(&str, &RadarFrame)>) -> Vec<String> {
    let TileKey { layer, z, x, y } = key;
    match layer {
        TileLayer::Esri => vec![Imagery::Esri.url(z, x, y)],
        TileLayer::Sentinel => vec![Imagery::Sentinel.url(z, x, y)],
        TileLayer::Reference => vec![src::reference_url(z, x, y)],
        TileLayer::Terrain(_) => vec![src::terrarium_url(z, x, y)],
        TileLayer::Radar(t) => radar.filter(|(_, f)| f.time as u32 == t).map(|(host, f)| vec![src::radar_tile_url(host, &f.path, z, x, y)]).unwrap_or_default(),
        TileLayer::Clouds(_) => src::IR_LAYERS.iter().map(|l| src::gibs_tile_url(l, None, src::IR_MAX_LEVEL, "png", z, x, y)).collect(),
        TileLayer::Daily(_) => {
            let day = layer.date().map(|d| d.format("%Y-%m-%d").to_string());
            vec![src::gibs_tile_url(src::DAILY_LAYER, day.as_deref(), src::DAILY_MAX_LEVEL, "jpg", z, x, y)]
        }
        TileLayer::Holo | TileLayer::Satellite | TileLayer::Features | TileLayer::Labels => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub layer: TileLayer,
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    pub fn parent(self) -> Option<TileKey> {
        (self.z > 0).then(|| TileKey { layer: self.layer, z: self.z - 1, x: self.x / 2, y: self.y / 2 })
    }
}

pub enum TileSlot {
    Loading,
    Ready { tex: TextureHandle, used: u64 },
    Failed { at: Instant, forever: bool },
}

/// Which of the GIBS URL forms a layer answers to (found once by asking for its world tile).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Unknown,
    Probing,
    Works(usize),
    /// GIBS answered, but none of the forms gave a tile.
    Dead,
    /// GIBS was out of reach (asked again a minute later).
    Unreachable(Instant),
}

/// The WMTS REST forms tried in order: without a time, with "default", with an empty one; JPEG
/// layers also as ".jpg".
const TEMPLATE_FORMS: usize = 6;

fn tile_url(layer: TileLayer, form: usize, z: u8, x: u32, y: u32) -> String {
    let tms = format!("GoogleMapsCompatible_Level{}", layer.max_level());
    let ext = match (layer.photo(), form >= 3) {
        (true, false) => "jpeg",
        (true, true) => "jpg",
        (false, _) => "png",
    };
    let time = ["", "default/", "/"][form % 3];
    format!("https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/{}/default/{time}{tms}/{z}/{y}/{x}.{ext}", layer.gibs())
}

fn wms_url(layer: &str, png: bool, v111: bool) -> String {
    let format = if png { "image/png&TRANSPARENT=TRUE" } else { "image/jpeg" };
    if v111 {
        format!("https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi?SERVICE=WMS&REQUEST=GetMap&VERSION=1.1.1&LAYERS={layer}&STYLES=&SRS=EPSG:4326&BBOX=-180,-90,180,90&WIDTH={BASE_W}&HEIGHT={BASE_H}&FORMAT={format}")
    } else {
        format!("https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi?SERVICE=WMS&REQUEST=GetMap&VERSION=1.3.0&LAYERS={layer}&STYLES=&CRS=EPSG:4326&BBOX=-90,-180,90,180&WIDTH={BASE_W}&HEIGHT={BASE_H}&FORMAT={format}")
    }
}

// -------------------------------------------------------------------------------- state ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Holo,
    Satellite,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// The point in the middle of the view.
    pub lat: f64,
    pub lon: f64,
    /// Above the surface.
    pub alt: f64,
}

/// The resting view: the Atlantic with the Americas, Europe and Africa around it.
pub const HOME: Camera = Camera { lat: 30.0, lon: -20.0, alt: HOME_ALT_KM };
/// The demo's view (the reference's): North America with New York.
pub const DEMO_CAMERA: Camera = Camera { lat: 28.0, lon: -78.0, alt: HOME_ALT_KM };

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// The open: the camera comes in from `from_d` to `to_d` Earth radii.
    Intro { start: Option<f64>, from_d: f64, to_d: f64 },
    /// A great-circle flight with a zoom arc.
    Fly { start: Option<f64>, from: Camera, to: Camera, secs: f64 },
}

/// How long a flight between two views takes (1 to 1.5 s by distance).
pub fn fly_secs(from: Camera, to: Camera) -> f64 {
    let km = haversine_km((from.lat, from.lon), (to.lat, to.lon));
    let zoom = (to.alt.max(0.05) / from.alt.max(0.05)).ln().abs();
    1.0 + 0.5 * (km / 10_000.0 + zoom / 6.0).min(1.0)
}

/// The camera `t` (0..1, already eased) of the way along a flight: the great circle between the
/// targets, the altitude interpolated in log space with a lift over long distances.
pub fn fly_camera(from: Camera, to: Camera, t: f64) -> Camera {
    let (lat, lon) = great_circle((from.lat, from.lon), (to.lat, to.lon), t);
    let (a, b) = (from.alt.max(0.05).ln(), to.alt.max(0.05).ln());
    let base = (a + (b - a) * t).exp();
    let km = haversine_km((from.lat, from.lon), (to.lat, to.lon));
    let lift = (0.9 * km - from.alt.max(to.alt)).max(0.0);
    Camera { lat, lon, alt: (base + lift * 4.0 * t * (1.0 - t)).clamp(MIN_ALT_KM, MAX_ALT_KM) }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Fetch {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// One live layer: its items, when it was last asked for, and how that went.
pub struct Feed<T> {
    pub items: Vec<T>,
    pub at: Option<Instant>,
    pub fetch: Fetch,
}

impl<T> Default for Feed<T> {
    fn default() -> Self {
        Self { items: Vec::new(), at: None, fetch: Fetch::Idle }
    }
}

impl<T> Feed<T> {
    fn due(&self, every: Duration) -> bool {
        self.fetch != Fetch::Loading && self.at.is_none_or(|at| at.elapsed() >= every)
    }

    fn set(&mut self, result: Result<Vec<T>, String>) {
        self.at = Some(Instant::now());
        match result {
            Ok(items) => {
                self.items = items;
                self.fetch = Fetch::Ready;
            }
            Err(e) => self.fetch = Fetch::Failed(e),
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// One equirectangular globe texture: its image in memory (uploaded again after a close) and
/// on the GPU.
#[derive(Default)]
pub struct Base {
    pub image: Option<Arc<ColorImage>>,
    pub tex: Option<TextureHandle>,
    pub fetch: Fetch,
}

/// A texture cross-fade on the globe: the new one over the old one.
#[derive(Debug, Clone, Copy, Default)]
pub struct TexFade {
    pub current: Option<TextureId>,
    pub previous: Option<TextureId>,
    pub since: f64,
    pub secs: f64,
}

impl TexFade {
    /// Shows `id` from `now` on (fading over `secs` when it replaces another); the share of the
    /// new texture drawn over the old one.
    pub fn show(&mut self, id: TextureId, now: f64, secs: f64) -> f32 {
        if self.current != Some(id) {
            self.previous = self.current;
            self.current = Some(id);
            self.since = now;
            self.secs = if self.previous.is_some() { secs } else { 0.0 };
        }
        self.progress(now)
    }

    pub fn progress(&self, now: f64) -> f32 {
        if self.secs <= 0.0 {
            1.0
        } else {
            ((now - self.since) / self.secs).clamp(0.0, 1.0) as f32
        }
    }
}

/// A lookup cell: (lat index, lon index, cell size in hundredths of a degree).
pub type Cell = (i32, i32, u32);

pub fn cell_of(lat: f64, lon: f64, size_deg: f64) -> Cell {
    let size = size_deg.max(0.01);
    ((lat / size).floor() as i32, (wrap_lon(lon) / size).floor() as i32, (size * 100.0).round() as u32)
}

/// The middle of a cell (what is looked up: nothing finer than the cell leaves the PC).
pub fn cell_centre(cell: Cell) -> (f64, f64) {
    let size = cell.2 as f64 / 100.0;
    ((cell.0 as f64 + 0.5) * size, wrap_lon((cell.1 as f64 + 0.5) * size))
}

/// The lookup cell size for an altitude: countries from far away, towns up close.
pub fn cell_size_for(alt_km: f64) -> f64 {
    if alt_km > 3_000.0 {
        2.0
    } else if alt_km > 600.0 {
        0.5
    } else {
        0.25
    }
}

/// What the view looks at this frame (`gui_earth` tells the scheduler).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outlook {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    /// The box the map shows (None: more than the flights may ask for).
    pub bbox: Option<Bbox>,
    /// The camera has rested `REST_SECS`.
    pub resting: bool,
}

/// Everything the Earth shows and knows.
pub struct Earth {
    /// The deterministic demo (verifier, dry runs, tests): no network at all.
    pub demo: bool,
    pub look: Look,
    pub layers: Layers,
    pub cam: Camera,
    pub motion: Option<Motion>,
    pub queued: Option<Motion>,
    pub zoom_target: Option<f64>,
    pub spin: bool,
    pub marker: Option<Marker>,
    pub area: Option<AreaQuery>,
    pub quakes: Feed<Quake>,
    pub events: Feed<GeoEvent>,
    pub iss: Feed<Iss>,
    pub track: Feed<(f64, f64)>,
    pub flights: Feed<Flight>,
    pub flights_box: Option<Bbox>,
    flights_backoff: Option<Instant>,
    pub flights_note: Option<&'static str>,
    pub place: Option<Place>,
    place_cell: Option<Cell>,
    place_cache: HashMap<Cell, Option<Place>>,
    place_busy: bool,
    place_asked: Option<Instant>,
    pub weather: Option<Weather>,
    weather_cell: Option<Cell>,
    weather_cache: HashMap<Cell, (Option<Weather>, Instant)>,
    weather_busy: bool,
    weather_asked: Option<Instant>,
    pub holo: Base,
    pub sat: Base,
    holo_mask: Option<Arc<HoloMask>>,
    pub holo_hue: f32,
    recolouring: bool,
    hue_checked: Option<Instant>,
    pub procedural: Option<TextureHandle>,
    /// Textures replaced while the globe still fades from them (dropped once it has).
    pub retired: Vec<TextureHandle>,
    pub tiles: HashMap<TileKey, TileSlot>,
    tiles_in_flight: usize,
    /// Decoded tiles in memory (least recently used go first) and this frame's uploads.
    pixels: HashMap<TileKey, (Arc<ColorImage>, u64)>,
    uploaded: usize,
    templates: HashMap<TileLayer, Template>,
    templates_loaded: bool,
    /// Frames drawn (the tile LRU's clock).
    pub frame: u64,
    /// egui time of the last frame.
    pub last_time: Option<f64>,
    /// egui time since which the camera has not moved.
    pub still_since: f64,
    pub globe_fade: TexFade,
    pub look_changed: f64,
    trimmed: bool,
    next_area: u64,
    /// What the block says it shows (the board's search and "copy").
    pub described: String,
    /// The photo layer.
    pub imagery: Imagery,
    pub overlays: Overlays,
    /// A day of NASA's daily imagery instead of today's.
    pub historic: Option<NaiveDate>,
    /// RainViewer's frames (oldest first) and its tile host.
    pub radar: Feed<RadarFrame>,
    pub radar_host: String,
    /// egui time the radar loop started.
    pub radar_since: f64,
    pub peaks: Feed<Peak>,
    pub peaks_box: Option<Bbox>,
    peaks_host: usize,
    peaks_backoff: Option<Instant>,
    /// Sir's position ("where am I?") and the egui time it was put on the map.
    pub me: Option<crate::earth_locate::Fix>,
    pub me_since: f64,
    pub tour: Option<crate::earth_tour::Tour>,
    /// A drag's speed after the release (points a second), dying away.
    pub inertia: Option<(f32, f32)>,
    /// The globe on the GPU: how its base textures are coming along, and its clipmaps.
    gpu_fetch: [Fetch; 5],
    gpu_busy: bool,
    gpu_historic: Option<NaiveDate>,
    pub clips: [ClipFeed; 2],
}

/// One of the globe's clipmaps on the CPU side: the window it wants, the layers it composites
/// (bottom first) and what each of its 64 slots holds.
#[derive(Default)]
pub struct ClipFeed {
    pub clip: Option<Clip>,
    pub layers: Vec<TileLayer>,
    pub mix: f32,
    slots: Vec<ClipSlot>,
    dirty: bool,
}

#[derive(Default, Clone)]
struct ClipSlot {
    tile: Option<(u32, u32)>,
    parts: HashMap<TileLayer, Arc<ColorImage>>,
    sent: bool,
}

impl ClipFeed {
    /// The clipmap's wish for this frame; slots that no longer fit it are emptied.
    fn want(&mut self, clip: Option<Clip>, layers: Vec<TileLayer>, mix: f32) {
        self.mix = mix;
        if self.slots.len() != (gl::CLIP_N * gl::CLIP_N) as usize {
            self.slots = vec![ClipSlot::default(); (gl::CLIP_N * gl::CLIP_N) as usize];
        }
        if clip == self.clip && layers == self.layers {
            return;
        }
        let z_changed = clip.map(|c| c.z) != self.clip.map(|c| c.z);
        for slot in &mut self.slots {
            let keep = match (clip, slot.tile) {
                (Some(c), Some((x, y))) => !z_changed && c.contains(x, y),
                _ => false,
            };
            if !keep {
                *slot = ClipSlot::default();
            } else {
                let before = slot.parts.len();
                slot.parts.retain(|layer, _| layers.contains(layer));
                if slot.parts.len() != before {
                    slot.sent = false;
                }
            }
        }
        self.clip = clip;
        self.layers = layers;
        self.dirty = true;
    }

    /// Which slots show their tile (the bottom layer is there for the imagery; anything for the
    /// overlays), as the GPU's slot map.
    fn valid(&self, need_bottom: bool) -> Vec<u8> {
        self.slots
            .iter()
            .map(|s| {
                let ok = s.sent && s.tile.is_some() && (!need_bottom || self.layers.first().is_some_and(|l| s.parts.contains_key(l)));
                if ok { 255 } else { 0 }
            })
            .collect()
    }

    /// The slot's layers composited (premultiplied "over", bottom first) as 256 x 256 RGBA.
    fn composite(&self, slot: &ClipSlot) -> Option<Vec<u8>> {
        let side = gl::TILE_PX as usize;
        let mut out = vec![0u8; side * side * 4];
        let mut any = false;
        for layer in &self.layers {
            let Some(img) = slot.parts.get(layer) else { continue };
            if img.size != [side, side] {
                continue;
            }
            any = true;
            for (o, p) in out.chunks_exact_mut(4).zip(&img.pixels) {
                let a = p.a() as u32;
                if a == 255 {
                    o.copy_from_slice(&p.to_array());
                } else if a > 0 {
                    let keep = 255 - a;
                    let c = p.to_array();
                    for k in 0..4 {
                        o[k] = (c[k] as u32 + (o[k] as u32 * keep + 127) / 255).min(255) as u8;
                    }
                }
            }
        }
        any.then_some(out)
    }
}

impl Default for Earth {
    fn default() -> Self {
        Self::new()
    }
}

impl Earth {
    pub fn new() -> Self {
        Self {
            demo: false,
            look: Look::Satellite,
            layers: Layers::DEFAULT,
            cam: HOME,
            motion: None,
            queued: None,
            zoom_target: None,
            spin: false,
            marker: None,
            area: None,
            quakes: Feed::default(),
            events: Feed::default(),
            iss: Feed::default(),
            track: Feed::default(),
            flights: Feed::default(),
            flights_box: None,
            flights_backoff: None,
            flights_note: None,
            place: None,
            place_cell: None,
            place_cache: HashMap::new(),
            place_busy: false,
            place_asked: None,
            weather: None,
            weather_cell: None,
            weather_cache: HashMap::new(),
            weather_busy: false,
            weather_asked: None,
            holo: Base::default(),
            sat: Base::default(),
            holo_mask: None,
            holo_hue: 206.0,
            recolouring: false,
            hue_checked: None,
            procedural: None,
            retired: Vec::new(),
            tiles: HashMap::new(),
            tiles_in_flight: 0,
            pixels: HashMap::new(),
            uploaded: 0,
            templates: HashMap::new(),
            templates_loaded: false,
            frame: 0,
            last_time: None,
            still_since: 0.0,
            globe_fade: TexFade::default(),
            look_changed: f64::NEG_INFINITY,
            trimmed: false,
            next_area: 1,
            described: String::new(),
            imagery: Imagery::Esri,
            overlays: Overlays::DEFAULT,
            historic: None,
            radar: Feed::default(),
            radar_host: String::new(),
            radar_since: 0.0,
            peaks: Feed::default(),
            peaks_box: None,
            peaks_host: 0,
            peaks_backoff: None,
            me: None,
            me_since: 0.0,
            tour: None,
            inertia: None,
            gpu_fetch: Default::default(),
            gpu_busy: false,
            gpu_historic: None,
            clips: Default::default(),
        }
    }

    /// The deterministic demo, settled (no fly-in): what the offscreen tests draw.
    #[cfg(test)]
    pub fn demo() -> Self {
        let mut e = Self::new();
        e.load_demo();
        e
    }

    /// Fresh for an open: home view with the fly-in, the photo look, the default layers.
    fn reset_for_open(&mut self, offline: bool) {
        self.look = Look::Satellite;
        self.layers = Layers::DEFAULT;
        self.overlays = Overlays::DEFAULT;
        self.historic = None;
        self.tour = None;
        self.inertia = None;
        self.peaks_box = None;
        self.marker = None;
        self.area = None;
        self.spin = false;
        self.zoom_target = None;
        self.queued = None;
        self.place = None;
        self.place_cell = None;
        self.weather = None;
        self.weather_cell = None;
        crate::earth_layers::reset(); // live layers (C1)
        if offline {
            self.load_demo();
        } else if self.demo {
            self.leave_demo();
        }
        let home = if self.demo { DEMO_CAMERA } else { HOME };
        let to_d = distance_of(home.alt);
        let from_d = to_d * INTRO_DISTANCE;
        self.cam = Camera { alt: (from_d - 1.0) * R_KM, ..home };
        self.motion = Some(Motion::Intro { start: None, from_d, to_d });
    }

    /// The demo's data: seven quakes (M4.5-5.2), three storms, a few fires over the plains, the
    /// ISS with its track, New York marked, flights over the north-east.
    pub fn load_demo(&mut self) {
        let now = Utc::now().timestamp_millis();
        self.demo = true;
        crate::earth_layers::load_demo(); // live layers (C1)
        self.cam = DEMO_CAMERA;
        self.motion = None;
        self.queued = None;
        let quake = |id: &str, mag: f64, place: &str, hours: i64, lat: f64, lon: f64| Quake { id: id.into(), mag, place: place.into(), time_ms: now - hours * 3_600_000, lat, lon, depth_km: 10.0 };
        self.quakes.items = vec![
            quake("demo1", 5.2, "38 km NNE of Popayan, Colombia", 5, 2.75, -76.45),
            quake("demo2", 5.0, "84 km SE of Kodiak, Alaska", 19, 57.3, -151.5),
            quake("demo3", 4.9, "Reykjanes Ridge", 30, 59.8, -29.6),
            quake("demo4", 4.7, "61 km W of Pisco, Peru", 44, -13.8, -76.8),
            quake("demo5", 4.6, "22 km S of Arica, Chile", 52, -18.7, -70.3),
            quake("demo6", 4.6, "45 km NW of Haines, Alaska", 70, 59.6, -136.2),
            quake("demo7", 4.5, "Galapagos Triple Junction region", 96, 1.9, -101.8),
        ];
        let event = |id: &str, kind: EventKind, title: &str, label: &str, lat: f64, lon: f64| GeoEvent { id: id.into(), kind, title: title.into(), label: label.into(), lat, lon };
        let mut events = vec![
            event("demo-s1", EventKind::Storm, "Tropical Storm Odalys", "ODALYS-26", 19.5, -112.0),
            event("demo-s2", EventKind::Storm, "Tropical Storm Polo", "POLO-26", 15.2, -101.0),
            event("demo-s3", EventKind::Storm, "Hurricane Fay", "FAY-26", 33.0, -52.0),
        ];
        for (i, (lat, lon)) in [(34.6, -98.4), (35.1, -97.2), (33.9, -99.1), (32.4, -97.8), (31.6, -99.9), (35.6, -99.6)].into_iter().enumerate() {
            events.push(event(&format!("demo-f{i}"), EventKind::Fire, &format!("Prairie Fire {}, Oklahoma", i + 1), "", lat, lon));
        }
        self.events.items = events;
        let iss = Iss { lat: 12.0, lon: -96.0, alt_km: 418.0, speed_kmh: 27_580.0, at: now / 1000 };
        self.track.items = iss_ground_track(iss.lat, iss.lon, true, &(-9..=9).map(|k| k as f64 * 5.0).collect::<Vec<_>>());
        self.iss.items = vec![iss];
        let mut flights = Vec::new();
        for i in 0..36 {
            let k = i as f64;
            let (lat, lon) = (39.2 + (k * 0.618).fract() * 3.4, -77.8 + (k * 0.414).fract() * 7.0);
            flights.push(Flight { callsign: format!("DEMO{}", 100 + i * 7), lat, lon, alt_m: Some(3_000.0 + (k * 977.0) % 9_000.0), speed_kmh: Some(420.0 + (k * 53.0) % 480.0), track: (k * 47.0) % 360.0, at: now / 1000, kind: None });
        }
        self.flights.items = flights;
        for feed in [&mut self.quakes.fetch, &mut self.events.fetch, &mut self.iss.fetch, &mut self.track.fetch, &mut self.flights.fetch] {
            *feed = Fetch::Ready;
        }
        let at = Some(Instant::now());
        self.quakes.at = at;
        self.events.at = at;
        self.iss.at = at;
        self.track.at = at;
        self.flights.at = at;
        self.flights_box = Some(Bbox::around(40.7, -74.0, 6.0, 9.0));
        self.marker = Some(Marker { name: "New York".into(), lat: 40.7128, lon: -74.006 });
        self.place = Some(Place { country: "United States".into(), region: "New York".into(), city: "New York".into(), water: String::new() });
        self.weather = Some(Weather { temp: 18.1, feels: 19.4, humidity: Some(94.0), wind_kmh: Some(8.0), wind_from: Some(45.0), code: Some(3), offset_secs: -4 * 3600, sunrise: Some("06:58".into()), sunset: Some("19:10".into()) });
        self.holo.fetch = Fetch::Failed("demo".into());
        self.sat.fetch = Fetch::Failed("demo".into());
        // (The procedural globe in the theme's hue.)
        let hue = theme_hue();
        if (hue - self.holo_hue).abs() > 0.5 {
            self.holo_hue = hue;
            self.retired.extend(self.procedural.take());
        }
    }

    /// From the demo back to the live Earth: its data goes, the real one is fetched.
    fn leave_demo(&mut self) {
        self.demo = false;
        crate::earth_layers::leave_demo(); // live layers (C1)
        for feed in [&mut self.quakes.fetch, &mut self.events.fetch, &mut self.iss.fetch, &mut self.track.fetch, &mut self.flights.fetch] {
            *feed = Fetch::Idle;
        }
        self.quakes.reset();
        self.events.reset();
        self.iss.reset();
        self.track.reset();
        self.flights.reset();
        self.flights_box = None;
        self.holo.fetch = if self.holo.image.is_some() { Fetch::Ready } else { Fetch::Idle };
        self.sat.fetch = if self.sat.image.is_some() { Fetch::Ready } else { Fetch::Idle };
    }

    /// Frees the GPU (a closed Earth keeps its images in memory only; the globe's textures come
    /// back from the disk cache on the next open).
    fn release_gpu(&mut self) {
        self.holo.tex = None;
        self.sat.tex = None;
        self.procedural = None;
        self.retired.clear();
        self.tiles.clear();
        self.pixels.clear();
        self.globe_fade = TexFade::default();
        gl::request_release();
        self.gpu_fetch = Default::default();
        self.gpu_historic = None;
        self.clips = Default::default();
    }

    /// Where the camera is headed (the end of its flight), or where it is.
    pub fn target(&self) -> Camera {
        match self.queued.or(self.motion) {
            Some(Motion::Fly { to, .. }) => to,
            Some(Motion::Intro { to_d, .. }) => Camera { alt: (to_d - 1.0) * R_KM, ..self.cam },
            None => Camera { alt: self.zoom_target.unwrap_or(self.cam.alt), ..self.cam },
        }
    }

    pub fn set_look(&mut self, look: Look) {
        if self.look != look {
            self.look = look;
            self.look_changed = f64::NAN;
        }
    }

    /// Flies the camera (after the fly-in when that still runs).
    pub fn fly_to(&mut self, to: Camera) {
        let to = Camera { lat: to.lat.clamp(-85.0, 85.0), lon: wrap_lon(to.lon), alt: to.alt.clamp(MIN_ALT_KM, MAX_ALT_KM) };
        let fly = Motion::Fly { start: None, from: self.cam, to, secs: 1.2 };
        self.zoom_target = None;
        self.spin = false;
        self.inertia = None;
        match self.motion {
            Some(Motion::Intro { .. }) => self.queued = Some(fly),
            _ => self.motion = Some(fly),
        }
    }

    pub fn go_home(&mut self) {
        let home = if self.demo { DEMO_CAMERA } else { HOME };
        self.fly_to(home);
    }

    /// Mouse wheel and "zoom in": the altitude target times `factor`, smoothed.
    pub fn zoom_by(&mut self, factor: f64) {
        if let Some(Motion::Fly { .. }) = self.motion {
            self.motion = None;
            self.queued = None;
        }
        let from = self.zoom_target.unwrap_or(self.cam.alt);
        self.zoom_target = Some((from * factor).clamp(MIN_ALT_KM, MAX_ALT_KM));
    }

    /// A drag of the map stops whatever moved the camera (a tour included).
    pub fn grab(&mut self) {
        self.spin = false;
        self.tour = None;
        self.inertia = None;
        if let Some(Motion::Fly { .. }) = self.motion {
            self.motion = None;
            self.queued = None;
        }
    }

    /// The camera at egui time `now`; true while it moves.
    pub fn step(&mut self, now: f64, dt: f64) -> bool {
        let mut moving = false;
        if let Some(mut motion) = self.motion {
            moving = true;
            let done = match &mut motion {
                Motion::Intro { start, from_d, to_d } => {
                    let t0 = *start.get_or_insert(now);
                    let t = ((now - t0) / INTRO_SECS).clamp(0.0, 1.0);
                    let d = *from_d + (*to_d - *from_d) * smootherstep(t);
                    self.cam.alt = (d - 1.0) * R_KM;
                    t >= 1.0
                }
                Motion::Fly { start, from, to, secs } => {
                    if start.is_none() {
                        *start = Some(now);
                        *from = self.cam;
                        *secs = fly_secs(*from, *to);
                    }
                    let t = ((now - start.unwrap_or(now)) / *secs).clamp(0.0, 1.0);
                    self.cam = fly_camera(*from, *to, smootherstep(t));
                    t >= 1.0
                }
            };
            self.motion = if done { self.queued.take() } else { Some(motion) };
        }
        if let Some(target) = self.zoom_target {
            moving = true;
            let (cur, goal) = (self.cam.alt.ln(), target.ln());
            let next = smooth_towards(cur, goal, dt, ZOOM_TAU_SECS);
            if (next - goal).abs() < 0.002 {
                self.cam.alt = target;
                self.zoom_target = None;
            } else {
                self.cam.alt = next.exp();
            }
        }
        if self.spin && self.motion.is_none() {
            moving = true;
            self.cam.lon = wrap_lon(self.cam.lon - SPIN_DEG_PER_SEC * dt);
        }
        if self.tour.is_some() {
            self.step_tour(now);
            moving = true;
        }
        if moving {
            self.still_since = now;
        }
        moving
    }

    /// The world tour's frame: on to the next stop, its line, or home.
    fn step_tour(&mut self, now: f64) {
        use crate::earth_tour::TourStep;
        let flying = self.motion.is_some() || self.queued.is_some();
        let Some(tour) = self.tour.as_mut() else { return };
        match tour.step(now, flying) {
            TourStep::Wait => {}
            TourStep::FlyTo(cam) => {
                let tour = self.tour.take();
                self.fly_to(cam);
                self.tour = tour;
            }
            TourStep::Say(line) => {
                if !self.demo && !offline() {
                    crate::announcements::say(line);
                }
            }
            TourStep::Done => {
                self.tour = None;
                if !self.demo && !offline() {
                    crate::announcements::say(crate::earth_tour::CLOSING);
                }
                self.go_home();
            }
        }
    }

    /// The radar frame shown now: the latest, or the loop's.
    pub fn radar_frame(&self, now: f64) -> Option<&RadarFrame> {
        let frames = &self.radar.items;
        let last = frames.len().checked_sub(1)?;
        if !self.overlays.animate || last == 0 {
            return frames.last();
        }
        let cycle = last as f64 * RADAR_STEP_SECS + RADAR_HOLD_SECS;
        let t = (now - self.radar_since).rem_euclid(cycle);
        frames.get(((t / RADAR_STEP_SECS) as usize).min(last))
    }

    /// The photo layer under everything: the day asked for, or the imagery chosen.
    pub fn imagery_layer(&self) -> TileLayer {
        match (self.look, self.historic) {
            (Look::Holo, _) => TileLayer::Holo,
            (_, Some(date)) => TileLayer::day(date),
            _ => TileLayer::of(self.imagery),
        }
    }

    /// The overlays over the imagery, bottom first, at time `now` (the radar's frame, the half
    /// hour of the clouds).
    pub fn overlay_layers(&self, now: f64) -> Vec<TileLayer> {
        let mut layers = Vec::new();
        if self.overlays.clouds && self.historic.is_none() {
            layers.push(TileLayer::Clouds((Utc::now().timestamp() / CLOUD_SLOT_SECS) as u32));
        }
        if self.overlays.radar && self.historic.is_none() {
            if let Some(frame) = self.radar_frame(now) {
                layers.push(TileLayer::Radar(frame.time as u32));
            }
        }
        if self.overlays.labels || self.look == Look::Holo {
            layers.push(if self.look == Look::Holo { TileLayer::Labels } else { TileLayer::Reference });
        }
        layers
    }

    /// The shaded terrain over the imagery (when it is on).
    pub fn terrain_layer(&self) -> Option<TileLayer> {
        self.overlays.terrain.then_some(TileLayer::Terrain(if self.overlays.exaggerate { 25 } else { 10 }))
    }

    /// The point in view's UTC offset: the weather's (a real time zone) or the nautical one.
    pub fn offset_secs(&self) -> i32 {
        match (&self.weather, self.weather_cell) {
            (Some(w), Some(cell)) if cell_of(self.cam.lat, self.cam.lon, cell.2 as f64 / 100.0) == cell => w.offset_secs,
            (Some(w), None) if self.demo => w.offset_secs,
            _ => estimate_offset_secs(self.cam.lon),
        }
    }

    /// The area query's results from what is loaded (the demo, a dry run).
    pub fn local_area(&self, centre: (f64, f64), radius_km: f64) -> Vec<Quake> {
        let mut found: Vec<Quake> = self.quakes.items.iter().filter(|q| haversine_km(centre, (q.lat, q.lon)) <= radius_km).cloned().collect();
        found.sort_by(|a, b| b.mag.total_cmp(&a.mag));
        found
    }

    /// Starts an area query: the circle, the drawer, the quakes on and the flights off, the
    /// camera over the area with the whole circle in view. Its id.
    pub fn begin_area(&mut self, centre: (f64, f64), radius_km: f64) -> u64 {
        let id = self.next_area;
        self.next_area += 1;
        self.area = Some(AreaQuery { id, lat: centre.0, lon: centre.1, radius_km, days: AREA_DAYS, asked: Local::now().format("%H:%M:%S").to_string(), results: None, failed: None });
        self.layers.quakes = true;
        self.layers.flights = false;
        self.fly_to(Camera { lat: centre.0, lon: centre.1, alt: area_altitude(radius_km) });
        id
    }

    fn area_result(&mut self, id: u64, result: Result<Vec<Quake>, String>) {
        if let Some(area) = self.area.as_mut().filter(|a| a.id == id) {
            match result {
                Ok(mut list) => {
                    list.sort_by(|a, b| b.mag.total_cmp(&a.mag));
                    area.results = Some(list);
                    area.failed = None;
                }
                Err(e) => area.failed = Some(e),
            }
        }
    }

    // ----------------------------------------------------------------- arrivals, textures ----

    /// What the background brought since the last frame; textures are uploaded here (a few
    /// tiles per frame).
    pub fn absorb(&mut self, ctx: &egui::Context, now: f64) {
        if !self.demo {
            let arrivals = std::mem::take(&mut *crate::crash_guard::lock(&INBOX));
            for arrival in arrivals {
                self.arrive(arrival);
            }
        }
        // The globe textures (again after a close).
        for (base, name) in [(&mut self.holo, "earth_holo"), (&mut self.sat, "earth_satellite")] {
            if base.tex.is_none() {
                if let Some(image) = &base.image {
                    base.tex = Some(ctx.load_texture(name, (**image).clone(), globe_texture_options()));
                }
            }
        }
        if self.procedural.is_none() {
            self.procedural = Some(ctx.load_texture("earth_procedural", procedural_texture(1024, 512, self.holo_hue), globe_texture_options()));
        }
        if self.uploaded >= UPLOADS_PER_FRAME {
            ctx.request_repaint();
        }
        self.uploaded = 0;
        // Decoded tiles past the memory budget go, least recently used first.
        if self.pixels.len() > PIXEL_CACHE {
            let mut ages: Vec<(u64, TileKey)> = self.pixels.iter().map(|(k, (_, used))| (*used, *k)).collect();
            ages.sort_by_key(|a| a.0);
            for (_, key) in ages.into_iter().take(self.pixels.len() - PIXEL_CACHE) {
                self.pixels.remove(&key);
            }
        }
        // Least recently drawn tiles leave the GPU.
        let ready = self.tiles.values().filter(|s| matches!(s, TileSlot::Ready { .. })).count();
        if ready > TILE_TEXTURES {
            let mut ages: Vec<(u64, TileKey)> = self.tiles.iter().filter_map(|(k, s)| match s {
                TileSlot::Ready { used, .. } => Some((*used, *k)),
                _ => None,
            }).collect();
            ages.sort_by_key(|a| a.0);
            for (used, key) in ages.into_iter().take(ready - TILE_TEXTURES) {
                if used < self.frame {
                    self.tiles.remove(&key);
                }
            }
        }
        if self.look_changed.is_nan() {
            self.look_changed = now;
        }
        if self.me_since.is_nan() {
            self.me_since = now;
        }
        if self.radar_since.is_nan() {
            self.radar_since = now;
        }
    }

    fn arrive(&mut self, arrival: Arrival) {
        match arrival {
            Arrival::Quakes(r) => self.quakes.set(r),
            Arrival::Events(r) => self.events.set(r),
            Arrival::Iss(r) => {
                let r = r.map(|now| {
                    let mut list = self.iss.items.last().copied().into_iter().collect::<Vec<_>>();
                    list.push(now);
                    list
                });
                self.iss.set(r);
                if self.iss.items.len() > 2 {
                    self.iss.items.remove(0);
                }
                if matches!(self.iss.fetch, Fetch::Failed(_)) {
                    // Back off: try again in half a minute.
                    self.iss.at = Some(Instant::now() + Duration::from_secs(25));
                }
            }
            Arrival::Track(r) => self.track.set(r),
            Arrival::Flights(bbox, r, backoff) => {
                self.flights_box = Some(bbox);
                self.flights.set(r);
                self.flights_backoff = backoff.map(|d| Instant::now() + d);
                self.flights_note = backoff.map(|_| "FLIGHTS RATE-LIMITED");
            }
            Arrival::Place(cell, place) => {
                self.place_busy = false;
                if self.place_cache.len() > 256 {
                    self.place_cache.clear();
                }
                self.place_cache.insert(cell, place.clone());
                if self.place_cell == Some(cell) {
                    self.place = place;
                }
            }
            Arrival::Weather(cell, weather) => {
                self.weather_busy = false;
                if self.weather_cache.len() > 256 {
                    self.weather_cache.clear();
                }
                self.weather_cache.insert(cell, (weather.clone(), Instant::now()));
                if self.weather_cell == Some(cell) {
                    self.weather = weather;
                }
            }
            Arrival::Base(look, result) => {
                if result.is_ok() {
                    let old = match look {
                        Look::Holo => self.holo.tex.take(),
                        Look::Satellite => self.sat.tex.take(),
                    };
                    self.retired.extend(old);
                }
                let base = match look {
                    Look::Holo => &mut self.holo,
                    Look::Satellite => &mut self.sat,
                };
                match result {
                    Ok(made) => {
                        base.image = Some(made.image);
                        base.tex = None;
                        base.fetch = Fetch::Ready;
                        if let Some(mask) = made.mask {
                            self.holo_mask = Some(mask);
                        }
                        if let Some(hue) = made.hue {
                            self.holo_hue = hue;
                        }
                        self.recolouring = false;
                    }
                    Err(e) => {
                        warn!("[EARTH] No {look:?} imagery: {e}");
                        base.fetch = Fetch::Failed(e);
                        self.recolouring = false;
                    }
                }
            }
            Arrival::Probe(layer, form) => {
                let template = match form {
                    Ok(form) => Template::Works(form),
                    Err(answered) => {
                        // (A probe that found a form brings its world tile, which counts it out.)
                        self.tiles_in_flight = self.tiles_in_flight.saturating_sub(1);
                        if answered { Template::Dead } else { Template::Unreachable(Instant::now()) }
                    }
                };
                self.templates.insert(layer, template);
                save_templates(&self.templates);
            }
            Arrival::Tile(key, result) => {
                self.tiles_in_flight = self.tiles_in_flight.saturating_sub(1);
                match result {
                    Ok(image) => {
                        self.tiles.remove(&key);
                        let image = Arc::new(image);
                        self.pixels.insert(key, (image.clone(), self.frame));
                        self.feed_clips(key, &image);
                    }
                    Err(forever) => {
                        self.tiles.insert(key, TileSlot::Failed { at: Instant::now(), forever });
                    }
                }
            }
            Arrival::Area(id, result) => self.area_result(id, result),
            Arrival::GpuBase(slot, result) => {
                self.gpu_busy = false;
                let i = slot as usize;
                self.gpu_fetch[i] = match result {
                    Ok(()) => Fetch::Ready,
                    Err(e) => {
                        warn!("[EARTH] No {slot:?} texture for the globe: {e}");
                        Fetch::Failed(e)
                    }
                };
            }
            Arrival::Radar(result) => match result {
                Ok(frames) => {
                    self.radar_host = frames.host;
                    self.radar.set(Ok(frames.frames));
                }
                Err(e) => self.radar.set(Err(e)),
            },
            Arrival::Peaks(bbox, result, host) => {
                if result.is_err() {
                    self.peaks_host = (host + 1) % src::OVERPASS_HOSTS.len();
                    self.peaks_backoff = Some(Instant::now() + Duration::from_secs(60));
                } else {
                    self.peaks_box = Some(bbox);
                }
                self.peaks.set(result);
            }
        }
    }

    /// A tile's texture when it is on the GPU (marked as drawn this frame); one decoded in memory
    /// goes up now (a few a frame).
    pub fn tile(&mut self, ctx: &egui::Context, key: TileKey) -> Option<TextureId> {
        if let Some(TileSlot::Ready { tex, used }) = self.tiles.get_mut(&key) {
            *used = self.frame;
            return Some(tex.id());
        }
        if self.uploaded >= UPLOADS_PER_FRAME {
            return None;
        }
        let (image, used) = self.pixels.get_mut(&key)?;
        *used = self.frame;
        let image = (**image).clone();
        self.uploaded += 1;
        let tex = ctx.load_texture(format!("earth_tile_{:?}_{}_{}_{}", key.layer, key.z, key.x, key.y), image, TextureOptions::LINEAR);
        let id = tex.id();
        self.tiles.insert(key, TileSlot::Ready { tex, used: self.frame });
        Some(id)
    }

    /// Asks for a tile that is not there yet (in the background, a few at a time).
    pub fn request_tile(&mut self, key: TileKey, rt: &Handle) {
        if self.demo || self.tiles_in_flight >= TILES_IN_FLIGHT || self.pixels.contains_key(&key) {
            return;
        }
        match self.tiles.get(&key) {
            Some(TileSlot::Ready { .. } | TileSlot::Loading) => return,
            Some(TileSlot::Failed { at, forever }) if *forever || at.elapsed() < Duration::from_secs(30) => return,
            _ => {}
        }
        let hue = self.holo_hue;
        if !key.layer.probed() {
            let radar = self.radar.items.iter().find(|f| matches!(key.layer, TileLayer::Radar(t) if f.time as u32 == t)).map(|f| (self.radar_host.as_str(), f));
            let urls = source_urls(key, radar);
            if urls.is_empty() {
                return;
            }
            self.tiles.insert(key, TileSlot::Loading);
            self.tiles_in_flight += 1;
            rt.spawn(async move {
                let result = fetch_source_tile(key, urls, hue).await;
                deliver(Arrival::Tile(key, result));
            });
            return;
        }
        if !self.templates_loaded {
            self.templates_loaded = true;
            self.templates.extend(load_templates());
        }
        let form = match self.templates.get(&key.layer).copied().unwrap_or(Template::Unknown) {
            Template::Works(form) => form,
            Template::Dead | Template::Probing => return,
            Template::Unreachable(at) if at.elapsed() < Duration::from_secs(60) => return,
            Template::Unknown | Template::Unreachable(_) => {
                self.templates.insert(key.layer, Template::Probing);
                self.tiles_in_flight += 1;
                let layer = key.layer;
                rt.spawn(async move {
                    let form = probe_layer(layer, hue).await;
                    deliver(Arrival::Probe(layer, form));
                });
                return;
            }
        };
        self.tiles.insert(key, TileSlot::Loading);
        self.tiles_in_flight += 1;
        rt.spawn(async move {
            let result = fetch_tile(key, form, hue).await;
            deliver(Arrival::Tile(key, result));
        });
    }

    // ------------------------------------------------------------------ the GPU globe ----

    /// A decoded tile the globe's clipmaps wait for goes into its slot.
    fn feed_clips(&mut self, key: TileKey, image: &Arc<ColorImage>) {
        for feed in self.clips.iter_mut() {
            let Some(clip) = feed.clip else { continue };
            if key.z != clip.z || !feed.layers.contains(&key.layer) || !clip.contains(key.x, key.y) {
                continue;
            }
            let i = Clip::slot(key.x, key.y);
            let Some(slot) = feed.slots.get_mut(i) else { continue };
            if slot.tile != Some((key.x, key.y)) {
                *slot = ClipSlot { tile: Some((key.x, key.y)), ..ClipSlot::default() };
            }
            slot.parts.insert(key.layer, image.clone());
            slot.sent = false;
            feed.dirty = true;
        }
    }

    /// The globe's clipmaps for this frame: `detail` (the imagery up close, the terrain shading
    /// on it) and `overlay` (clouds, radar, labels) as windows of level `z` around the view; the
    /// tiles they lack are asked for, the slots that changed go to the GPU. What the shader
    /// should draw of each.
    pub fn update_clips(&mut self, rt: &Handle, view: (f64, f64), detail: Option<(u8, f32)>, overlay_z: Option<u8>, now: f64) -> [ClipUse; 2] {
        let mut detail_layers = vec![self.imagery_layer()];
        detail_layers.extend(self.terrain_layer());
        let overlay_layers = self.overlay_layers(now);
        let detail_clip = detail.map(|(z, _)| Clip::around(view.0, view.1, z.min(detail_layers[0].max_level())));
        let overlay_clip = overlay_z.filter(|_| !overlay_layers.is_empty()).map(|z| Clip::around(view.0, view.1, overlay_layers.iter().map(|l| l.max_level()).fold(z, u8::min)));
        self.clips[0].want(detail_clip, detail_layers, detail.map_or(0.0, |d| d.1));
        self.clips[1].want(overlay_clip, overlay_layers, 1.0);
        for k in 0..2 {
            let Some(clip) = self.clips[k].clip else { continue };
            let layers = self.clips[k].layers.clone();
            for (x, y) in clip.tiles() {
                let i = Clip::slot(x, y);
                for layer in &layers {
                    if self.clips[k].slots[i].tile == Some((x, y)) && self.clips[k].slots[i].parts.contains_key(layer) {
                        continue;
                    }
                    let key = TileKey { layer: *layer, z: clip.z, x, y };
                    match self.pixels.get_mut(&key) {
                        Some((image, used)) => {
                            *used = self.frame;
                            let image = image.clone();
                            self.feed_clips(key, &image);
                        }
                        None => self.request_tile(key, rt),
                    }
                }
            }
            // The changed slots go up (the GPU side takes a few a frame).
            if self.clips[k].dirty {
                self.clips[k].dirty = false;
                let atlas = if k == 0 { Atlas::Detail } else { Atlas::Overlay };
                let feed = &mut self.clips[k];
                for i in 0..feed.slots.len() {
                    if feed.slots[i].sent || feed.slots[i].tile.is_none() {
                        continue;
                    }
                    if let Some(rgba) = feed.composite(&feed.slots[i]) {
                        gl::submit_tile(atlas, i, rgba);
                    }
                    feed.slots[i].sent = true;
                }
                gl::submit_valid(atlas, feed.valid(k == 0));
            }
        }
        [ClipUse { clip: self.clips[0].clip, mix: self.clips[0].mix }, ClipUse { clip: self.clips[1].clip, mix: self.clips[1].mix }]
    }

    // -------------------------------------------------------------------------- schedule ----

    /// Starts whatever is due: imagery, the live layers, the place in view and its weather.
    pub fn schedule(&mut self, rt: &Handle, view: &Outlook) {
        crate::earth_layers::schedule(rt, self.demo); // live layers (C1)
        if self.demo {
            return;
        }
        if !self.trimmed {
            self.trimmed = true;
            rt.spawn_blocking(|| trim_cache(Path::new(CACHE_DIR), CACHE_LIMIT, CACHE_TRIM_TO));
        }
        // The globe's imagery: on the GPU its own textures; drawn on the CPU (no GL, or not yet
        // known after a few frames) the 2048 px ones, the look in view first.
        self.schedule_gpu(rt);
        let cpu = gl::failed() || (!gl::proven() && self.frame > 30);
        let holo_first = self.look == Look::Holo;
        if cpu && self.holo.fetch == Fetch::Idle && (holo_first || self.sat.fetch != Fetch::Loading) {
            self.holo.fetch = Fetch::Loading;
            let hue = theme_hue();
            rt.spawn(async move { deliver(Arrival::Base(Look::Holo, fetch_holo_base(hue).await)) });
        }
        if cpu && self.sat.fetch == Fetch::Idle && (!holo_first || self.holo.fetch != Fetch::Loading) {
            self.sat.fetch = Fetch::Loading;
            rt.spawn(async move { deliver(Arrival::Base(Look::Satellite, fetch_satellite_base().await)) });
        }
        // The radar's frames while it shows; the peaks over a resting view of the terrain.
        if self.overlays.radar && self.radar.due(RADAR_EVERY) {
            self.radar.fetch = Fetch::Loading;
            rt.spawn(async { deliver(Arrival::Radar(fetch_radar().await)) });
        }
        if self.overlays.terrain && view.resting {
            self.schedule_peaks(rt, view);
        }
        // A new theme recolours the hologram (and its tiles).
        if self.hue_checked.is_none_or(|at| at.elapsed() > Duration::from_millis(700)) {
            self.hue_checked = Some(Instant::now());
            let hue = theme_hue();
            let off = ((hue - self.holo_hue + 540.0).rem_euclid(360.0) - 180.0).abs();
            if off > 4.0 {
                self.tiles.retain(|k, _| k.layer != TileLayer::Holo);
                self.retired.extend(self.procedural.take());
                match (self.holo_mask.clone(), self.recolouring) {
                    (Some(mask), false) => {
                        self.recolouring = true;
                        rt.spawn(async move {
                            let made = tokio::task::spawn_blocking(move || BaseImage { image: Arc::new(colour_holo(&mask, &holo_palette(hue))), mask: None, hue: Some(hue) }).await.map_err(|e| e.to_string());
                            deliver(Arrival::Base(Look::Holo, made));
                        });
                    }
                    (None, _) => self.holo_hue = hue,
                    _ => {}
                }
            }
        }
        if self.layers.quakes && self.quakes.due(QUAKES_EVERY) {
            self.quakes.fetch = Fetch::Loading;
            rt.spawn(async { deliver(Arrival::Quakes(fetch_quakes_week().await)) });
        }
        if (self.layers.storms || self.layers.fires) && self.events.due(EVENTS_EVERY) {
            self.events.fetch = Fetch::Loading;
            rt.spawn(async { deliver(Arrival::Events(fetch_events().await)) });
        }
        if self.layers.iss {
            if self.iss.due(ISS_EVERY) {
                self.iss.fetch = Fetch::Loading;
                rt.spawn(async { deliver(Arrival::Iss(fetch_iss().await)) });
            }
            if self.track.due(TRACK_EVERY) {
                self.track.fetch = Fetch::Loading;
                let ascending = match self.iss.items.as_slice() {
                    [a, b] => b.lat >= a.lat,
                    _ => true,
                };
                let now = self.iss.items.last().copied();
                rt.spawn(async move { deliver(Arrival::Track(fetch_iss_track(now, ascending).await)) });
            }
        }
        self.schedule_flights(rt, view);
        if view.resting {
            self.look_up_place(rt, view);
        }
    }

    /// The globe's textures, one at a time (each from the disk cache when it is there): the day
    /// first, then the land mask, the night lights and the relief; a date's imagery when asked.
    fn schedule_gpu(&mut self, rt: &Handle) {
        let Some(caps) = gl::caps() else { return };
        if gl::failed() || self.gpu_busy {
            return;
        }
        if self.historic != self.gpu_historic && !matches!(self.gpu_fetch[gl::Slot::Historic as usize], Fetch::Loading) {
            self.gpu_fetch[gl::Slot::Historic as usize] = Fetch::Idle;
        }
        let photo = self.look == Look::Satellite;
        let wanted = [
            (gl::Slot::Day, true),
            (gl::Slot::Mask, true),
            (gl::Slot::Historic, self.historic.is_some() && self.historic != self.gpu_historic),
            (gl::Slot::Night, photo),
            (gl::Slot::Relief, !photo),
        ];
        for (slot, want) in wanted {
            let i = slot as usize;
            if !want || self.gpu_fetch[i] != Fetch::Idle || (slot != gl::Slot::Historic && gl::has(slot)) {
                continue;
            }
            self.gpu_fetch[i] = Fetch::Loading;
            self.gpu_busy = true;
            let date = self.historic;
            if slot == gl::Slot::Historic {
                self.gpu_historic = date;
            }
            rt.spawn(async move {
                let result = fetch_gpu_base(slot, caps, date).await.map(|image| gl::submit(slot, image));
                deliver(Arrival::GpuBase(slot, result));
            });
            return;
        }
    }

    /// The named peaks in view (a little more than the view, so a small pan needs no new query).
    fn schedule_peaks(&mut self, rt: &Handle, view: &Outlook) {
        if view.alt > PEAKS_MAX_ALT_KM || self.peaks.fetch == Fetch::Loading || self.peaks_backoff.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let half_lat = (view.alt / 111.2 * 0.75).clamp(0.08, 6.0);
        let half_lon = (half_lat / view.lat.to_radians().cos().max(0.2)).min(10.0);
        let inside = self.peaks_box.is_some_and(|b| b.contains(view.lat - half_lat * 0.6, view.lon - half_lon * 0.6) && b.contains(view.lat + half_lat * 0.6, view.lon + half_lon * 0.6) && b.span().0 < half_lat * 5.0);
        if inside || self.peaks.at.is_some_and(|at| at.elapsed() < PEAKS_GAP) {
            return;
        }
        let bbox = Bbox::around(view.lat, view.lon, half_lat * 1.3, half_lon * 1.3);
        let host = self.peaks_host;
        self.peaks.fetch = Fetch::Loading;
        self.peaks.at = Some(Instant::now());
        rt.spawn(async move {
            let result = fetch_peaks(bbox, host).await;
            deliver(Arrival::Peaks(bbox, result, host));
        });
    }

    fn schedule_flights(&mut self, rt: &Handle, view: &Outlook) {
        if !self.layers.flights {
            self.flights_note = None;
            return;
        }
        if self.flights_backoff.is_some_and(|until| Instant::now() < until) || self.flights.fetch == Fetch::Loading {
            return;
        }
        let Some(bbox) = view.bbox.filter(|_| view.alt <= FLIGHTS_MAX_ALT_KM) else {
            self.flights_note = Some("ZOOM IN FOR FLIGHTS");
            return;
        };
        self.flights_note = None;
        let age = self.flights.at.map_or(Duration::MAX, |at| at.elapsed());
        let moved = self.flights_box.is_none_or(|old| {
            let ((a, b), (c, d)) = (old.centre(), bbox.centre());
            let (span_lat, span_lon) = old.span();
            (a - c).abs() > span_lat * 0.4 || (b - d).abs() > span_lon * 0.4
        });
        if age >= FLIGHTS_EVERY || (moved && age >= FLIGHTS_MIN && view.resting) {
            self.flights.fetch = Fetch::Loading;
            rt.spawn(async move {
                let (result, backoff) = fetch_flights(bbox).await;
                deliver(Arrival::Flights(bbox, result, backoff));
            });
        }
    }

    /// The place chip and card: the country or region in view, its weather and time zone.
    fn look_up_place(&mut self, rt: &Handle, view: &Outlook) {
        // Around Sir's own position nothing is looked up: his position stays on this PC.
        if self.near_me(view.lat, view.lon) {
            self.place = self.me.as_ref().and_then(|m| m.label.clone()).map(|city| Place { city: city.clone(), region: city, ..Place::default() });
            self.place_cell = None;
            self.weather = None;
            self.weather_cell = None;
            return;
        }
        let cell = cell_of(view.lat, view.lon, cell_size_for(view.alt));
        if self.place_cell != Some(cell) {
            if let Some(known) = self.place_cache.get(&cell) {
                self.place_cell = Some(cell);
                self.place = known.clone();
            } else if !self.place_busy && self.place_asked.is_none_or(|at| at.elapsed() >= PLACE_GAP) {
                self.place_cell = Some(cell);
                self.place_busy = true;
                self.place_asked = Some(Instant::now());
                let (lat, lon) = cell_centre(cell);
                rt.spawn(async move { deliver(Arrival::Place(cell, fetch_place(lat, lon).await)) });
            }
        }
        let cell = cell_of(view.lat, view.lon, cell_size_for(view.alt).max(0.5));
        if self.weather_cell != Some(cell) || self.weather_cache.get(&cell).is_some_and(|(_, at)| at.elapsed() > WEATHER_TTL) {
            if let Some((known, at)) = self.weather_cache.get(&cell).filter(|(_, at)| at.elapsed() <= WEATHER_TTL) {
                let _ = at;
                self.weather_cell = Some(cell);
                self.weather = known.clone();
            } else if !self.weather_busy && self.weather_asked.is_none_or(|at| at.elapsed() >= WEATHER_GAP) {
                self.weather_cell = Some(cell);
                self.weather_busy = true;
                self.weather_asked = Some(Instant::now());
                let (lat, lon) = cell_centre(cell);
                rt.spawn(async move { deliver(Arrival::Weather(cell, fetch_weather(lat, lon).await)) });
            }
        }
    }

    /// Within `PRIVATE_KM` of Sir's fix.
    pub fn near_me(&self, lat: f64, lon: f64) -> bool {
        self.me.as_ref().is_some_and(|m| haversine_km((m.lat, m.lon), (lat, lon)) < PRIVATE_KM.max(m.accuracy_m / 1000.0 * 2.0))
    }

    /// One line on the state of the imagery for the map's status (None: all is well): what is
    /// loading or missing, the day shown, the radar's time.
    pub fn status(&self, now: f64) -> Option<String> {
        if self.demo {
            return Some("DEMO DATA \u{b7} NO IMAGERY".into());
        }
        if let Some(date) = self.historic {
            return Some(format!("NASA VIIRS \u{b7} {}", date.format("%-d %b %Y").to_string().to_uppercase()));
        }
        if self.overlays.radar && self.overlays.animate {
            if let Some(frame) = self.radar_frame(now) {
                let at = DateTime::from_timestamp(frame.time, 0).map(|t| t.with_timezone(&Local).format("%H:%M").to_string()).unwrap_or_default();
                return Some(format!("RADAR {at}"));
            }
        }
        if gl::proven() {
            let day = gl::Slot::Day as usize;
            return match (&self.gpu_fetch[day], gl::ready(gl::Slot::Day)) {
                (_, true) => self.flights_note.map(str::to_string),
                (Fetch::Failed(_), false) => Some("OFFLINE \u{b7} NO IMAGERY".into()),
                _ => Some("ACQUIRING IMAGERY..".into()),
            };
        }
        match (&self.holo.fetch, &self.sat.fetch, self.look) {
            (Fetch::Loading | Fetch::Idle, _, Look::Holo) => Some("ACQUIRING IMAGERY..".into()),
            (_, Fetch::Loading | Fetch::Idle, Look::Satellite) => Some("ACQUIRING SATELLITE IMAGERY..".into()),
            (Fetch::Failed(_), _, Look::Holo) => Some("OFFLINE \u{b7} NO IMAGERY".into()),
            (_, Fetch::Failed(_), Look::Satellite) => Some("NO SATELLITE IMAGERY".into()),
            _ => self.flights_note.map(str::to_string),
        }
    }
}

// ---------------------------------------------------------------------- shared state ----

#[cfg(not(test))]
static OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// A test opens the Earth on its own thread only: the router's tests read `is_open` meanwhile.
#[cfg(test)]
thread_local!(static OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
static STATE: LazyLock<Mutex<Earth>> = LazyLock::new(|| Mutex::new(Earth::new()));

/// The Earth's state (the HUD holds it while it draws a frame).
pub fn state() -> MutexGuard<'static, Earth> {
    crate::crash_guard::lock(&STATE)
}

/// Whether a layer of the globe is on now, by any of its names ("flights", "clouds",
/// "satellites"); None for a name that is no layer.
pub fn layer_shown(name: &str) -> Option<bool> {
    if let Some(layer) = Layer::parse(name).filter(|_| name != "satellite") {
        return Some(state().layers.get(layer));
    }
    if let Some(o) = Overlay::parse(name) {
        return Some(state().overlays.get(o));
    }
    crate::earth_layers::shown(name)
}

/// Opens or closes the Earth; whether it was open.
fn set_open(on: bool) -> bool {
    #[cfg(not(test))]
    {
        OPEN.swap(on, Ordering::AcqRel)
    }
    #[cfg(test)]
    {
        OPEN.with(|open| open.replace(on))
    }
}

/// The Earth is up (a block on the board, or its own window).
pub fn is_open() -> bool {
    #[cfg(not(test))]
    {
        OPEN.load(Ordering::Acquire)
    }
    #[cfg(test)]
    {
        OPEN.with(|open| open.get())
    }
}

/// Closes the Earth (its `[ CLOSE ]`, "close the earth"); its textures leave the GPU once its
/// window has played its exit (`let_go`).
pub fn close() {
    set_open(false);
}

/// The closed Earth has left the screen (its exit included): its textures leave the GPU.
pub fn let_go() {
    if !is_open() {
        state().release_gpu();
    }
}

/// The Earth's shared state is global: tests that use it run one at a time.
#[cfg(test)]
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    crate::crash_guard::lock(&LOCK)
}

/// A finished image: the texture and, for the hologram, the mask it was coloured from.
pub struct BaseImage {
    pub image: Arc<ColorImage>,
    pub mask: Option<Arc<HoloMask>>,
    pub hue: Option<f32>,
}

/// What a background task brings back.
pub enum Arrival {
    Quakes(Result<Vec<Quake>, String>),
    Events(Result<Vec<GeoEvent>, String>),
    Iss(Result<Iss, String>),
    Track(Result<Vec<(f64, f64)>, String>),
    Flights(Bbox, Result<Vec<Flight>, String>, Option<Duration>),
    Place(Cell, Option<Place>),
    Weather(Cell, Option<Weather>),
    Base(Look, Result<BaseImage, String>),
    /// The URL form a tile layer answers to (or whether GIBS answered at all).
    Probe(TileLayer, Result<usize, bool>),
    /// A tile's image, or whether it will never come.
    Tile(TileKey, Result<ColorImage, bool>),
    Area(u64, Result<Vec<Quake>, String>),
    /// A globe texture went to the GPU's queue (or could not be made).
    GpuBase(gl::Slot, Result<(), String>),
    Radar(Result<src::RadarFrames, String>),
    /// The peaks in a box, and the Overpass server asked.
    Peaks(Bbox, Result<Vec<Peak>, String>, usize),
}

static INBOX: Mutex<Vec<Arrival>> = Mutex::new(Vec::new());
/// The context and viewport that draw the Earth (woken when something arrives).
static WAKER: Mutex<Option<(egui::Context, egui::ViewportId)>> = Mutex::new(None);

/// The HUD frame drawing the Earth: arrivals wake that viewport.
pub fn set_waker(ctx: &egui::Context) {
    *crate::crash_guard::lock(&WAKER) = Some((ctx.clone(), ctx.viewport_id()));
}

fn deliver(arrival: Arrival) {
    crate::crash_guard::lock(&INBOX).push(arrival);
    crate::gui::refresh();
    if let Some((ctx, viewport)) = crate::crash_guard::lock(&WAKER).clone() {
        ctx.request_repaint_of(viewport);
    }
}

/// No network: the verifier, a dry run, the unit tests.
fn offline() -> bool {
    cfg!(test) || crate::live_self_tester::running() || crate::live_self_tester::dry()
}

// ------------------------------------------------------------------------------ network ----

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder().user_agent(USER_AGENT).timeout(FETCH_TIMEOUT).connect_timeout(Duration::from_secs(5)).build().unwrap_or_else(|_| reqwest::Client::new())
});

/// A GET's body, or (HTTP status if any, what went wrong).
async fn get(url: &str, timeout: Duration) -> Result<Vec<u8>, (Option<u16>, String)> {
    let response = HTTP.get(url).timeout(timeout).send().await.map_err(|e| (None, e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err((Some(status.as_u16()), format!("HTTP {status}")));
    }
    response.bytes().await.map(|b| b.to_vec()).map_err(|e| (None, e.to_string()))
}

pub(crate) async fn get_json(url: &str, timeout: Duration) -> Result<Value, String> {
    let body = get(url, timeout).await.map_err(|e| e.1)?;
    serde_json::from_slice(&body).map_err(|e| format!("unreadable answer: {e}"))
}

pub const QUAKES_URL: &str = "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/4.5_week.geojson";
pub const EVENTS_URL: &str = "https://eonet.gsfc.nasa.gov/api/v3/events?status=open&category=severeStorms,wildfires&days=20";
pub const ISS_URL: &str = "https://api.wheretheiss.at/v1/satellites/25544";

/// The USGS FDSN query for an area: M2.5+ within `radius_km` over the last `days`.
pub fn area_url(lat: f64, lon: f64, radius_km: f64, days: u32, now: DateTime<Utc>) -> String {
    let start = (now - chrono::Duration::days(days as i64)).format("%Y-%m-%dT%H:%M:%S");
    format!("https://earthquake.usgs.gov/fdsnws/event/1/query?format=geojson&latitude={lat:.3}&longitude={:.3}&maxradiuskm={radius_km:.0}&starttime={start}&minmagnitude=2.5&orderby=magnitude&limit=200", wrap_lon(lon))
}

pub fn flights_url(b: &Bbox) -> String {
    format!("https://opensky-network.org/api/states/all?lamin={:.3}&lomin={:.3}&lamax={:.3}&lomax={:.3}", b.south, b.west, b.north, b.east)
}

pub fn place_url(lat: f64, lon: f64) -> String {
    format!("https://api.bigdatacloud.net/data/reverse-geocode-client?latitude={lat:.3}&longitude={:.3}&localityLanguage=en", wrap_lon(lon))
}

pub fn weather_url(lat: f64, lon: f64) -> String {
    format!("https://api.open-meteo.com/v1/forecast?latitude={lat:.3}&longitude={:.3}&current=temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,wind_direction_10m,weather_code&daily=sunrise,sunset&timezone=auto&forecast_days=1", wrap_lon(lon))
}

pub fn geocode_url(name: &str) -> String {
    format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json", urlencoding::encode(name.trim()))
}

async fn fetch_quakes_week() -> Result<Vec<Quake>, String> {
    Ok(parse_quakes(&get_json(QUAKES_URL, FETCH_TIMEOUT).await?))
}

async fn fetch_area(lat: f64, lon: f64, radius_km: f64, days: u32, timeout: Duration) -> Result<Vec<Quake>, String> {
    Ok(parse_quakes(&get_json(&area_url(lat, lon, radius_km, days, Utc::now()), timeout).await?))
}

async fn fetch_events() -> Result<Vec<GeoEvent>, String> {
    Ok(parse_events(&get_json(EVENTS_URL, FETCH_TIMEOUT).await?))
}

async fn fetch_iss() -> Result<Iss, String> {
    parse_iss(&get_json(ISS_URL, FETCH_TIMEOUT).await?).ok_or_else(|| "no position in the answer".to_string())
}

/// The ISS's ground track from 45 minutes ago to 45 minutes ahead (wheretheiss.at), or the
/// orbit model's when that does not answer.
async fn fetch_iss_track(now: Option<Iss>, ascending: bool) -> Result<Vec<(f64, f64)>, String> {
    let t = Utc::now().timestamp();
    let stamps: Vec<String> = (-4..=5).map(|k| (t + k * 600 - 300).to_string()).collect();
    let url = format!("{ISS_URL}/positions?timestamps={}&units=kilometers", stamps.join(","));
    match get_json(&url, FETCH_TIMEOUT).await.map(|v| parse_iss_track(&v)) {
        Ok(points) if points.len() >= 2 => Ok(points),
        other => match now {
            Some(iss) => Ok(iss_ground_track(iss.lat, iss.lon, ascending, &(-9..=9).map(|k| k as f64 * 5.0).collect::<Vec<_>>())),
            None => Err(other.err().unwrap_or_else(|| "no track".to_string())),
        },
    }
}

/// OpenSky's aircraft in a box, and how long to wait when it said "too many requests".
async fn fetch_flights(bbox: Bbox) -> (Result<Vec<Flight>, String>, Option<Duration>) {
    match get(&flights_url(&bbox), FETCH_TIMEOUT).await {
        Ok(body) => (serde_json::from_slice::<Value>(&body).map(|v| parse_flights(&v)).map_err(|e| e.to_string()), None),
        Err((Some(429), e)) => (Err(e), Some(Duration::from_secs(600))),
        Err((_, e)) => (Err(e), Some(Duration::from_secs(60))),
    }
}

async fn fetch_place(lat: f64, lon: f64) -> Option<Place> {
    parse_reverse(&get_json(&place_url(lat, lon), FETCH_TIMEOUT).await.ok()?)
}

async fn fetch_weather(lat: f64, lon: f64) -> Option<Weather> {
    parse_weather(&get_json(&weather_url(lat, lon), FETCH_TIMEOUT).await.ok()?)
}

async fn geocode(name: &str) -> Result<GeoHit, String> {
    let v = get_json(&geocode_url(name), FETCH_TIMEOUT).await?;
    parse_geocode(&v).ok_or_else(|| format!("I couldn't find '{name}' on the map."))
}

/// A few well-known places for the demo and for when the geocoder is out of reach.
pub fn gazetteer(name: &str) -> Option<GeoHit> {
    const PLACES: [(&str, f64, f64); 32] = [
        ("new york", 40.7128, -74.006),
        ("london", 51.5072, -0.1276),
        ("paris", 48.8566, 2.3522),
        ("berlin", 52.52, 13.405),
        ("tokyo", 35.6895, 139.6917),
        ("naples", 40.8518, 14.2681),
        ("rome", 41.9028, 12.4964),
        ("madrid", 40.4168, -3.7038),
        ("lisbon", 38.7223, -9.1393),
        ("athens", 37.9838, 23.7275),
        ("istanbul", 41.0082, 28.9784),
        ("moscow", 55.7558, 37.6173),
        ("cairo", 30.0444, 31.2357),
        ("nairobi", -1.2921, 36.8219),
        ("cape town", -33.9249, 18.4241),
        ("dubai", 25.2048, 55.2708),
        ("mumbai", 19.076, 72.8777),
        ("delhi", 28.6139, 77.209),
        ("beijing", 39.9042, 116.4074),
        ("shanghai", 31.2304, 121.4737),
        ("seoul", 37.5665, 126.978),
        ("jakarta", -6.2088, 106.8456),
        ("manila", 14.5995, 120.9842),
        ("sydney", -33.8688, 151.2093),
        ("los angeles", 34.0522, -118.2437),
        ("san francisco", 37.7749, -122.4194),
        ("mexico city", 19.4326, -99.1332),
        ("toronto", 43.6532, -79.3832),
        ("rio de janeiro", -22.9068, -43.1729),
        ("lima", -12.0464, -77.0428),
        ("reykjavik", 64.1466, -21.9426),
        ("anchorage", 61.2181, -149.9003),
    ];
    let key = name.trim().to_lowercase();
    let key = key.strip_prefix("the ").unwrap_or(&key);
    PLACES.iter().find(|(n, ..)| *n == key).map(|(n, lat, lon)| GeoHit {
        name: n.split(' ').map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }).collect::<Vec<_>>().join(" "),
        lat: *lat,
        lon: *lon,
        kind: PlaceKind::City,
    })
}

async fn resolve(name: &str, offline: bool) -> Result<GeoHit, String> {
    if let Some(hit) = continent(name) {
        return Ok(hit);
    }
    if offline {
        return gazetteer(name).ok_or_else(|| format!("'{name}' is not among the demo's places."));
    }
    match geocode(name).await {
        Ok(hit) => Ok(hit),
        Err(e) => gazetteer(name).ok_or(e),
    }
}

/// Sir's approximate location when the app already knows one (the home saved in the settings),
/// rounded to half a degree: nothing finer leaves the PC.
fn known_location(offline: bool) -> Option<(f64, f64)> {
    if offline {
        return None;
    }
    // (Where "locate me" found him, else the home in the settings; to half a degree either way.)
    let found = state().me.as_ref().map(|f| (f.lat, f.lon));
    let (lat, lon) = match found {
        Some(p) => p,
        None => {
            let raw = std::fs::read_to_string(crate::config::CONFIG_FILE).ok()?;
            let v: Value = serde_json::from_str(&raw).ok()?;
            (v["location"]["home_lat"].as_f64()?, v["location"]["home_lon"].as_f64()?)
        }
    };
    Some(((lat * 2.0).round() / 2.0, (lon * 2.0).round() / 2.0))
}

/// The ISS's ground track from a position on a circular orbit (51.64°, 92.9 min) over the
/// turning Earth, at `minutes` from now.
pub fn iss_ground_track(lat: f64, lon: f64, ascending: bool, minutes: &[f64]) -> Vec<(f64, f64)> {
    const INCLINATION: f64 = 51.64;
    const PERIOD_MIN: f64 = 92.9;
    const SIDEREAL_MIN: f64 = 1436.07;
    let i = INCLINATION.to_radians();
    let (si, ci) = (i.sin(), i.cos());
    let mut u0 = (lat.to_radians().sin() / si).clamp(-1.0, 1.0).asin();
    if !ascending {
        u0 = PI - u0;
    }
    let node = lon.to_radians() - (ci * u0.sin()).atan2(u0.cos());
    minutes
        .iter()
        .map(|t| {
            let u = u0 + 2.0 * PI * t / PERIOD_MIN;
            let node = node - 2.0 * PI * t / SIDEREAL_MIN;
            let lat = (si * u.sin()).clamp(-1.0, 1.0).asin();
            let lon = node + (ci * u.sin()).atan2(u.cos());
            (lat.to_degrees(), wrap_lon(lon.to_degrees()))
        })
        .collect()
}

// ------------------------------------------------------------------------ disk cache ----

fn cache_path(parts: &[&str]) -> PathBuf {
    parts.iter().fold(PathBuf::from(CACHE_DIR), |p, part| p.join(part))
}

fn tile_path(key: TileKey) -> PathBuf {
    let ext = if key.layer.photo() { "jpg" } else { "png" };
    cache_path(&["tiles", &key.layer.dir(), &key.z.to_string(), &format!("{}_{}.{ext}", key.x, key.y)])
}

static WRITES: AtomicUsize = AtomicUsize::new(0);

async fn cache_write(path: PathBuf, bytes: Vec<u8>) {
    if let Some(dir) = path.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    if let Err(e) = tokio::fs::write(&path, bytes).await {
        debug!("[EARTH] Could not cache {}: {e}", path.display());
    }
    // Every 150 writes the cache is measured again.
    if WRITES.fetch_add(1, Ordering::AcqRel) % 150 == 149 {
        let _ = tokio::task::spawn_blocking(|| trim_cache(Path::new(CACHE_DIR), CACHE_LIMIT, CACHE_TRIM_TO)).await;
    }
}

/// Keeps a cache folder under `limit` bytes: past it, the oldest files go until `target` is
/// left. How many bytes are left.
pub fn trim_cache(dir: &Path, limit: u64, target: u64) -> u64 {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), e.into_path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= limit {
        return total;
    }
    files.sort_by_key(|f| f.0);
    for (_, size, path) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    info!("[EARTH] Trimmed the imagery cache to {} MB", total / (1024 * 1024));
    total
}

fn templates_path() -> PathBuf {
    cache_path(&["templates.json"])
}

fn load_templates() -> HashMap<TileLayer, Template> {
    let Ok(raw) = std::fs::read_to_string(templates_path()) else { return HashMap::new() };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else { return HashMap::new() };
    [TileLayer::Holo, TileLayer::Satellite, TileLayer::Features, TileLayer::Labels]
        .into_iter()
        .filter_map(|l| v[l.dir().as_str()].as_u64().filter(|f| (*f as usize) < TEMPLATE_FORMS).map(|f| (l, Template::Works(f as usize))))
        .collect()
}

fn save_templates(templates: &HashMap<TileLayer, Template>) {
    let mut v = serde_json::Map::new();
    for (layer, t) in templates {
        if let Template::Works(form) = t {
            v.insert(layer.dir(), Value::from(*form as u64));
        }
    }
    let path = templates_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, Value::Object(v).to_string());
}

// ---------------------------------------------------------------------- imagery fetch ----

/// Bytes of an image from the cache, or from the network (cached when it decodes).
async fn image_bytes(path: PathBuf, urls: &[String]) -> Result<Vec<u8>, String> {
    if let Ok(bytes) = tokio::fs::read(&path).await {
        if image::load_from_memory(&bytes).is_ok() {
            return Ok(bytes);
        }
    }
    let mut last = String::from("no source");
    for url in urls {
        match get(url, Duration::from_secs(20)).await {
            Ok(bytes) if image::load_from_memory(&bytes).is_ok() => {
                cache_write(path, bytes.clone()).await;
                return Ok(bytes);
            }
            Ok(_) => last = "the answer was no image".to_string(),
            Err((_, e)) => last = e,
        }
    }
    Err(last)
}

/// The hologram: the shaded relief classified, coastlines and borders drawn in, coloured in
/// the theme's hue.
async fn fetch_holo_base(hue: f32) -> Result<BaseImage, String> {
    let urls = |layer: &str, png: bool| vec![wms_url(layer, png, false), wms_url(layer, png, true)];
    let (relief_urls, coast_urls, border_urls) = (urls("BlueMarble_ShadedRelief_Bathymetry", false), urls("Coastlines_15m", true), urls("Reference_Features_15m", true));
    let relief = image_bytes(cache_path(&["base", "relief_2048.jpg"]), &relief_urls);
    let coast = image_bytes(cache_path(&["base", "coastlines_2048.png"]), &coast_urls);
    let borders = image_bytes(cache_path(&["base", "features_2048.png"]), &border_urls);
    let (relief, coast, borders) = tokio::join!(relief, coast, borders);
    let relief = relief?;
    let (coast, borders) = (coast.ok(), borders.ok());
    tokio::task::spawn_blocking(move || {
        let relief = image::load_from_memory(&relief).map_err(|e| e.to_string())?.to_rgb8();
        let decode = |b: Option<Vec<u8>>| b.and_then(|b| image::load_from_memory(&b).ok()).map(|i| i.to_rgba8());
        let mask = build_mask(&relief, decode(coast).as_ref(), decode(borders).as_ref());
        let image = colour_holo(&mask, &holo_palette(hue));
        info!("[EARTH] Holographic imagery ready ({}x{})", mask.w, mask.h);
        Ok(BaseImage { image: Arc::new(image), mask: Some(Arc::new(mask)), hue: Some(hue) })
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn fetch_satellite_base() -> Result<BaseImage, String> {
    let urls = vec![wms_url("BlueMarble_NextGeneration", false, false), wms_url("BlueMarble_NextGeneration", false, true)];
    let bytes = image_bytes(cache_path(&["base", "bluemarble_2048.jpg"]), &urls).await?;
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?.to_rgba8();
        Ok(BaseImage { image: Arc::new(to_color_image(&img)), mask: None, hue: None })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A tile's bytes decoded for the GPU: the relief in the holo look, the GIBS lines as white light,
/// the elevation as shading; imagery and the other overlays as they are.
fn tile_image(key: TileKey, bytes: &[u8], hue: f32) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(match key.layer {
        TileLayer::Holo => colour_holo(&build_mask(&img.to_rgb8(), None, None), &holo_palette(hue)),
        TileLayer::Features => as_light(&img.to_rgba8()),
        TileLayer::Terrain(tenths) => {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            let n = 1u64 << key.z;
            let lat = lat_of_merc_y((key.y as f64 + 0.5) / n as f64);
            let shaded = src::terrain_overlay(rgba.as_raw(), w, h, key.z, lat, tenths as f32 / 10.0);
            ColorImage::from_rgba_premultiplied([w, h], &shaded)
        }
        _ => to_color_image(&img.to_rgba8()),
    })
}

/// A tile of the layers that need no probing: from the disk cache or the first URL that answers;
/// the infrared clouds from all three satellites, merged.
async fn fetch_source_tile(key: TileKey, urls: Vec<String>, hue: f32) -> Result<ColorImage, bool> {
    if let TileLayer::Clouds(_) = key.layer {
        let got = futures_util::future::join_all(urls.iter().map(|u| get(u, FETCH_TIMEOUT))).await;
        if got.iter().all(Result::is_err) {
            return Err(got.iter().all(|r| matches!(r, Err((Some(400 | 404), _)))));
        }
        return tokio::task::spawn_blocking(move || {
            let side = gl::TILE_PX as usize;
            let tiles: Vec<Option<Vec<u8>>> = got
                .into_iter()
                .map(|r| r.ok().and_then(|b| image::load_from_memory(&b).ok()).map(|i| i.resize_exact(side as u32, side as u32, image::imageops::FilterType::Triangle).to_rgba8().into_raw()))
                .collect();
            let merged = src::merge_ir(&tiles, side * side);
            ColorImage::from_rgba_premultiplied([side, side], &merged)
        })
        .await
        .map_err(|_| false);
    }
    let path = tile_path(key);
    let cached = if key.layer.cached_on_disk() { tokio::fs::read(&path).await.ok() } else { None };
    let bytes = match cached {
        Some(bytes) => bytes,
        None => {
            let mut forever = false;
            let mut found = None;
            for url in &urls {
                match get(url, FETCH_TIMEOUT).await {
                    Ok(bytes) => {
                        found = Some(bytes);
                        break;
                    }
                    Err((status, _)) => forever = matches!(status, Some(400 | 404)),
                }
            }
            let Some(bytes) = found else { return Err(forever) };
            if key.layer.cached_on_disk() && image::load_from_memory(&bytes).is_ok() {
                cache_write(path, bytes.clone()).await;
            }
            bytes
        }
    };
    tokio::task::spawn_blocking(move || tile_image(key, &bytes, hue)).await.ok().flatten().ok_or(true)
}

/// RainViewer's frame list.
async fn fetch_radar() -> Result<src::RadarFrames, String> {
    src::parse_rainviewer(&get_json(src::RAINVIEWER_URL, FETCH_TIMEOUT).await?).ok_or_else(|| "no radar frames".to_string())
}

/// The named peaks in a box from an Overpass server (the higher ones only in a wide box).
async fn fetch_peaks(b: Bbox, host: usize) -> Result<Vec<Peak>, String> {
    let (span_lat, span_lon) = b.span();
    let query = src::peaks_query_from(b.south, b.west, b.north, b.east, src::peaks_floor(span_lat.max(span_lon * b.centre().0.to_radians().cos())));
    let url = src::overpass_url(src::OVERPASS_HOSTS[host % src::OVERPASS_HOSTS.len()], &query);
    Ok(src::parse_peaks(&get_json(&url, Duration::from_secs(25)).await?))
}

/// A whole-world image from GIBS WMS in `cols` x `rows` pieces of 2048 px (each cached), as RGB.
async fn world_pieces(layer: &str, date: Option<NaiveDate>, cols: u32, rows: u32, png: bool) -> Result<image::RgbaImage, String> {
    const PIECE: u32 = 2048;
    let (dlon, dlat) = (360.0 / cols as f64, 180.0 / rows as f64);
    let tag = date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "best".into());
    let mut jobs: Vec<(PathBuf, String)> = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let (south, north) = (90.0 - (r + 1) as f64 * dlat, 90.0 - r as f64 * dlat);
            let (west, east) = (-180.0 + c as f64 * dlon, -180.0 + (c + 1) as f64 * dlon);
            let format = if png { "image/png&TRANSPARENT=TRUE" } else { "image/jpeg" };
            let time = date.map(|d| format!("&TIME={}", d.format("%Y-%m-%d"))).unwrap_or_default();
            let url = format!("https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi?SERVICE=WMS&REQUEST=GetMap&VERSION=1.3.0&LAYERS={layer}&STYLES=&CRS=EPSG:4326&BBOX={south},{west},{north},{east}&WIDTH={PIECE}&HEIGHT={PIECE}&FORMAT={format}{time}");
            let path = cache_path(&["globe", "pieces", &format!("{layer}_{tag}_{cols}x{rows}_{r}_{c}.{}", if png { "png" } else { "jpg" })]);
            jobs.push((path, url));
        }
    }
    // (Two at a time: polite, and the pieces are big.)
    let mut bytes = Vec::new();
    for pair in jobs.chunks(2) {
        let got = futures_util::future::join_all(pair.iter().map(|(path, url)| image_bytes(path.clone(), std::slice::from_ref(url)))).await;
        for b in got {
            bytes.push(b?);
        }
    }
    tokio::task::spawn_blocking(move || {
        let mut world = image::RgbaImage::new(cols * PIECE, rows * PIECE);
        for (i, b) in bytes.iter().enumerate() {
            let piece = image::load_from_memory(b).map_err(|e| e.to_string())?.to_rgba8();
            let piece = if piece.dimensions() == (PIECE, PIECE) { piece } else { image::imageops::resize(&piece, PIECE, PIECE, image::imageops::FilterType::Triangle) };
            image::imageops::replace(&mut world, &piece, (i as u32 % cols * PIECE) as i64, (i as u32 / cols * PIECE) as i64);
        }
        Ok(world)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One of the globe's textures: from its blob in the disk cache, or made from GIBS imagery (and
/// cached as a blob for the next time).
async fn fetch_gpu_base(slot: gl::Slot, caps: gl::Caps, date: Option<NaiveDate>) -> Result<GpuImage, String> {
    let big = caps.dxt1 && caps.max_side >= 8192;
    let name = match slot {
        gl::Slot::Day => format!("day_{}.jegi", if big { "8k_dxt" } else if caps.dxt1 { "4k_dxt" } else { "4k_rgba" }),
        gl::Slot::Night => "night_4k.jegi".into(),
        // (v2: the deepest sea is water now; the old mask had black blots in the oceans.)
        gl::Slot::Mask => "mask_4k_v2.jegi".into(),
        gl::Slot::Relief => format!("relief_4k_{}.jegi", if caps.dxt1 { "dxt" } else { "rgba" }),
        gl::Slot::Historic => format!("daily_{}_{}.jegi", date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(), if caps.dxt1 { "dxt" } else { "rgba" }),
    };
    let path = cache_path(&["globe", &name]);
    if let Ok(blob) = tokio::fs::read(&path).await {
        let touched = path.clone();
        if let Some(image) = tokio::task::spawn_blocking(move || {
            // (Used: it stays out of the cache's trim a while longer.)
            let _ = std::fs::File::options().write(true).open(&touched).and_then(|f| f.set_modified(std::time::SystemTime::now()));
            GpuImage::from_blob(&blob)
        })
        .await
        .ok()
        .flatten()
        {
            return Ok(image);
        }
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
    let colour = move |img: image::RgbaImage| -> GpuImage {
        let (w, h) = img.dimensions();
        if caps.dxt1 {
            GpuImage::dxt1_with_mips(w, h, img.into_raw(), threads)
        } else {
            let (w2, h2) = (w.min(4096), h.min(2048));
            let img = if (w, h) == (w2, h2) { img } else { image::imageops::resize(&img, w2, h2, image::imageops::FilterType::Triangle) };
            GpuImage::with_mips(gl::Format::Rgba8, w2, h2, img.into_raw())
        }
    };
    let image = match slot {
        gl::Slot::Day => {
            let (cols, rows) = if big { (4, 2) } else { (2, 1) };
            let world = world_pieces(src::DAY_LAYER, None, cols, rows, false).await?;
            tokio::task::spawn_blocking(move || colour(world)).await.map_err(|e| e.to_string())?
        }
        gl::Slot::Relief => {
            let world = world_pieces(src::RELIEF_LAYER, None, 2, 1, false).await?;
            tokio::task::spawn_blocking(move || colour(world)).await.map_err(|e| e.to_string())?
        }
        gl::Slot::Historic => {
            let world = world_pieces(src::DAILY_LAYER, date, 2, 1, false).await?;
            tokio::task::spawn_blocking(move || colour(world)).await.map_err(|e| e.to_string())?
        }
        gl::Slot::Night => {
            let world = world_pieces(src::NIGHT_LAYER, None, 2, 1, false).await?;
            tokio::task::spawn_blocking(move || {
                let (w, h) = world.dimensions();
                let lum: Vec<u8> = world.pixels().map(|p| ((p.0[0] as u32 * 3 + p.0[1] as u32 * 5 + p.0[2] as u32 * 2) / 10) as u8).collect();
                GpuImage::with_mips(gl::Format::R8, w, h, lum)
            })
            .await
            .map_err(|e| e.to_string())?
        }
        gl::Slot::Mask => {
            let relief = world_pieces(src::RELIEF_LAYER, None, 2, 1, false).await?;
            let lines = world_pieces(src::LINES_LAYER, None, 2, 1, true).await.ok();
            tokio::task::spawn_blocking(move || {
                let (w, h) = relief.dimensions();
                GpuImage::with_mips(gl::Format::Rg8, w, h, gpu_mask(&relief, lines.as_ref()))
            })
            .await
            .map_err(|e| e.to_string())?
        }
    };
    let blob = image.to_blob();
    cache_write(path, blob).await;
    info!("[EARTH] Globe texture {slot:?} made ({} MB)", image.bytes() / (1024 * 1024));
    Ok(image)
}

/// The globe's mask (two bytes a texel): red is land, softened so its 0.5 line is the coast at
/// any zoom; green the borders (the GIBS line layer away from the coasts).
pub fn gpu_mask(relief: &image::RgbaImage, lines: Option<&image::RgbaImage>) -> Vec<u8> {
    let (w, h) = (relief.width() as usize, relief.height() as usize);
    let mut land: Vec<f32> = relief.pixels().map(|p| if is_water([p.0[0], p.0[1], p.0[2]]) { 0.0 } else { 1.0 }).collect();
    // A lone texel among four of the other kind takes theirs.
    let source = land.clone();
    for y in 1..h.saturating_sub(1) {
        for x in 0..w {
            let n = [source[y * w + (x + w - 1) % w], source[y * w + (x + 1) % w], source[(y - 1) * w + x], source[(y + 1) * w + x]];
            if n.iter().all(|v| *v == n[0]) && n[0] != source[y * w + x] {
                land[y * w + x] = n[0];
            }
        }
    }
    // Two passes of a 3x3 box blur: a ramp across the coast.
    for _ in 0..2 {
        let src = land.clone();
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for dy in [-1isize, 0, 1] {
                    let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    for dx in [w - 1, 0, 1] {
                        sum += src[yy * w + (x + dx) % w];
                    }
                }
                land[y * w + x] = sum / 9.0;
            }
        }
    }
    let mut out = Vec::with_capacity(w * h * 2);
    for y in 0..h {
        for x in 0..w {
            let l = land[y * w + x];
            let line = lines.map_or(0u8, |m| {
                let (mx, my) = ((x * m.width() as usize / w) as u32, (y * m.height() as usize / h) as u32);
                m.get_pixel(mx.min(m.width() - 1), my.min(m.height() - 1)).0[3]
            });
            // (Near a coast the line is the coast itself, drawn from the red.)
            let border = if l > 0.08 && l < 0.92 { 0 } else { line };
            out.push((l * 255.0).round() as u8);
            out.push(border);
        }
    }
    out
}

/// Finds the URL form a layer answers to with its world tile, which it delivers as a tile.
/// Err(true): GIBS answered but no form gave a tile; Err(false): GIBS was out of reach.
async fn probe_layer(layer: TileLayer, hue: f32) -> Result<usize, bool> {
    let key = TileKey { layer, z: 0, x: 0, y: 0 };
    for form in 0..TEMPLATE_FORMS {
        if !layer.photo() && form >= 3 {
            break;
        }
        match get(&tile_url(layer, form, 0, 0, 0), FETCH_TIMEOUT).await {
            Ok(bytes) => {
                let decoded = tokio::task::spawn_blocking({
                    let bytes = bytes.clone();
                    move || tile_image(key, &bytes, hue)
                })
                .await
                .ok()
                .flatten();
                if let Some(image) = decoded {
                    cache_write(tile_path(key), bytes).await;
                    deliver(Arrival::Tile(key, Ok(image)));
                    return Ok(form);
                }
            }
            Err((None, e)) => {
                // Offline: nothing to learn about the URL forms now.
                debug!("[EARTH] {layer:?} tiles unreachable: {e}");
                return Err(false);
            }
            Err(_) => {}
        }
    }
    warn!("[EARTH] GIBS serves no {layer:?} tiles in Web Mercator (none of the URL forms answered)");
    Err(true)
}

async fn fetch_tile(key: TileKey, form: usize, hue: f32) -> Result<ColorImage, bool> {
    let path = tile_path(key);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => match get(&tile_url(key.layer, form, key.z, key.x, key.y), FETCH_TIMEOUT).await {
            Ok(bytes) => {
                cache_write(path, bytes.clone()).await;
                bytes
            }
            Err((status, _)) => return Err(matches!(status, Some(400 | 404))),
        },
    };
    tokio::task::spawn_blocking(move || tile_image(key, &bytes, hue)).await.ok().flatten().ok_or(true)
}

// ---------------------------------------------------------------------------------- run ----

/// `open_jarvis_view {"view": "earth", "action": …, "place": …, "radius_km": …, "layer": …}`:
/// opens the globe or acts on it. The result is the display text and a "Spoken:" line.
pub async fn run(args: &Value) -> Result<String, String> {
    let action = parse_action(args)?;
    let offline = offline();
    {
        let mut st = state();
        let newly = !set_open(true);
        if newly {
            st.reset_for_open(offline);
        } else if !offline && st.demo {
            // A verifier's Earth that Sir now uses for real.
            st.leave_demo();
        }
    }
    crate::gui::show();
    crate::gui::refresh();
    let (display, spoken) = act(action, offline).await;
    crate::gui::refresh();
    Ok(format!("{display}\nSpoken: {spoken}"))
}

async fn act(action: Action, offline: bool) -> (String, String) {
    // Whatever Sir asks for next ends a tour (but the Earth's own "show").
    if !matches!(action, Action::Tour | Action::StopTour | Action::Show) {
        state().tour = None;
    }
    match action {
        Action::Show => ("The Earth is open: the globe with earthquakes, storms, wildfires and the ISS.".into(), "The Earth is on your display, Sir. You can spin or zoom it as you like.".into()),
        Action::Satellite => {
            let imagery = {
                let mut st = state();
                st.set_look(Look::Satellite);
                st.imagery
            };
            (format!("Satellite imagery: {}.", imagery.credit().trim_start_matches("Imagery: ")), "The satellite view is now active, Sir.".into())
        }
        Action::Locate => locate_me(offline).await,
        Action::Mountains(place) => mountains(place, offline).await,
        Action::Exaggerate(on) => {
            let mut st = state();
            st.overlays.terrain |= on;
            st.overlays.exaggerate = on;
            if on {
                ("Terrain: relief exaggerated 2.5 times.".into(), "Exaggerating the relief, Sir.".into())
            } else {
                ("Terrain: true to scale.".into(), "The relief is back to scale, Sir.".into())
            }
        }
        Action::Radar { animate } => {
            let mut st = state();
            st.overlays.radar = true;
            st.overlays.animate = animate;
            st.radar_since = f64::NAN;
            if !animate {
                return ("Rain radar: RainViewer.".into(), "The rain radar is on, Sir.".into());
            }
            // The loop runs on the map (the globe shows the latest frame): down to a region.
            let t = st.target();
            if t.alt > FLAT_FROM_KM {
                st.fly_to(Camera { alt: 1_300.0, ..t });
            }
            ("Rain radar: the past two hours in a loop (RainViewer).".into(), "Here's the radar over the past two hours, Sir.".into())
        }
        Action::Clouds => {
            let mut st = state();
            st.overlays.clouds = true;
            st.historic = None;
            ("Clouds: infrared from GOES and Himawari (NASA GIBS).".into(), "The live cloud cover is on, Sir.".into())
        }
        Action::Overlay(overlay, on) => {
            state().overlays.set(overlay, on);
            let spoken = match (overlay, on) {
                (Overlay::Labels, true) => "Labels and borders are on, Sir.",
                (Overlay::Labels, false) => "Labels and borders are off, Sir.",
                (Overlay::Terrain, true) => "The terrain is shaded, Sir.",
                (Overlay::Terrain, false) => "The terrain is off, Sir.",
                (Overlay::Radar, true) => "The rain radar is on, Sir.",
                (Overlay::Radar, false) => "The rain radar is off, Sir.",
                (Overlay::Clouds, true) => "The clouds are on, Sir.",
                (Overlay::Clouds, false) => "The clouds are off, Sir.",
            };
            (format!("{}: {}.", overlay.noun(), if on { "on" } else { "off" }), spoken.into())
        }
        Action::Imagery(imagery) => {
            let mut st = state();
            st.imagery = imagery;
            st.historic = None;
            st.set_look(Look::Satellite);
            (imagery.credit().to_string(), format!("Switching to {}, Sir.", imagery.spoken()))
        }
        Action::Historic { date, place } => historic(date, place, offline).await,
        Action::Today => {
            let mut st = state();
            st.historic = None;
            st.set_look(Look::Satellite);
            ("Today's imagery.".into(), "Back to today's imagery, Sir.".into())
        }
        Action::Tour => world_tour(offline).await,
        Action::StopTour => {
            let mut st = state();
            let touring = st.tour.take().is_some();
            if let Some(Motion::Fly { .. }) = st.motion {
                st.motion = None;
                st.queued = None;
            }
            if touring {
                ("The tour is over.".into(), "Stopping the tour, Sir.".into())
            } else {
                ("No tour was running.".into(), "There's no tour running, Sir.".into())
            }
        }
        Action::Holo => {
            state().set_look(Look::Holo);
            ("The holographic globe.".into(), "Back to the holographic view, Sir.".into())
        }
        Action::Place(name) => show_place(&name, offline).await,
        Action::Quakes { place: None, radius_km: None } => global_quakes(offline).await,
        Action::Quakes { place, radius_km } => area_quakes(place, radius_km, offline).await,
        Action::Flights(place) => flights(place, offline).await,
        Action::Storms => events(EventKind::Storm, offline).await,
        Action::Fires => events(EventKind::Fire, offline).await,
        Action::Iss => iss(offline).await,
        Action::Hide(layer) => {
            state().layers.set(layer, false);
            let spoken = if layer == Layer::Iss { "The space station is off the map, Sir.".to_string() } else { format!("The {} are off, Sir.", layer.noun()) };
            (format!("Layer off: {}.", layer.noun()), spoken)
        }
        Action::HideAll => {
            crate::webcams::set_earth_layer(false); // cameras (C2)
            let mut st = state();
            st.layers = Layers { quakes: false, storms: false, fires: false, iss: false, flights: false };
            st.overlays = Overlays { labels: st.overlays.labels, ..Overlays { labels: false, terrain: false, exaggerate: false, radar: false, animate: false, clouds: false } };
            st.area = None;
            crate::earth_layers::hide_all(); // live layers (C1)
            ("Every layer is off.".into(), "All layers are off, Sir.".into())
        }
        Action::ZoomIn => {
            state().zoom_by(1.0 / ZOOM_STEP);
            ("Zoomed in.".into(), "Zooming in, Sir.".into())
        }
        Action::ZoomOut => {
            state().zoom_by(ZOOM_STEP);
            ("Zoomed out.".into(), "Zooming out, Sir.".into())
        }
        Action::Home => {
            let mut st = state();
            st.area = None;
            st.go_home();
            ("The whole globe.".into(), "Back to the whole globe, Sir.".into())
        }
        Action::Spin => {
            state().spin = true;
            ("The globe spins.".into(), "Spinning the globe, Sir.".into())
        }
        Action::StopSpin => {
            state().spin = false;
            ("The globe holds still.".into(), "Holding it still, Sir.".into())
        }
        Action::Cameras => crate::webcams::earth_layer_action(offline).await, // cameras (C2)
        Action::Live(live) => crate::earth_layers::act(live, offline).await, // live layers (C1)
    }
}

async fn show_place(name: &str, offline: bool) -> (String, String) {
    match resolve(name, offline).await {
        Ok(hit) => {
            {
                let mut st = state();
                st.marker = Some(Marker { name: hit.name.clone(), lat: hit.lat, lon: hit.lon });
                st.fly_to(Camera { lat: hit.lat, lon: hit.lon, alt: hit.kind.altitude_km() });
            }
            (format!("{} at {}.", hit.name, coords_label(hit.lat, hit.lon)), format!("Here is {}, Sir.", hit.name))
        }
        Err(e) => (e, format!("I couldn't find {name} on the map, Sir.")),
    }
}

/// "Where am I?": the camera flies to Sir with the "YOU" marker and its accuracy circle.
async fn locate_me(offline: bool) -> (String, String) {
    match crate::earth_locate::locate(offline).await {
        Ok(fix) => {
            let mut st = state();
            st.marker = None;
            st.area = None;
            st.fly_to(Camera { lat: fix.lat, lon: fix.lon, alt: fix.altitude_km() });
            st.me_since = f64::NAN;
            let said = (fix.display(), fix.spoken());
            st.me = Some(fix);
            said
        }
        Err(e) => (format!("No position: {e}"), "I can't tell where you are right now, Sir: the location service, the network lookup and the settings all came up empty.".into()),
    }
}

/// The map's own "where am I" button: the same fix as the voice command, nothing spoken.
pub fn locate_in_background(rt: &Handle) {
    let offline = offline();
    rt.spawn(async move {
        // Not logged: the coordinates stay on the map.
        let _ = locate_me(offline).await;
        crate::gui::refresh();
    });
}

/// Mountain ranges for "show me the mountains" far from any.
const RANGES: [(&str, f64, f64); 9] = [
    ("the Alps", 46.4, 9.6),
    ("the Himalayas", 28.2, 85.0),
    ("the Rocky Mountains", 39.6, -106.2),
    ("the Andes", -32.7, -70.0),
    ("the Caucasus", 43.0, 43.5),
    ("the Alaska Range", 63.1, -150.5),
    ("the Southern Alps", -43.6, 170.3),
    ("the Atlas Mountains", 31.1, -7.9),
    ("the Pyrenees", 42.7, 0.6),
];

/// The range nearest a point.
pub fn nearest_range(lat: f64, lon: f64) -> (&'static str, f64, f64) {
    RANGES.into_iter().min_by(|a, b| haversine_km((lat, lon), (a.1, a.2)).total_cmp(&haversine_km((lat, lon), (b.1, b.2)))).expect("ranges")
}

/// "Show me the mountains": the shaded terrain, the peaks named up close; over a place, or the
/// range nearest the view when the camera is far out.
async fn mountains(place: Option<String>, offline: bool) -> (String, String) {
    let target = state().target();
    let goal = match place {
        Some(name) => match resolve(&name, offline).await {
            Ok(hit) => Some((hit.name, hit.lat, hit.lon)),
            Err(e) => return (e, format!("I couldn't find {name} on the map, Sir.")),
        },
        None if target.alt > 2_600.0 => {
            let (name, lat, lon) = nearest_range(target.lat, target.lon);
            Some((name.to_string(), lat, lon))
        }
        None => None,
    };
    let mut st = state();
    st.overlays.terrain = true;
    st.set_look(Look::Satellite);
    match goal {
        Some((name, lat, lon)) => {
            st.fly_to(Camera { lat, lon, alt: 520.0 });
            (format!("Terrain over {name}: shaded relief (AWS Terrain Tiles), peaks from OpenStreetMap."), format!("Here are the mountains of {name}, Sir, with the peaks named as you zoom in."))
        }
        None => ("Terrain: shaded relief, peaks named up close.".into(), "The terrain is shaded, Sir, with the peaks named.".into()),
    }
}

/// A day of NASA's daily imagery, over a place when one is named.
async fn historic(date: NaiveDate, place: Option<String>, offline: bool) -> (String, String) {
    let hit = match place.as_deref().filter(|p| !p.eq_ignore_ascii_case("here")) {
        Some(name) => match resolve(name, offline).await {
            Ok(hit) => Some(hit),
            Err(e) => return (e, format!("I couldn't find {name} on the map, Sir.")),
        },
        None => None,
    };
    let when = src::spoken_date(date);
    let mut st = state();
    st.historic = Some(date);
    st.set_look(Look::Satellite);
    st.overlays.clouds = false;
    let display = format!("NASA VIIRS true colour of {}.", date.format("%Y-%m-%d"));
    match hit {
        Some(hit) => {
            let alt = (hit.kind.altitude_km() * 1.6).min(MAX_ALT_KM);
            st.marker = (hit.kind != PlaceKind::Continent).then(|| Marker { name: hit.name.clone(), lat: hit.lat, lon: hit.lon });
            st.fly_to(Camera { lat: hit.lat, lon: hit.lon, alt });
            (display, format!("Here's {} on {when}, Sir, as NASA's satellites saw it.", hit.name))
        }
        None => (display, format!("Here's the Earth on {when}, Sir, clouds and all, as NASA's satellites saw it.")),
    }
}

/// "What's happening in the world?": the tour through the week's strongest quake, the storms and
/// the largest group of wildfires.
async fn world_tour(offline: bool) -> (String, String) {
    let (quakes_due, events_due) = {
        let mut st = state();
        let live = !offline && !st.demo;
        st.layers.quakes = true;
        st.layers.storms = true;
        st.layers.fires = true;
        (live && st.quakes.due(QUAKES_EVERY), live && st.events.due(EVENTS_EVERY))
    };
    let (quakes, events) = tokio::join!(
        async { if quakes_due { Some(fetch_quakes_week().await) } else { None } },
        async { if events_due { Some(fetch_events().await) } else { None } }
    );
    let mut st = state();
    if let Some(r) = quakes {
        st.quakes.set(r);
    }
    if let Some(r) = events {
        st.events.set(r);
    }
    let stops = crate::earth_tour::plan(&st.quakes.items, &st.events.items, Utc::now().timestamp_millis());
    let spoken = crate::earth_tour::opening(&stops);
    if stops.is_empty() {
        return ("Nothing to visit: no earthquakes, storms or wildfires are loaded.".into(), spoken);
    }
    let display = format!(
        "World tour, {} stops: {}",
        stops.len(),
        stops.iter().map(|s| format!("{} ({})", s.title, s.caption)).collect::<Vec<_>>().join("; ")
    );
    st.area = None;
    st.marker = None;
    st.spin = false;
    st.tour = Some(crate::earth_tour::Tour::new(stops));
    (display, spoken)
}

async fn global_quakes(offline: bool) -> (String, String) {
    let fetch = {
        let mut st = state();
        st.layers.quakes = true;
        let fetch = !offline && !st.demo && st.quakes.due(QUAKES_EVERY);
        if fetch {
            st.quakes.fetch = Fetch::Loading;
        }
        fetch
    };
    if fetch {
        let result = fetch_quakes_week().await;
        state().quakes.set(result);
    }
    let st = state();
    if let Fetch::Failed(e) = &st.quakes.fetch {
        if st.quakes.items.is_empty() {
            return (format!("USGS: {e}"), "The USGS isn't answering right now, Sir; I'll put the earthquakes up as soon as it does.".into());
        }
    }
    let list = st.quakes.items.clone();
    (format!("{} earthquakes of magnitude 4.5+ in the past week (USGS).", list.len()), global_quake_answer(&list))
}

async fn area_quakes(place: Option<String>, radius: Option<f64>, offline: bool) -> (String, String) {
    let radius = radius.unwrap_or(DEFAULT_RADIUS_KM).clamp(10.0, 5_000.0);
    let place = place.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    let (centre, area) = match place.as_deref() {
        None | Some("here") => {
            let t = state().target();
            ((t.lat, t.lon), AreaName::Here)
        }
        Some("my location") => match known_location(offline) {
            Some(p) => (p, AreaName::Mine),
            None => return ("No location of Sir's is known.".into(), "I don't know where you are yet, Sir. Name a place, or ask about the area in view.".into()),
        },
        Some(name) => match resolve(name, offline).await {
            Ok(hit) => {
                state().marker = Some(Marker { name: hit.name.clone(), lat: hit.lat, lon: hit.lon });
                ((hit.lat, hit.lon), AreaName::Named(hit.name))
            }
            Err(e) => return (e, format!("I couldn't find {name} on the map, Sir.")),
        },
    };
    let (id, demo) = {
        let mut st = state();
        (st.begin_area(centre, radius), st.demo)
    };
    let header = format!("Earthquakes 0 - {} km around {} in the last {AREA_DAYS} days", group_digits(radius.round() as i64), coords_label(centre.0, centre.1));
    let found = if offline || demo {
        Ok(state().local_area(centre, radius))
    } else {
        let mut task = tokio::spawn(fetch_area(centre.0, centre.1, radius, AREA_DAYS, SLOW_TIMEOUT));
        match tokio::time::timeout(FETCH_TIMEOUT, &mut task).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => {
                // Too slow for the answer: it comes later, said as soon as Jarvis is free.
                tokio::spawn(async move {
                    let Ok(result) = task.await else { return };
                    let line = match &result {
                        Ok(list) => quake_answer(list, radius, &area, Utc::now().timestamp_millis()),
                        Err(_) => "The USGS never answered about that area, Sir.".to_string(),
                    };
                    deliver(Arrival::Area(id, result));
                    crate::announcements::say(line);
                });
                return (format!("{header}: still waiting for the USGS."), "Still waiting on the USGS, Sir. I'll tell you as soon as the results are in.".into());
            }
        }
    };
    let now_ms = Utc::now().timestamp_millis();
    match found {
        Ok(list) => {
            let spoken = quake_answer(&list, radius, &area, now_ms);
            state().area_result(id, Ok(list.clone()));
            let rows: Vec<String> = list.iter().take(10).map(|q| format!("- M{:.1} {} ({})", q.mag, q.place, ago((now_ms - q.time_ms) / 1000))).collect();
            let display = if rows.is_empty() { format!("{header}: none.") } else { format!("{header}: {}\n{}", list.len(), rows.join("\n")) };
            (display, spoken)
        }
        Err(e) => {
            state().area_result(id, Err(e.clone()));
            (format!("{header}: USGS error ({e})."), "The USGS isn't answering right now, Sir.".into())
        }
    }
}

async fn flights(place: Option<String>, offline: bool) -> (String, String) {
    state().layers.flights = true;
    let Some(name) = place else {
        let alt = state().target().alt;
        return if alt > FLIGHTS_MAX_ALT_KM {
            ("Flights on (they show once the map is zoomed in).".into(), "The flights are on, Sir. Zoom in on an area and I'll show you the aircraft there.".into())
        } else {
            ("Flights on: OpenSky Network.".into(), "The live flights are on, Sir.".into())
        };
    };
    let hit = match resolve(&name, offline).await {
        Ok(hit) => hit,
        Err(e) => return (e, format!("I couldn't find {name} on the map, Sir.")),
    };
    let bbox = Bbox::around(hit.lat, hit.lon, 3.0, 4.5);
    let demo = {
        let mut st = state();
        st.marker = Some(Marker { name: hit.name.clone(), lat: hit.lat, lon: hit.lon });
        st.area = None;
        st.fly_to(Camera { lat: hit.lat, lon: hit.lon, alt: 700.0 });
        if !(offline || st.demo) {
            st.flights.fetch = Fetch::Loading;
        }
        st.demo
    };
    let count = if offline || demo {
        Ok(state().flights.items.iter().filter(|f| bbox.contains(f.lat, f.lon)).count())
    } else {
        let (result, backoff) = fetch_flights(bbox).await;
        let count = result.as_ref().map(Vec::len).map_err(|e| (e.clone(), backoff.is_some_and(|b| b >= Duration::from_secs(600))));
        deliver(Arrival::Flights(bbox, result, backoff));
        count
    };
    match count {
        Ok(0) => (format!("No aircraft over {}.", hit.name), format!("I don't see any aircraft over {} right now, Sir.", hit.name)),
        Ok(n) => (format!("{n} aircraft over {} (OpenSky Network).", hit.name), format!("There are {} aircraft over {} right now, Sir.", group_digits(n as i64), hit.name)),
        Err((e, true)) => (format!("OpenSky: {e}"), "OpenSky is rate-limiting me for a few minutes, Sir; the flights will follow.".into()),
        Err((e, false)) => (format!("OpenSky: {e}"), "The flight data isn't available right now, Sir.".into()),
    }
}

async fn events(kind: EventKind, offline: bool) -> (String, String) {
    let fetch = {
        let mut st = state();
        match kind {
            EventKind::Storm => st.layers.storms = true,
            EventKind::Fire => st.layers.fires = true,
        }
        let fetch = !offline && !st.demo && st.events.due(EVENTS_EVERY);
        if fetch {
            st.events.fetch = Fetch::Loading;
        }
        fetch
    };
    if fetch {
        let result = fetch_events().await;
        state().events.set(result);
    }
    let st = state();
    if let (Fetch::Failed(e), true) = (&st.events.fetch, st.events.items.is_empty()) {
        return (format!("NASA EONET: {e}"), "NASA's event tracker isn't answering right now, Sir.".into());
    }
    let list: Vec<GeoEvent> = st.events.items.iter().filter(|e| e.kind == kind).cloned().collect();
    match kind {
        EventKind::Storm => (format!("{} active storms (NASA EONET): {}", list.len(), list.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(", ")), storm_answer(&list)),
        EventKind::Fire => (format!("{} active wildfires (NASA EONET).", list.len()), fire_answer(list.len())),
    }
}

async fn iss(offline: bool) -> (String, String) {
    let demo = {
        let mut st = state();
        st.layers.iss = true;
        st.demo
    };
    let position = if offline || demo {
        state().iss.items.last().copied().ok_or_else(|| "no position".to_string())
    } else {
        let result = fetch_iss().await;
        if let Ok(now) = result {
            deliver(Arrival::Iss(Ok(now)));
        }
        result
    };
    let iss = match position {
        Ok(iss) => iss,
        Err(e) => return (format!("wheretheiss.at: {e}"), "I can't reach the space station's tracker right now, Sir.".into()),
    };
    {
        let mut st = state();
        let alt = st.target().alt.max(6_000.0);
        st.marker = None;
        st.fly_to(Camera { lat: iss.lat, lon: iss.lon, alt });
    }
    let over = if offline || demo { None } else { fetch_place(iss.lat, iss.lon).await.map(|p| p.spoken()).filter(|s| !s.is_empty()) };
    let over = over.unwrap_or_else(|| spoken_coords(iss.lat, iss.lon));
    (
        format!("ISS at {}, {} km up, {} km/h.", coords_label(iss.lat, iss.lon), iss.alt_km.round(), group_digits(iss.speed_kmh.round() as i64)),
        format!("The International Space Station is over {over}, {} kilometres up, Sir.", iss.alt_km.round() as i64),
    )
}

// -------------------------------------------------------------------------------- tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn near(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    /// The tile of zoom level `z` that holds a point (x east, y south, both from 0).
    fn tile_of(lat: f64, lon: f64, z: u8) -> (u32, u32) {
        let n = (1u64 << z) as f64;
        let x = (merc_x(wrap_lon(lon)) * n).floor().clamp(0.0, n - 1.0);
        let y = (merc_y(lat) * n).floor().clamp(0.0, n - 1.0);
        (x as u32, y as u32)
    }

    /// The north-west corner (lat, lon) of a tile.
    fn tile_corner(z: u8, x: u32, y: u32) -> (f64, f64) {
        let n = (1u64 << z) as f64;
        (lat_of_merc_y(y as f64 / n), x as f64 / n * 360.0 - 180.0)
    }

    #[test]
    fn lat_lon_round_trips_through_3d_and_the_screen() {
        for (lat, lon) in [(0.0, 0.0), (40.7128, -74.006), (-33.87, 151.21), (64.1, -21.9), (-10.0, 179.9)] {
            let (la, lo) = lat_lon(unit(lat, lon));
            assert!(near(la, lat, 1e-9) && near(wrap_lon(lo - lon), 0.0, 1e-9), "{lat},{lon} -> {la},{lo}");
        }
        let view = GlobeView::new(30.0, -40.0, HOME_ALT_KM, 760.0, (500.0, 300.0));
        for (lat, lon) in [(30.0, -40.0), (40.7128, -74.006), (10.0, -20.0), (55.0, -60.0), (5.0, -70.0)] {
            let (x, y, _) = view.project(lat, lon).expect("on the near side");
            let (la, lo) = view.unproject(x, y).expect("on the globe");
            assert!(near(la, lat, 1e-6) && near(wrap_lon(lo - lon), 0.0, 1e-6), "{lat},{lon} -> {la},{lo}");
        }
        // The middle of the screen is the camera's target, north is up.
        let (x, y, z) = view.project(30.0, -40.0).unwrap();
        assert!(near(x, 500.0, 1e-9) && near(y, 300.0, 1e-9) && near(z, 1.0, 1e-12));
        let (_, y_north, _) = view.project(40.0, -40.0).unwrap();
        assert!(y_north < 300.0);
        // Off the globe there is nothing to unproject.
        assert!(view.unproject(500.0 + view.limb_radius() + 2.0, 300.0).is_none());
        assert!(view.unproject(500.0 + view.limb_radius() - 2.0, 300.0).is_some());
    }

    #[test]
    fn the_far_side_is_culled_at_the_horizon_of_the_camera() {
        let view = GlobeView::new(0.0, 0.0, HOME_ALT_KM, 760.0, (0.0, 0.0));
        assert!(view.project(0.0, 180.0).is_none(), "the antipode is behind");
        assert!(view.project(0.0, 95.0).is_none(), "past the limb");
        // From one diameter up the camera sees acos(1/3) = 70.5° around its target.
        assert!(view.project(0.0, 70.0).is_some());
        assert!(view.project(0.0, 71.0).is_none());
        // The limb: the tangent from the eye, f / sqrt(d^2 - 1).
        let edge = view.screen(view.view_coords(0.0, (1.0 / 3.0f64).acos().to_degrees()));
        assert!(near(edge.0, view.limb_radius(), 0.5), "{} vs {}", edge.0, view.limb_radius());
        // The home globe is 0.42 of the shorter side.
        let f = focal_for(640.0);
        let home = GlobeView::new(0.0, 0.0, HOME_ALT_KM, f, (0.0, 0.0));
        assert!(near(home.limb_radius(), 0.42 * 640.0, 1e-6));
        // In the middle of the view the scale is f / altitude.
        assert!(near(home.px_per_km(), f / HOME_ALT_KM, 1e-12));
    }

    #[test]
    fn great_circles_and_distances() {
        let (ny, london) = ((40.7128, -74.006), (51.5072, -0.1276));
        assert!(near(haversine_km(ny, london), 5570.0, 15.0), "{}", haversine_km(ny, london));
        assert_eq!(great_circle(ny, london, 0.0), ny);
        let end = great_circle(ny, london, 1.0);
        assert!(near(end.0, london.0, 1e-9) && near(end.1, london.1, 1e-9));
        // The middle is half way along, and north of both (the route arcs over the Atlantic).
        let mid = great_circle(ny, london, 0.5);
        assert!(near(haversine_km(ny, mid), haversine_km(mid, london), 0.5));
        assert!(mid.0 > 51.5, "{mid:?}");
        // Antipodes still give a path.
        let p = great_circle((0.0, 0.0), (0.0, 180.0), 0.5);
        assert!(p.0.is_finite() && p.1.is_finite());
        // A circle of 500 km stays 500 km from its centre.
        for p in circle_points(ny, 500.0, 24) {
            assert!(near(haversine_km(ny, p), 500.0, 0.5));
        }
        assert!(near(haversine_km(ny, destination(ny, 90.0, 300.0)), 300.0, 0.01));
    }

    #[test]
    fn smoothing_eases_and_zoom_halves_in_about_four_tenths_of_a_second() {
        assert_eq!(smootherstep(0.0), 0.0);
        assert_eq!(smootherstep(1.0), 1.0);
        assert!(near(smootherstep(0.5), 0.5, 1e-12));
        assert!(smootherstep(0.1) < 0.01 && smootherstep(0.9) > 0.99);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert_eq!(smoothstep(1300.0, 1700.0, 1000.0), 0.0);
        // A wheel notch: the altitude target halves; after 0.4 s at 60 fps it is ~97 % there.
        let mut e = Earth::demo();
        let start = e.cam.alt;
        e.zoom_by(0.5);
        let mut t = 0.0;
        while t < 0.4 {
            e.step(t, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        let done = (start.ln() - e.cam.alt.ln()) / std::f64::consts::LN_2;
        assert!(done > 0.95 && done <= 1.0 + 1e-9, "{done}");
        for _ in 0..60 {
            e.step(t, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert_eq!(e.cam.alt, start / 2.0);
        assert!(e.zoom_target.is_none());
        assert!(near(smooth_towards(10.0, 0.0, 0.12, 0.12), 10.0 / std::f64::consts::E, 1e-9));
    }

    #[test]
    fn the_fly_in_comes_from_far_and_flights_follow_the_great_circle() {
        let mut e = Earth::new();
        e.reset_for_open(true);
        assert!(e.cam.alt > HOME_ALT_KM * 3.0, "starts far out: {}", e.cam.alt);
        e.step(0.0, 0.0);
        e.step(INTRO_SECS * 0.5, 0.2);
        let mid = e.cam.alt;
        e.step(INTRO_SECS + 0.01, 0.2);
        assert!(near(e.cam.alt, HOME_ALT_KM, 1e-6) && mid > HOME_ALT_KM);
        assert!(e.motion.is_none());
        // A flight to Tokyo: through the great circle, ending on target.
        let to = Camera { lat: 35.6895, lon: 139.6917, alt: 2_600.0 };
        e.fly_to(to);
        let secs = fly_secs(e.cam, to);
        assert!((1.0..=1.5).contains(&secs));
        e.step(1.0, 0.0);
        e.step(1.0 + secs / 2.0, 0.0);
        let half = e.cam;
        assert!(half.alt > 2_600.0, "lifted on the way: {}", half.alt);
        e.step(1.0 + secs + 0.01, 0.0);
        assert!(near(e.cam.lat, to.lat, 1e-6) && near(e.cam.lon, to.lon, 1e-6) && near(e.cam.alt, to.alt, 1e-6));
        // Asked during the fly-in, a flight waits for it.
        e.reset_for_open(true);
        e.fly_to(to);
        assert!(matches!(e.motion, Some(Motion::Intro { .. })) && e.queued.is_some());
        assert_eq!(e.target().lat, to.lat);
    }

    #[test]
    fn web_mercator_tiles() {
        assert_eq!(tile_of(0.0, 0.0, 1), (1, 1));
        assert_eq!(tile_of(85.0, -179.9, 1), (0, 0));
        // New York at zoom 10.
        assert_eq!(tile_of(40.7128, -74.006, 10), (301, 385));
        let (lat, lon) = tile_corner(10, 301, 385);
        assert!(lat >= 40.7128 && lon <= -74.006);
        let (lat2, lon2) = tile_corner(10, 302, 386);
        assert!(lat2 < 40.7128 && lon2 > -74.006);
        for lat in [-80.0, -45.0, 0.0, 33.3, 66.6, 85.0] {
            assert!(near(lat_of_merc_y(merc_y(lat)), lat, 1e-9));
        }
        // The flat view: a point maps to the screen and back; the tiles cover the view.
        let f = focal_for(600.0);
        let view = FlatView::new(40.7, -74.0, 1_100.0, f, (400.0, 300.0));
        let (x, y) = view.project(42.0, -71.0);
        let (la, lo) = view.unproject(x, y);
        assert!(near(la, 42.0, 1e-6) && near(lo, -71.0, 1e-6));
        // Its scale in the middle is the globe's at the same altitude.
        let globe = GlobeView::new(40.7, -74.0, 1_100.0, f, (400.0, 300.0));
        assert!(near(view.px_per_km(), globe.px_per_km(), 1e-9));
        let z = view.zoom().round() as u8;
        let tiles = view.tiles([0.0, 0.0, 800.0, 600.0], z);
        let (cx, cy) = tile_of(40.7, -74.0, z);
        assert_eq!(tiles[0], (cx as i64, cy as i64), "the middle tile first");
        for corner in [(0.0, 0.0), (799.0, 0.0), (0.0, 599.0), (799.0, 599.0)] {
            assert!(tiles.iter().any(|t| {
                let r = view.tile_rect(z, t.0, t.1);
                (r[0]..r[2]).contains(&corner.0) && (r[1]..r[3]).contains(&corner.1)
            }), "corner {corner:?} uncovered");
        }
        // Across the antimeridian x runs on (and wraps for the tile itself).
        let pacific = FlatView::new(0.0, 179.5, 1_000.0, f, (400.0, 300.0));
        let t = pacific.tiles([0.0, 0.0, 800.0, 600.0], 6);
        assert!(t.iter().any(|t| t.0 >= 64) && t.iter().all(|t| t.0.rem_euclid(64) < 64));
        assert_eq!(TileKey { layer: TileLayer::Labels, z: 10, x: 301, y: 385 }.parent(), Some(TileKey { layer: TileLayer::Labels, z: 9, x: 150, y: 192 }));
    }

    #[test]
    fn an_area_fits_the_map_whatever_its_size() {
        for min_side in [260.0, 460.0, 900.0] {
            for radius in [200.0, DEFAULT_RADIUS_KM, 1_500.0] {
                let alt = area_altitude(radius);
                let on_screen = FlatView::new(40.7, -74.0, alt, focal_for(min_side), (0.0, 0.0)).px_per_km() * radius;
                assert!(on_screen < 0.5 * min_side && on_screen > 0.3 * min_side, "{radius} km on {min_side} px: {on_screen:.0} px");
            }
        }
        // The default area is shown on the flat map.
        assert!(area_altitude(DEFAULT_RADIUS_KM) < FLAT_FULL_KM);
    }

    #[test]
    fn haversine_filters_an_area() {
        let e = Earth::demo();
        // Around New York nothing shook in the demo; around Popayan the M5.2 did.
        assert!(e.local_area((40.7128, -74.006), 500.0).is_empty());
        let found = e.local_area((2.44, -76.61), 500.0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].mag, 5.2);
        // Two Alaskan quakes within 1 500 km of Anchorage, strongest first.
        let alaska = e.local_area((61.2181, -149.9), 1_500.0);
        assert_eq!(alaska.iter().map(|q| q.mag).collect::<Vec<_>>(), vec![5.0, 4.6]);
    }

    #[test]
    fn footprint_offsets_and_labels() {
        // The ISS at ~420 km sees ~2 250 km around it (wheretheiss.at's footprint is ~4 500 km across).
        assert!(near(footprint_radius_km(420.0), 2_250.0, 30.0), "{}", footprint_radius_km(420.0));
        assert_eq!(footprint_radius_km(0.0), 0.0);
        assert_eq!(utc_offset_label(3 * 3600), "UTC+03:00");
        assert_eq!(utc_offset_label(-4 * 3600), "UTC\u{2212}04:00");
        assert_eq!(utc_offset_label(19_800), "UTC+05:30");
        assert_eq!(utc_offset_label(0), "UTC+00:00");
        assert_eq!(estimate_offset_secs(-74.0), -5 * 3600);
        assert_eq!(estimate_offset_secs(139.7), 9 * 3600);
        assert_eq!(group_digits(12_742), "12 742");
        assert_eq!(group_digits(734), "734");
        assert_eq!(group_digits(1_234_567), "1 234 567");
        assert_eq!(nice_scale(0.5, 120.0), Some((200.0, 100.0)));
        assert_eq!(nice_scale(0.1, 120.0).map(|s| s.0), Some(1_000.0));
        assert_eq!(distance_label(500.0), "500 km");
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(3 * 3600 + 5), "3 h ago");
        assert_eq!(spoken_ago(2 * 86_400), "2 days ago");
        assert_eq!(spoken_ago(3_600), "1 hour ago");
        assert_eq!(coords_label(40.7127, -74.006), "40.7127, -74.0060");
        assert_eq!(storm_label("Tropical Storm Odalys", Some(2026)), "ODALYS-26");
        assert_eq!(storm_label("Super Typhoon Man-yi", Some(2024)), "MAN-YI-24");
        assert_eq!(clean_country("United States of America (the)"), "United States");
        assert_eq!(clean_country("Iran (Islamic Republic of)"), "Iran");
        assert_eq!(clean_country("Netherlands (the)"), "Netherlands");
        let cell = cell_of(40.71, -74.0, 0.5);
        let (lat, lon) = cell_centre(cell);
        assert!(near(lat, 40.75, 1e-9) && near(lon, -73.75, 1e-9));
    }

    #[test]
    fn parses_usgs_eonet_iss_opensky_and_open_meteo_samples() {
        let usgs = json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "id": "us7000abcd", "properties": {"mag": 5.1, "place": "12 km SSW of Tadotsu, Japan", "time": 1_790_000_000_000i64},
             "geometry": {"type": "Point", "coordinates": [133.7, 34.2, 35.5]}},
            {"type": "Feature", "id": "nomag", "properties": {"mag": null, "place": "x"}, "geometry": {"type": "Point", "coordinates": [1.0, 2.0, 3.0]}}
        ]});
        let quakes = parse_quakes(&usgs);
        assert_eq!(quakes.len(), 1);
        assert_eq!((quakes[0].mag, quakes[0].lat, quakes[0].lon, quakes[0].depth_km), (5.1, 34.2, 133.7, 35.5));
        assert_eq!(quakes[0].place, "12 km SSW of Tadotsu, Japan");

        let eonet = json!({"events": [
            {"id": "EONET_1", "title": "Tropical Storm Odalys", "categories": [{"id": "severeStorms", "title": "Severe Storms"}],
             "geometry": [{"date": "2026-09-19T00:00:00Z", "type": "Point", "coordinates": [-110.0, 17.0]},
                          {"date": "2026-09-20T12:00:00Z", "type": "Point", "coordinates": [-112.0, 19.5]}]},
            {"id": "EONET_2", "title": "Smith Fire, Oklahoma", "categories": [{"id": "wildfires"}],
             "geometry": [{"date": "2026-09-20T00:00:00Z", "type": "Polygon", "coordinates": [[[-98.0, 34.0], [-97.0, 34.0], [-97.0, 35.0], [-98.0, 35.0]]]}]},
            {"id": "EONET_3", "title": "Volcano", "categories": [{"id": "volcanoes"}], "geometry": [{"date": "2026-09-20T00:00:00Z", "type": "Point", "coordinates": [0.0, 0.0]}]}
        ]});
        let events = parse_events(&eonet);
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].kind, events[0].label.as_str(), events[0].lat, events[0].lon), (EventKind::Storm, "ODALYS-26", 19.5, -112.0));
        assert_eq!((events[1].kind, events[1].lat, events[1].lon), (EventKind::Fire, 34.5, -97.5));

        let iss = parse_iss(&json!({"name": "iss", "id": 25544, "latitude": -12.3, "longitude": -30.4, "altitude": 418.2, "velocity": 27580.1, "footprint": 4489.0, "timestamp": 1_790_000_000})).unwrap();
        assert_eq!((iss.lat, iss.lon, iss.alt_km, iss.at), (-12.3, -30.4, 418.2, 1_790_000_000));
        let track = parse_iss_track(&json!([{"latitude": 2.0, "longitude": 3.0, "timestamp": 20}, {"latitude": 1.0, "longitude": 2.0, "timestamp": 10}]));
        assert_eq!(track, vec![(1.0, 2.0), (2.0, 3.0)]);

        let opensky = json!({"time": 1_790_000_000, "states": [
            ["a1b2c3", "AAL1075 ", "United States", 1_790_000_000, 1_790_000_001, -73.9, 40.6, 10668.0, false, 240.6, 45.0, 0.0, null, 10700.0, "1234", false, 0],
            ["ffffff", null, "United States", null, 1_790_000_000, -74.0, 40.7, null, true, 0.0, 0.0, null, null, null, null, false, 0],
            ["000001", "", "Canada", null, 1_790_000_000, null, null, null, false, null, null, null, null, null, null, false, 0]
        ]});
        let flights = parse_flights(&opensky);
        assert_eq!(flights.len(), 1);
        assert_eq!(flights[0].callsign, "AAL1075");
        assert_eq!(flights[0].details(), "10 668 m \u{b7} 866 km/h");
        let mut typed = flights[0].clone();
        typed.kind = Some("A320".into());
        assert_eq!(typed.details(), "A320 \u{b7} 10 668 m \u{b7} 866 km/h");
        // Flying on: ~66.8 m/s north-east... a minute later the plane is ~14 km on.
        let (la, lo) = flights[0].reckoned(60.0);
        assert!(near(haversine_km((40.6, -73.9), (la, lo)), 240.6 * 3.6 / 60.0, 0.05) && la > 40.6 && lo > -73.9);

        let meteo = json!({"latitude": 38.7, "longitude": -80.6, "utc_offset_seconds": -14400, "timezone": "America/New_York",
            "current": {"time": "2026-09-21T15:30", "temperature_2m": 18.1, "apparent_temperature": 19.4, "relative_humidity_2m": 94, "wind_speed_10m": 8.0, "wind_direction_10m": 225, "weather_code": 3},
            "daily": {"time": ["2026-09-21"], "sunrise": ["2026-09-21T06:58"], "sunset": ["2026-09-21T19:10"]}});
        let w = parse_weather(&meteo).unwrap();
        assert_eq!((w.temp, w.feels, w.humidity, w.offset_secs), (18.1, 19.4, Some(94.0), -14_400));
        assert_eq!((w.sunrise.as_deref(), w.sunset.as_deref(), w.wind_from), (Some("06:58"), Some("19:10"), Some(225.0)));

        let geo = json!({"results": [{"id": 1, "name": "Tokyo", "latitude": 35.6895, "longitude": 139.69171, "feature_code": "PPLC", "country": "Japan"}]});
        assert_eq!(parse_geocode(&geo).unwrap(), GeoHit { name: "Tokyo".into(), lat: 35.6895, lon: 139.69171, kind: PlaceKind::City });
        assert!(parse_geocode(&json!({"generationtime_ms": 0.5})).is_none());

        let place = parse_reverse(&json!({"countryName": "United States of America (the)", "principalSubdivision": "West Virginia", "city": "", "locality": "Charleston"})).unwrap();
        assert_eq!((place.name(false), place.name(true), place.city.as_str()), ("United States".into(), "West Virginia".into(), "Charleston"));
        let sea = parse_reverse(&json!({"countryName": "", "principalSubdivision": "", "locality": "", "localityInfo": {"informative": [{"name": "Earth"}, {"name": "North Atlantic Ocean"}]}})).unwrap();
        assert_eq!((sea.name(false), sea.spoken()), ("North Atlantic Ocean".into(), "the North Atlantic Ocean".into()));
    }

    #[test]
    fn spoken_answers_for_none_one_and_several_quakes() {
        let now = 1_790_000_000_000i64;
        let q = |mag: f64, place: &str, hours: i64| Quake { id: String::new(), mag, place: place.into(), time_ms: now - hours * 3_600_000, lat: 0.0, lon: 0.0, depth_km: 10.0 };
        assert_eq!(quake_answer(&[], 500.0, &AreaName::Here, now), "There have been no recent earthquakes in this area, Sir.");
        assert_eq!(quake_answer(&[], 300.0, &AreaName::Named("Naples".into()), now), "There have been no earthquakes within 300 kilometres of Naples in the past week, Sir.");
        assert_eq!(quake_answer(&[q(4.2, "35 km SW of Kyoto, Japan", 50)], 500.0, &AreaName::Here, now), "One in the last week, Sir: a magnitude 4.2 near Kyoto, Japan, 2 days ago.");
        let several = [q(3.1, "10 km N of Somewhere, Italy", 3), q(5.1, "5 km E of Pozzuoli, Italy", 20), q(2.7, "Tyrrhenian Sea", 100)];
        assert_eq!(quake_answer(&several, 500.0, &AreaName::Here, now), "Three in the last week, Sir; the strongest a magnitude 5.1 near Pozzuoli, Italy.");
        assert!(global_quake_answer(&several).contains("3 of magnitude 4.5 or more, the strongest a 5.1 near Pozzuoli, Italy"));
        // USGS names without "of": a region, a sea, a ridge, islands.
        assert!(global_quake_answer(&[q(6.1, "Fiji region", 5)]).ends_with("the strongest a 6.1 in the Fiji region."));
        assert_eq!(near_phrase("Banda Sea"), " in the Banda Sea");
        assert_eq!(near_phrase("central Mid-Atlantic Ridge"), " on the central Mid-Atlantic Ridge");
        assert_eq!(near_phrase("South Sandwich Islands"), " near the South Sandwich Islands");
        assert_eq!(near_phrase("Fiji"), " near Fiji");
        assert_eq!(near_phrase("  "), "");
        let storms = Earth::demo().events.items.into_iter().filter(|e| e.kind == EventKind::Storm).collect::<Vec<_>>();
        assert_eq!(storm_answer(&storms), "Three storms are active, Sir: Odalys, Polo and Fay.");
        assert_eq!(storm_answer(&[]), "No storms are being tracked right now, Sir.");
        assert_eq!(fire_answer(26), "NASA is tracking 26 active wildfires, Sir; they are marked on the globe.");
    }

    #[test]
    fn arguments_become_actions() {
        let a = |v: Value| parse_action(&v);
        assert_eq!(a(json!({"view": "earth"})), Ok(Action::Show));
        assert_eq!(a(json!({"view": "earth", "action": "show_satellite"})), Ok(Action::Satellite));
        assert_eq!(a(json!({"action": "show_holo"})), Ok(Action::Holo));
        assert_eq!(a(json!({"action": "show_place", "place": "Tokyo"})), Ok(Action::Place("Tokyo".into())));
        assert!(a(json!({"action": "show_place"})).is_err());
        assert_eq!(a(json!({"view": "earth", "place": "New York"})), Ok(Action::Place("New York".into())));
        assert_eq!(a(json!({"action": "show_quakes", "place": "here"})), Ok(Action::Quakes { place: Some("here".into()), radius_km: None }));
        assert_eq!(a(json!({"action": "show_quakes", "place": "Naples", "radius_km": 300})), Ok(Action::Quakes { place: Some("Naples".into()), radius_km: Some(300.0) }));
        assert_eq!(a(json!({"action": "show_quakes", "radius_km": "200"})), Ok(Action::Quakes { place: None, radius_km: Some(200.0) }));
        assert_eq!(a(json!({"action": "show_flights", "place": "New York"})), Ok(Action::Flights(Some("New York".into()))));
        assert_eq!(a(json!({"action": "show_flights"})), Ok(Action::Flights(None)));
        assert_eq!(a(json!({"action": "hide_layer", "layer": "flights"})), Ok(Action::Hide(Layer::Flights)));
        assert!(a(json!({"action": "hide_layer", "layer": "volcanoes"})).is_err());
        assert_eq!(a(json!({"action": "hide_layer", "layer": "clouds"})), Ok(Action::Overlay(Overlay::Clouds, false)));
        assert_eq!(a(json!({"action": "hide_layer", "layer": "rain radar"})), Ok(Action::Overlay(Overlay::Radar, false)));
        assert_eq!(a(json!({"action": "hide_layers"})), Ok(Action::HideAll));
        for (name, action) in [("show_storms", Action::Storms), ("show_fires", Action::Fires), ("show_iss", Action::Iss), ("zoom_in", Action::ZoomIn), ("zoom_out", Action::ZoomOut), ("home", Action::Home), ("spin", Action::Spin), ("stop_spin", Action::StopSpin)] {
            assert_eq!(a(json!({"action": name})), Ok(action));
        }
        assert!(a(json!({"action": "launch_missiles"})).is_err());
        // Locate, terrain, weather, imagery, dates, the tour.
        let new = [
            ("show_my_location", Action::Locate),
            ("locate_me", Action::Locate),
            ("show_world_tour", Action::Tour),
            ("show_terrain", Action::Mountains(None)),
            ("exaggerate_terrain", Action::Exaggerate(true)),
            ("flatten_terrain", Action::Exaggerate(false)),
            ("show_radar", Action::Radar { animate: false }),
            ("animate_radar", Action::Radar { animate: true }),
            ("show_clouds", Action::Clouds),
            ("show_labels", Action::Overlay(Overlay::Labels, true)),
            ("hide_borders", Action::Overlay(Overlay::Labels, false)),
            ("show_today", Action::Today),
            ("world_tour", Action::Tour),
            ("stop_tour", Action::StopTour),
        ];
        for (name, action) in new {
            assert_eq!(a(json!({"action": name})), Ok(action), "{name}");
        }
        assert_eq!(a(json!({"action": "show_mountains", "place": "Nepal"})), Ok(Action::Mountains(Some("Nepal".into()))));
        assert_eq!(a(json!({"action": "set_imagery", "imagery": "Sentinel-2"})), Ok(Action::Imagery(Imagery::Sentinel)));
        assert!(a(json!({"action": "set_imagery", "imagery": "crayons"})).is_err());
        let march = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
        assert_eq!(a(json!({"action": "show_date", "date": "2024-03-01", "place": "Europe"})), Ok(Action::Historic { date: march, place: Some("Europe".into()) }));
        assert_eq!(a(json!({"action": "show_clouds", "date": "1 March 2024"})), Ok(Action::Historic { date: march, place: None }));
        assert!(a(json!({"action": "show_date"})).is_err(), "a date is needed");
        assert!(a(json!({"action": "show_date", "date": "next Tuesday"})).is_err());
    }

    #[test]
    fn continents_ranges_and_the_land_mask() {
        let europe = continent("Europa").expect("German too");
        assert_eq!((europe.name.as_str(), europe.kind), ("Europe", PlaceKind::Continent));
        assert!(continent("the Atlantic").is_some() && continent("Narnia").is_none());
        assert_eq!(nearest_range(47.0, 8.0).0, "the Alps");
        assert_eq!(nearest_range(-20.0, -68.0).0, "the Andes");
        // Relief: land is light, sea dark; the lines' alpha is the border channel.
        let relief = image::RgbaImage::from_fn(32, 8, |x, _| if x < 16 { image::Rgba([10, 30, 90, 255]) } else { image::Rgba([90, 120, 60, 255]) });
        let lines = image::RgbaImage::from_fn(32, 8, |x, _| image::Rgba([255, 255, 255, if x == 24 || x == 16 { 255 } else { 0 }]));
        let mask = gpu_mask(&relief, Some(&lines));
        assert_eq!(mask.len(), 32 * 8 * 2);
        let at = |x: usize, y: usize| (mask[(y * 32 + x) * 2], mask[(y * 32 + x) * 2 + 1]);
        assert!(at(6, 4).0 < 5 && at(24, 4).0 > 250, "sea then land: {:?} {:?}", at(6, 4), at(24, 4));
        assert!(at(15, 4).0 > 40 && at(15, 4).0 < at(16, 4).0 && at(16, 4).0 < 215, "a ramp across the coast: {:?} {:?}", at(15, 4), at(16, 4));
        assert_eq!((at(24, 4).1, at(23, 4).1), (255, 0), "a border only where the lines are");
        assert_eq!(at(16, 4).1, 0, "on the coast the line is the coast's");
    }

    #[test]
    fn tile_layers_name_their_sources() {
        let key = |layer, z, x, y| TileKey { layer, z, x, y };
        assert_eq!(source_urls(key(TileLayer::Esri, 12, 2140, 1420), None), vec![Imagery::Esri.url(12, 2140, 1420)]);
        assert!(source_urls(key(TileLayer::Terrain(25), 8, 134, 90), None)[0].ends_with("/terrarium/8/134/90.png"));
        assert_eq!(source_urls(key(TileLayer::Clouds(7), 2, 1, 1), None).len(), 3, "one per geostationary satellite");
        let frame = RadarFrame { time: 1_794_178_200, path: "/v2/radar/2c5".into() };
        assert!(source_urls(key(TileLayer::Radar(1_794_178_200), 3, 4, 2), None).is_empty(), "no frame list yet");
        assert_eq!(source_urls(key(TileLayer::Radar(1_794_178_200), 3, 4, 2), Some(("https://tilecache.rainviewer.com", &frame))), vec!["https://tilecache.rainviewer.com/v2/radar/2c5/256/3/4/2/2/1_1.png".to_string()]);
        let march = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
        let daily = TileLayer::day(march);
        assert_eq!(daily.date(), Some(march));
        assert!(source_urls(key(daily, 4, 8, 5), None)[0].contains("/VIIRS_SNPP_CorrectedReflectance_TrueColor/default/2024-03-01/GoogleMapsCompatible_Level9/4/5/8.jpg"));
        assert!(source_urls(key(TileLayer::Holo, 2, 1, 1), None).is_empty(), "probed GIBS layers go by their templates");
        assert_eq!(TileLayer::of(Imagery::Nasa), TileLayer::Satellite);
        assert!(!TileLayer::Radar(1).cached_on_disk() && TileLayer::Esri.cached_on_disk());
    }

    /// `run` end to end on the demo (tests are always offline): open, the reference's quake
    /// query, a place, a layer off, close.
    #[test]
    fn run_answers_from_the_demo_and_closes() {
        let _one = test_lock();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        close();
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show"}))).unwrap();
        assert!(is_open());
        assert!(out.ends_with("Spoken: The Earth is on your display, Sir. You can spin or zoom it as you like."), "{out}");
        assert!(state().demo);
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_satellite"}))).unwrap();
        assert!(out.ends_with("Spoken: The satellite view is now active, Sir."));
        assert_eq!(state().look, Look::Satellite);
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_quakes", "place": "here"}))).unwrap();
        assert!(out.ends_with("Spoken: There have been no recent earthquakes in this area, Sir."), "{out}");
        {
            let st = state();
            let area = st.area.as_ref().expect("the drawer is open");
            assert_eq!((area.header(), area.results.as_deref().map(<[Quake]>::len)), ("0 - 500 KM - 7 D".to_string(), Some(0)));
            assert!(!st.layers.flights && st.layers.quakes);
        }
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_quakes", "place": "Lima", "radius_km": 600}))).unwrap();
        assert!(out.contains("Spoken: One in the last week, Sir: a magnitude 4.7 near Pisco, Peru"), "{out}");
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_place", "place": "tokyo"}))).unwrap();
        assert!(out.ends_with("Spoken: Here is Tokyo, Sir."), "{out}");
        assert_eq!(state().target().lat, 35.6895);
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_place", "place": "Atlantis"}))).unwrap();
        assert!(out.ends_with("Spoken: I couldn't find Atlantis on the map, Sir."), "{out}");
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_quakes", "place": "my location"}))).unwrap();
        assert!(out.contains("I don't know where you are yet, Sir."), "{out}");
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_storms"}))).unwrap();
        assert!(out.ends_with("Spoken: Three storms are active, Sir: Odalys, Polo and Fay."), "{out}");
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_iss"}))).unwrap();
        assert!(out.contains("The International Space Station is over 12 degrees north, 96 degrees west, 418 kilometres up, Sir."), "{out}");
        let out = rt.block_on(run(&json!({"view": "earth", "action": "show_flights", "place": "New York"}))).unwrap();
        assert!(out.contains("aircraft over New York right now, Sir."), "{out}");
        let out = rt.block_on(run(&json!({"view": "earth", "action": "hide_layer", "layer": "flights"}))).unwrap();
        assert!(out.ends_with("Spoken: The flights are off, Sir."));
        assert!(!state().layers.flights);
        assert!(rt.block_on(run(&json!({"view": "earth", "action": "warp"}))).is_err());
        close();
        assert!(!is_open());
    }

    /// The new actions on the demo: locate, mountains, weather, imagery, a date, the tour.
    #[test]
    fn locate_terrain_weather_dates_and_the_tour_on_the_demo() {
        let _one = test_lock();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        let go = |args: Value| rt.block_on(run(&args)).unwrap();
        close();
        let out = go(json!({"view": "earth", "action": "locate_me"}));
        assert!(out.ends_with("Spoken: There you are, Sir."), "{out}");
        {
            let st = state();
            let me = st.me.as_ref().expect("the YOU marker");
            assert_eq!(me.source, crate::earth_locate::Source::Demo);
            assert!(haversine_km((st.target().lat, st.target().lon), (me.lat, me.lon)) < 1.0, "the camera flies to him");
            assert!(st.near_me(me.lat + 0.1, me.lon) && !st.near_me(0.0, 0.0), "places near him are not looked up");
        }
        // Far out, "show me the mountains" goes to the range nearest the view.
        go(json!({"view": "earth", "action": "home"}));
        let out = go(json!({"view": "earth", "action": "show_mountains"}));
        assert!(out.ends_with("Spoken: Here are the mountains of the Rocky Mountains, Sir, with the peaks named as you zoom in."), "{out}");
        assert!(state().overlays.terrain && state().target().alt < 1_000.0);
        let out = go(json!({"view": "earth", "action": "show_mountains", "place": "Delhi"}));
        assert!(out.contains("Spoken: Here are the mountains of Delhi, Sir"), "{out}");
        let out = go(json!({"view": "earth", "action": "show_terrain"}));
        assert!(out.ends_with("Spoken: The terrain is shaded, Sir, with the peaks named."), "{out}");
        let out = go(json!({"view": "earth", "action": "exaggerate_terrain"}));
        assert!(out.ends_with("Spoken: Exaggerating the relief, Sir."), "{out}");
        assert!(state().overlays.exaggerate);
        assert_eq!(state().terrain_layer(), Some(TileLayer::Terrain(25)));
        let out = go(json!({"view": "earth", "action": "show_radar"}));
        assert!(out.ends_with("Spoken: The rain radar is on, Sir."), "{out}");
        let out = go(json!({"view": "earth", "action": "show_clouds"}));
        assert!(out.ends_with("Spoken: The live cloud cover is on, Sir."), "{out}");
        assert!(state().overlays.clouds && state().overlays.radar);
        let out = go(json!({"view": "earth", "action": "hide_layer", "layer": "radar"}));
        assert!(out.ends_with("Spoken: The rain radar is off, Sir."), "{out}");
        let out = go(json!({"view": "earth", "action": "set_imagery", "imagery": "sentinel"}));
        assert!(out.ends_with("Spoken: Switching to the Sentinel-2 cloudless imagery, Sir."), "{out}");
        assert_eq!(state().imagery_layer(), TileLayer::Sentinel);
        let out = go(json!({"view": "earth", "action": "show_date", "date": "2024-03-01", "place": "Europe"}));
        assert!(out.contains("2024-03-01") && out.ends_with("Spoken: Here's Europe on 1 March 2024, Sir, as NASA's satellites saw it."), "{out}");
        assert_eq!(state().imagery_layer(), TileLayer::day(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()));
        assert!(!state().overlays.clouds, "the day's own clouds, not today's");
        let out = go(json!({"view": "earth", "action": "show_today"}));
        assert!(out.ends_with("Spoken: Back to today's imagery, Sir."), "{out}");
        assert!(state().historic.is_none());
        let out = go(json!({"view": "earth", "action": "world_tour"}));
        assert!(out.contains("Spoken: Here's the world right now, Sir."), "{out}");
        assert!(state().tour.is_some() && state().layers.quakes);
        let out = go(json!({"view": "earth", "action": "stop_tour"}));
        assert!(out.ends_with("Spoken: Stopping the tour, Sir."), "{out}");
        assert!(state().tour.is_none());
        let out = go(json!({"view": "earth", "action": "stop_tour"}));
        assert!(out.ends_with("Spoken: There's no tour running, Sir."), "{out}");
        // Another order ends a tour.
        go(json!({"view": "earth", "action": "world_tour"}));
        go(json!({"view": "earth", "action": "zoom_in"}));
        assert!(state().tour.is_none());
        close();
        let_go();
        assert!(state().me.is_some(), "the fix stays for the session");
    }

    #[test]
    fn the_hologram_is_classified_and_coloured() {
        // A 6x4 relief: water on the left, land on the right, one blue speck inland.
        let mut relief = image::RgbImage::from_pixel(6, 4, image::Rgb([10, 30, 90]));
        for y in 0..4 {
            for x in 3..6 {
                relief.put_pixel(x, y, image::Rgb([90, 120, 60]));
            }
        }
        relief.put_pixel(4, 2, image::Rgb([20, 40, 120]));
        let mask = build_mask(&relief, None, None);
        assert_eq!(mask.texels.len(), 24);
        let at = |x: usize, y: usize| mask.texels[y * 6 + x][0];
        assert_eq!(at(0, 1) & LAND, 0, "water");
        assert_ne!(at(5, 1) & LAND, 0, "land");
        assert_ne!(at(4, 2) & LAND, 0, "the speck became land");
        assert_ne!(at(3, 1) & COAST, 0, "land next to water is coast");
        assert_eq!(at(4, 1) & COAST, 0);
        let p = holo_palette(206.0);
        let image = colour_holo(&mask, &p);
        assert_eq!(image.size, [6, 4]);
        assert_eq!(image.pixels[6 + 3], Color32::from_rgb(p.coast[0], p.coast[1], p.coast[2]));
        let ocean = image.pixels[6];
        assert!(ocean.b() > ocean.r() && ocean.b() < 160, "deep blue ocean: {ocean:?}");
        assert!(is_water([8, 22, 64]) && is_water([60, 130, 190]) && !is_water([240, 245, 250]) && !is_water([200, 170, 130]));
        // The deepest basins (near black) are the sea; dark forest and shadowed rock stay land.
        assert!(is_water([2, 5, 20]) && is_water([1, 3, 18]) && !is_water([15, 40, 20]) && !is_water([30, 28, 26]));
        let proc = procedural_texture(64, 32, 206.0);
        assert_eq!(proc.pixels.len(), 64 * 32);
    }

    #[test]
    fn the_iss_track_passes_through_the_station_and_stays_within_its_inclination() {
        let track = iss_ground_track(12.0, -96.0, true, &[-30.0, -10.0, 0.0, 10.0, 30.0]);
        assert!(near(track[2].0, 12.0, 1e-9) && near(track[2].1, -96.0, 1e-9));
        assert!(track.iter().all(|p| p.0.abs() <= 51.65));
        assert!(track[3].0 > track[2].0, "ascending: north next");
        assert!(track[3].1 > track[2].1, "and eastward");
    }

    #[test]
    fn the_cache_keeps_under_its_limit_oldest_first() {
        let dir = std::env::temp_dir().join(format!("jarvis_earth_cache_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tiles")).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join("tiles").join(format!("{i}.png")), vec![0u8; 1000]).unwrap();
            let file = std::fs::File::options().write(true).open(dir.join("tiles").join(format!("{i}.png"))).unwrap();
            file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000 + i as u64 * 100)).unwrap();
        }
        assert_eq!(trim_cache(&dir, 10_000, 8_000), 5_000, "under the limit nothing goes");
        assert_eq!(trim_cache(&dir, 4_000, 2_500), 2_000);
        assert!(!dir.join("tiles").join("0.png").exists() && !dir.join("tiles").join("2.png").exists());
        assert!(dir.join("tiles").join("3.png").exists() && dir.join("tiles").join("4.png").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn urls_carry_the_query_and_nothing_else() {
        let now = Utc.with_ymd_and_hms(2026, 9, 21, 19, 40, 0).unwrap();
        assert_eq!(
            area_url(40.71234, -74.0061, 500.0, 7, now),
            "https://earthquake.usgs.gov/fdsnws/event/1/query?format=geojson&latitude=40.712&longitude=-74.006&maxradiuskm=500&starttime=2026-09-14T19:40:00&minmagnitude=2.5&orderby=magnitude&limit=200"
        );
        assert_eq!(flights_url(&Bbox { south: 38.0, west: -78.0, north: 43.0, east: -70.0 }), "https://opensky-network.org/api/states/all?lamin=38.000&lomin=-78.000&lamax=43.000&lomax=-70.000");
        assert_eq!(geocode_url("New York"), "https://geocoding-api.open-meteo.com/v1/search?name=New%20York&count=1&language=en&format=json");
        assert!(tile_url(TileLayer::Labels, 0, 5, 9, 12).ends_with("/Reference_Labels_15m/default/GoogleMapsCompatible_Level13/5/12/9.png"));
        assert!(tile_url(TileLayer::Holo, 4, 2, 1, 3).ends_with("/BlueMarble_ShadedRelief_Bathymetry/default/default/GoogleMapsCompatible_Level8/2/3/1.jpg"));
        assert!(wms_url("Coastlines_15m", true, false).contains("BBOX=-90,-180,90,180&WIDTH=2048&HEIGHT=1024&FORMAT=image/png"));
        assert_eq!(USER_AGENT, "JARVIS/3 desktop assistant");
    }
}
