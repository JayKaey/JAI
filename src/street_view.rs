//! Street view (`show_street_view`): street-level photos of a place, 360° panoramas where there
//! are any, on Sir's HUD.
//!
//! * **Sources.** Mapillary's Graph API only when Sir stored a client token of his own in Quick
//!   Connections (sealed in the vault like the Discord bot's; never asked for in chat, never
//!   logged, sent as a header). Without one, keyless: Panoramax (the federated open street
//!   imagery, CC BY-SA; its `place_position` search finds pictures looking AT a landmark) and
//!   KartaView.
//! * **Places.** A name goes through the geocoders (open-meteo, then OpenStreetMap); "here" is
//!   the Earth's centre while the Earth is open, else Sir's own position.
//! * **Nothing nearby** is said plainly, with the distance to the nearest street-level photo.
//! * **The viewer** (`gui_media.rs`): an equirectangular panorama is painted inside a sphere
//!   (drag to look, the wheel zooms, ← → step along the sequence); a flat photo is framed.
//!
//! Tests and dry runs never touch the network: a stand-in panorama sequence (`offline`).

use crate::image_viewer::{offline, pixels};
use crate::tools::{bool_arg, str_arg, ToolContext};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tracing::{debug, info};

pub const TOOL: &str = "show_street_view";
/// The vault slot of Sir's Mapillary client token.
pub const VAULT_SERVICE: &str = "mapillary";
const TIMEOUT: Duration = Duration::from_secs(9);
/// Street view of a place: photos within this distance count as "at" it.
const NEARBY_KM: f64 = 1.0;
/// The nearest photo is looked for this far out when there is none nearby.
const SEARCH_FAR_KM: f64 = 30.0;
/// Panoramas are decoded to at most this many pixels a side.
pub const PANO_SIDE: u32 = 4096;
const PANORAMAX: &str = "https://api.panoramax.xyz/api";
const KARTAVIEW: &str = "https://api.kartaview.org/2.0/photo/";
const MAPILLARY: &str = "https://graph.mapillary.com";
const MAPILLARY_FIELDS: &str = "id,thumb_256_url,thumb_2048_url,computed_geometry,geometry,is_pano,captured_at,sequence,creator,compass_angle";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Mapillary,
    Panoramax,
    KartaView,
    Demo,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mapillary => "Mapillary",
            Self::Panoramax => "Panoramax",
            Self::KartaView => "KartaView",
            Self::Demo => "stand-in",
        }
    }
}

/// How to reach the next or previous photo of a sequence.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The item's address.
    Panoramax(String),
    KartaView { sequence: String, index: i64 },
    Mapillary { sequence: String, id: String },
    Demo(i64),
}

/// One street-level photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub provider: Provider,
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    /// Where the middle of the picture looks, degrees from north.
    pub heading: Option<f64>,
    /// An equirectangular 360° panorama (else a flat photo).
    pub pano: bool,
    /// "2024-09-15".
    pub taken: String,
    pub thumb: String,
    pub full: String,
    pub alt: Vec<String>,
    pub creator: String,
    pub license: String,
    pub page: String,
    pub prev: Option<Step>,
    pub next: Option<Step>,
}

impl Shot {
    pub fn full_key(&self) -> String {
        format!("sv:f:{}:{}", self.provider.label(), self.id)
    }

    pub fn thumb_key(&self) -> String {
        format!("sv:t:{}:{}", self.provider.label(), self.id)
    }

    pub fn full_origin(&self) -> pixels::Origin {
        pixels::Origin::Web(std::iter::once(self.full.clone()).chain(self.alt.iter().cloned()).collect())
    }

    pub fn thumb_origin(&self) -> pixels::Origin {
        pixels::Origin::Web(vec![self.thumb.clone()])
    }

    /// "Cyril5C · CC-BY-SA-4.0 · Panoramax · 2024-09-15".
    pub fn credit(&self) -> String {
        [self.creator.as_str(), self.license.as_str(), self.provider.label(), self.taken.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ")
    }

    pub fn want_pixels(&self) {
        pixels::want(&self.thumb_key(), &self.thumb_origin(), 640, false);
        pixels::want(&self.full_key(), &self.full_origin(), if self.pano { PANO_SIDE } else { crate::image_viewer::FULL_SIDE }, false);
    }
}

/// What the street-view block shows.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    /// "the Eiffel Tower", "here".
    pub label: String,
    pub target: Option<(f64, f64)>,
    pub shot: Option<Shot>,
    /// Where to look first, degrees from the picture's middle (at the landmark, for a panorama).
    pub start_yaw: f32,
    pub version: u64,
    pub note: String,
    pub busy: bool,
}

static SCENE: LazyLock<Mutex<Scene>> = LazyLock::new(Default::default);
static OPEN: AtomicBool = AtomicBool::new(false);
static SCENE_VERSION: AtomicU64 = AtomicU64::new(1);
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
/// The Mapillary token of the last request (memory only, for stepping from the GUI).
static TOKEN: Mutex<Option<String>> = Mutex::new(None);

pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

pub fn close() {
    OPEN.store(false, Ordering::Release);
    crate::gui::refresh();
}

pub fn scene() -> Scene {
    lock(&SCENE).clone()
}

fn update(f: impl FnOnce(&mut Scene)) {
    let mut s = lock(&SCENE);
    f(&mut s);
    s.version = SCENE_VERSION.fetch_add(1, Ordering::AcqRel);
    drop(s);
    crate::gui::refresh();
}

fn open_with(f: impl FnOnce(&mut Scene)) {
    update(f);
    OPEN.store(true, Ordering::Release);
    crate::hud_dock::cancel_close("street_view");
    crate::gui::show();
}

pub fn set_runtime(rt: &tokio::runtime::Handle) {
    let _ = RUNTIME.set(rt.clone());
}

/// A Mapillary client token as Sir pasted it ("MLY|…"), or None when it is not one.
pub fn clean_token(raw: &str) -> Option<String> {
    let t = raw.trim().trim_matches('"').trim();
    (t.starts_with("MLY|") && t.len() >= 20 && t.len() <= 300 && !t.chars().any(char::is_whitespace)).then(|| t.to_string())
}

// ------------------------------------------------------------------------------- geometry ----

/// Degrees from north, from `a` towards `b`.
pub fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let dl = (b.1 - a.1).to_radians();
    let y = dl.sin() * la2.cos();
    let x = la1.cos() * la2.sin() - la1.sin() * la2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
    crate::earth::haversine_km(a, b)
}

/// The box of `km` around a point: west, south, east, north.
fn bbox(at: (f64, f64), radius_km: f64) -> [f64; 4] {
    let dlat = radius_km / 111.0;
    let dlon = radius_km / (111.0 * at.0.to_radians().cos().abs().max(0.05));
    [at.1 - dlon, at.0 - dlat, at.1 + dlon, at.0 + dlat]
}

// ------------------------------------------------------------------------------- sources ----

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x: &f64| x.is_finite())
}

/// One Panoramax picture (a STAC item).
pub fn parse_panoramax(f: &Value) -> Option<Shot> {
    let coords = f["geometry"]["coordinates"].as_array()?;
    let (lon, lat) = (num(coords.first()?)?, num(coords.get(1)?)?);
    let p = &f["properties"];
    let assets = &f["assets"];
    let sd = assets["sd"]["href"].as_str();
    let hd = assets["hd"]["href"].as_str();
    let thumb = assets["thumb"]["href"].as_str().or(sd)?.to_string();
    let full = sd.or(hd).unwrap_or(&thumb).to_string();
    let link = |rel: &str| f["links"].as_array().and_then(|links| links.iter().find(|l| l["rel"] == rel)).and_then(|l| l["href"].as_str()).map(|h| Step::Panoramax(h.to_string()));
    let license = p["license"].as_str().map(str::to_string).or_else(|| {
        let title = f["links"].as_array()?.iter().find(|l| l["rel"] == "license")?["title"].as_str()?;
        Some(title.rsplit_once('(').map_or(title, |(_, rest)| rest.trim_end_matches(')')).to_string())
    });
    let id = f["id"].as_str()?.to_string();
    Some(Shot {
        provider: Provider::Panoramax,
        page: format!("https://api.panoramax.xyz/#focus=pic&pic={id}"),
        id,
        lat,
        lon,
        heading: num(&p["view:azimuth"]),
        pano: num(&p["pers:interior_orientation"]["field_of_view"]).is_some_and(|fov| fov >= 359.0),
        taken: p["datetime"].as_str().map(|d| d.chars().take(10).collect()).unwrap_or_default(),
        thumb,
        full,
        alt: hd.filter(|h| Some(*h) != sd).map(|h| vec![h.to_string()]).unwrap_or_default(),
        creator: f["providers"].as_array().and_then(|p| p.first()).and_then(|p| p["name"].as_str()).or(p["geovisio:producer"].as_str()).unwrap_or("").to_string(),
        license: license.unwrap_or_else(|| "CC BY-SA 4.0".into()),
        prev: link("prev"),
        next: link("next"),
    })
}

/// One KartaView photo.
pub fn parse_kartaview(d: &Value) -> Option<Shot> {
    let (lat, lon) = (num(&d["lat"])?, num(&d["lng"])?);
    let id = d["id"].as_str().map(str::to_string).or_else(|| d["id"].as_i64().map(|i| i.to_string()))?;
    let sequence = d["sequenceId"].as_str().map(str::to_string).or_else(|| d["sequenceId"].as_i64().map(|i| i.to_string())).unwrap_or_default();
    let index = num(&d["sequenceIndex"]).map(|i| i as i64);
    let pick = |keys: &[&str]| keys.iter().filter_map(|k| d[*k].as_str()).filter(|u| u.starts_with("http") && !u.contains("{{")).map(str::to_string).collect::<Vec<_>>();
    let fulls = pick(&["imageLthUrl", "imageProcUrl", "fileurlLTh", "fileurlProc"]);
    let thumb = pick(&["imageThUrl", "fileurlTh"]).into_iter().next().or_else(|| fulls.first().cloned())?;
    let step = |i: i64| (!sequence.is_empty() && i >= 0).then(|| Step::KartaView { sequence: sequence.clone(), index: i });
    Some(Shot {
        provider: Provider::KartaView,
        page: match index {
            Some(i) if !sequence.is_empty() => format!("https://kartaview.org/details/{sequence}/{i}"),
            _ => "https://kartaview.org/".into(),
        },
        id,
        lat,
        lon,
        heading: num(&d["heading"]),
        pano: d["projection"].as_str().is_some_and(|p| p.eq_ignore_ascii_case("SPHERE")),
        taken: d["shotDate"].as_str().map(|s| s.chars().take(10).collect()).unwrap_or_default(),
        full: fulls.first().cloned().unwrap_or_else(|| thumb.clone()),
        alt: fulls.iter().skip(1).cloned().collect(),
        thumb,
        creator: d["username"].as_str().filter(|u| !u.is_empty()).unwrap_or("KartaView contributors").to_string(),
        license: "CC BY-SA 4.0".into(),
        prev: index.and_then(|i| step(i - 1)),
        next: index.and_then(|i| step(i + 1)),
    })
}

/// One Mapillary image (Graph API v4).
pub fn parse_mapillary(d: &Value) -> Option<Shot> {
    let geometry = if d["computed_geometry"]["coordinates"].is_array() { &d["computed_geometry"] } else { &d["geometry"] };
    let coords = geometry["coordinates"].as_array()?;
    let (lon, lat) = (num(coords.first()?)?, num(coords.get(1)?)?);
    let id = d["id"].as_str().map(str::to_string).or_else(|| d["id"].as_i64().map(|i| i.to_string()))?;
    let full = d["thumb_2048_url"].as_str()?.to_string();
    let sequence = d["sequence"].as_str().map(str::to_string).or_else(|| d["sequence"]["id"].as_str().map(str::to_string)).unwrap_or_default();
    let taken = d["captured_at"].as_i64().and_then(|ms| chrono::DateTime::from_timestamp_millis(ms)).map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default();
    let step = (!sequence.is_empty()).then(|| Step::Mapillary { sequence: sequence.clone(), id: id.clone() });
    Some(Shot {
        provider: Provider::Mapillary,
        page: format!("https://www.mapillary.com/app/?pKey={id}"),
        lat,
        lon,
        heading: num(&d["compass_angle"]),
        pano: d["is_pano"].as_bool() == Some(true),
        taken,
        thumb: d["thumb_256_url"].as_str().unwrap_or(&full).to_string(),
        full,
        alt: Vec::new(),
        creator: d["creator"]["username"].as_str().unwrap_or("Mapillary contributors").to_string(),
        license: "CC BY-SA 4.0".into(),
        prev: step.clone(),
        next: step,
        id,
    })
}

async fn panoramax(url: String) -> Vec<Shot> {
    match crate::image_viewer::get_json(&url, TIMEOUT).await {
        Ok(v) => v["features"].as_array().into_iter().flatten().filter_map(parse_panoramax).collect(),
        Err(e) => {
            debug!("[STREET] Panoramax: {e}");
            Vec::new()
        }
    }
}

async fn kartaview(url: String) -> Vec<Shot> {
    match crate::image_viewer::get_json(&url, TIMEOUT).await {
        Ok(v) => v["result"]["data"].as_array().into_iter().flatten().filter_map(parse_kartaview).collect(),
        Err(e) => {
            debug!("[STREET] KartaView: {e}");
            Vec::new()
        }
    }
}

/// A Graph API call with Sir's token in the header (never in the address, never in a log).
async fn mapillary_get(url: &str, token: &str) -> Result<Value, String> {
    let resp = crate::image_viewer::http().get(url).timeout(TIMEOUT).header("Authorization", format!("OAuth {token}")).send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Mapillary answered HTTP {}", resp.status().as_u16()));
    }
    resp.json::<Value>().await.map_err(|e| e.without_url().to_string())
}

async fn mapillary(at: (f64, f64), radius_km: f64, token: &str) -> Vec<Shot> {
    let [w, s, e, n] = bbox(at, radius_km);
    let url = format!("{MAPILLARY}/images?fields={MAPILLARY_FIELDS}&bbox={w:.6},{s:.6},{e:.6},{n:.6}&limit=50");
    match mapillary_get(&url, token).await {
        Ok(v) => v["data"].as_array().into_iter().flatten().filter_map(parse_mapillary).collect(),
        Err(e) => {
            debug!("[STREET] Mapillary: {e}");
            Vec::new()
        }
    }
}

/// The best of what was found: the nearest, a panorama when one is about as near.
fn best(shots: Vec<Shot>, at: (f64, f64)) -> Option<(Shot, f64)> {
    let mut scored: Vec<(f64, Shot)> = shots.into_iter().map(|s| (km(at, (s.lat, s.lon)), s)).collect();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    let nearest = scored.first()?.0;
    let pick = scored.iter().position(|(d, s)| s.pano && *d <= nearest + 0.03).unwrap_or(0);
    let (d, shot) = scored.swap_remove(pick);
    Some((shot, d))
}

/// What a search found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Found(Shot, f64),
    /// Nothing within a kilometre; the nearest photo is this far (km) away.
    Nearest(f64),
    Nothing,
}

async fn search(at: (f64, f64), landmark: bool, token: Option<&str>) -> Outcome {
    // 1. Mapillary, with Sir's own token.
    if let Some(token) = token {
        for radius in [0.15, NEARBY_KM] {
            if let Some((shot, d)) = best(mapillary(at, radius, token).await, at) {
                return Outcome::Found(shot, d);
            }
        }
    }
    // 2. Panoramax: pictures looking at a landmark, then pictures near the point.
    if landmark {
        let url = format!("{PANORAMAX}/search?place_position={:.6},{:.6}&place_distance=5-250&limit=12", at.1, at.0);
        if let Some((shot, d)) = best(panoramax(url).await, at) {
            return Outcome::Found(shot, d);
        }
    }
    // 3. Panoramax and KartaView around the point, at once.
    for radius in [0.15, NEARBY_KM] {
        let [w, s, e, n] = bbox(at, radius);
        let pnx = format!("{PANORAMAX}/search?bbox={w:.6},{s:.6},{e:.6},{n:.6}&limit=40");
        let kv = format!("{KARTAVIEW}?lat={:.6}&lng={:.6}&radius={}&itemsPerPage=40", at.0, at.1, (radius * 1000.0) as u32);
        let (a, b) = tokio::join!(panoramax(pnx), kartaview(kv));
        let mut shots = a;
        shots.extend(b);
        if let Some((shot, d)) = best(shots, at) {
            return Outcome::Found(shot, d);
        }
    }
    // 4. Nothing nearby: how far the nearest one is.
    let [w, s, e, n] = bbox(at, SEARCH_FAR_KM);
    let pnx = format!("{PANORAMAX}/search?bbox={w:.6},{s:.6},{e:.6},{n:.6}&limit=100");
    let kv = format!("{KARTAVIEW}?lat={:.6}&lng={:.6}&radius=5000&itemsPerPage=40", at.0, at.1);
    let (a, b) = tokio::join!(panoramax(pnx), kartaview(kv));
    let far = a.into_iter().chain(b).map(|s| km(at, (s.lat, s.lon))).fold(f64::INFINITY, f64::min);
    if far.is_finite() {
        Outcome::Nearest(far)
    } else {
        Outcome::Nothing
    }
}

/// The stand-in sequence: five panoramas along a street.
fn demo_shot(at: (f64, f64), n: i64) -> Shot {
    Shot {
        provider: Provider::Demo,
        id: format!("demo{n}"),
        lat: at.0 + n as f64 * 0.0001,
        lon: at.1,
        heading: Some(90.0),
        pano: true,
        taken: "2026-01-01".into(),
        thumb: format!("demo://pano/{n}/thumb"),
        full: format!("demo://pano/{n}"),
        alt: Vec::new(),
        creator: "JAI self-test".into(),
        license: "CC0".into(),
        page: String::new(),
        prev: (n > 0).then_some(Step::Demo(n - 1)),
        next: (n < 4).then_some(Step::Demo(n + 1)),
    }
}

fn demo_search(at: (f64, f64)) -> Outcome {
    // (The demo's ocean: nothing at street level for 30 km.)
    if at.0.abs() < 5.0 && at.1 < -120.0 {
        return Outcome::Nothing;
    }
    Outcome::Found(demo_shot((at.0 + 0.0003, at.1), 0), km(at, (at.0 + 0.0003, at.1)))
}

/// The Panoramax or KartaView photo one step along the sequence.
async fn fetch_step(step: &Step) -> Option<Shot> {
    match step {
        Step::Panoramax(href) => crate::image_viewer::get_json(href, TIMEOUT).await.ok().and_then(|v| parse_panoramax(&v)),
        Step::KartaView { sequence, index } => {
            let url = format!("{KARTAVIEW}?sequenceId={sequence}&sequenceIndex={index}&itemsPerPage=1");
            crate::image_viewer::get_json(&url, TIMEOUT).await.ok().and_then(|v| v["result"]["data"].as_array().and_then(|d| d.first()).and_then(parse_kartaview))
        }
        Step::Mapillary { .. } | Step::Demo(_) => None,
    }
}

static MAPILLARY_SEQUENCES: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(Default::default);

/// Mapillary: the image before or after `id` in its sequence.
async fn mapillary_step(sequence: &str, id: &str, forward: bool, token: &str) -> Option<Shot> {
    let cached = lock(&MAPILLARY_SEQUENCES).get(sequence).cloned();
    let ids = match cached {
        Some(ids) => ids,
        None => {
            let v = mapillary_get(&format!("{MAPILLARY}/image_ids?sequence_id={}", urlencoding::encode(sequence)), token).await.ok()?;
            let ids: Vec<String> = v["data"].as_array()?.iter().filter_map(|d| d["id"].as_str().map(str::to_string)).collect();
            lock(&MAPILLARY_SEQUENCES).insert(sequence.to_string(), ids.clone());
            ids
        }
    };
    let at = ids.iter().position(|i| i == id)?;
    let next = if forward { ids.get(at + 1)? } else { ids.get(at.checked_sub(1)?)? };
    let v = mapillary_get(&format!("{MAPILLARY}/{next}?fields={MAPILLARY_FIELDS}"), token).await.ok()?;
    parse_mapillary(&v)
}

/// Forward or back along the sequence ("step forward", the arrows). What happened, in words.
pub async fn step(forward: bool) -> Result<String, String> {
    let Some(shot) = scene().shot.filter(|_| is_open()) else {
        return Ok("Street view is not open.\nSpoken: Street view isn't up, Sir.".into());
    };
    let way = if forward { "forward" } else { "back" };
    let token = lock(&TOKEN).clone();
    let next = match (if forward { shot.next.clone() } else { shot.prev.clone() }, offline()) {
        (None, _) => None,
        (Some(Step::Demo(n)), _) => {
            let at = shot.id.trim_start_matches("demo").parse::<i64>().unwrap_or(0) as f64 * 0.0001;
            Some(demo_shot((shot.lat - at, shot.lon), n))
        }
        (Some(_), true) => None,
        (Some(Step::Mapillary { sequence, id }), false) => match token.as_deref() {
            Some(t) => mapillary_step(&sequence, &id, forward, t).await,
            None => None,
        },
        (Some(step), false) => fetch_step(&step).await,
    };
    let Some(next) = next else {
        update(|s| s.note = format!("That's the end of this sequence ({way})."));
        return Ok(format!("No photo further {way} in this sequence.\nSpoken: That's as far as this sequence goes, Sir."));
    };
    next.want_pixels();
    let moved = km((shot.lat, shot.lon), (next.lat, next.lon)) * 1000.0;
    update(|s| {
        s.shot = Some(next.clone());
        s.note.clear();
    });
    Ok(format!("One step {way}: {:.0} m, {} ({}).\nSpoken: {}.", moved, next.taken, next.credit(), if forward { "Forward" } else { "Back" }))
}

/// The GUI's arrows and buttons.
pub fn step_in_background(forward: bool) {
    let task = async move {
        if let Err(e) = step(forward).await {
            debug!("[STREET] step: {e}");
        }
    };
    match tokio::runtime::Handle::try_current().ok().or_else(|| RUNTIME.get().cloned()) {
        Some(rt) => {
            rt.spawn(task);
        }
        None => debug!("[STREET] no runtime to step on"),
    }
}

// ---------------------------------------------------------------------------- the tool ----

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "Street-level 360° photos of a place, or here; forward/back.",
        "parameters": {"type": "object", "properties": {
            "place": {"type": "string"},
            "here": {"type": "boolean"},
            "action": {"type": "string", "enum": ["show", "forward", "back"]}
        }}
    }})]
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        pixels::set_runtime(&rt);
        set_runtime(&rt);
    }
    let action = str_arg(args, &["action"]).unwrap_or("show").trim().to_lowercase();
    match action.as_str() {
        "forward" | "next" => step(true).await,
        "back" | "previous" | "backward" => step(false).await,
        "close" | "hide" => {
            if is_open() && crate::hud_dock::close_named("street view").is_empty() {
                close();
            }
            Ok("Street view is closed.\nSpoken: Street view closed, Sir.".into())
        }
        _ => show(tools, args).await,
    }
}

async fn show(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let place = str_arg(args, &["place", "location", "query", "address"]).map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
    let here = bool_arg(args, "here") == Some(true) || place.as_deref().is_none_or(|p| matches!(p.to_lowercase().as_str(), "here" | "hier" | "this place" | "there"));
    let (at, label, landmark) = if here {
        if crate::earth::is_open() {
            let cam = crate::earth::state().cam;
            let label = if offline() { None } else { tools.stark.location.reverse_geocode(cam.lat, cam.lon).await };
            ((cam.lat, cam.lon), label.unwrap_or_else(|| "the middle of the Earth view".into()), false)
        } else if offline() {
            // (Dry runs stand in Stuttgart, the demo's own city.)
            ((48.7823, 9.177), "where you are".into(), false)
        } else {
            let fix = tools.stark.location.current(tools).await?;
            ((fix.lat, fix.lon), fix.address.unwrap_or_else(|| "where you are".into()), false)
        }
    } else {
        let place = place.unwrap_or_default();
        let (lat, lon, name) = locate(tools, &place).await?;
        // ("The Eiffel Tower" is a landmark to look at; "Berlin" is a city to stand in.)
        let landmark = crate::earth::gazetteer(&place).is_none();
        ((lat, lon), if name.is_empty() { place } else { name }, landmark)
    };
    let token = if offline() { None } else { tools.pim.config().vault().secret(VAULT_SERVICE, crate::vault::TOKEN).filter(|t| !t.trim().is_empty()) };
    *lock(&TOKEN) = token.clone();
    open_with(|s| *s = Scene { label: label.clone(), target: Some(at), shot: None, start_yaw: 0.0, version: 0, note: format!("Looking for street-level photos of {label}…"), busy: true });
    let started = std::time::Instant::now();
    let outcome = if offline() { demo_search(at) } else { search(at, landmark, token.as_deref()).await };
    info!("[STREET] '{label}': {} in {} ms", match &outcome { Outcome::Found(s, d) => format!("{} {} at {:.0} m", s.provider.label(), if s.pano { "panorama" } else { "photo" }, d * 1000.0), Outcome::Nearest(d) => format!("nearest {d:.1} km"), Outcome::Nothing => "nothing".into() }, started.elapsed().as_millis());
    match outcome {
        Outcome::Found(shot, d) => {
            shot.want_pixels();
            // A panorama starts looking at the landmark.
            let start_yaw = match (shot.pano, shot.heading, landmark) {
                (true, Some(heading), true) if d > 0.005 => (bearing((shot.lat, shot.lon), at) - heading + 540.0).rem_euclid(360.0) as f32 - 180.0,
                _ => 0.0,
            };
            let kind = if shot.pano { "a 360° panorama" } else { "a street-level photo" };
            let year = shot.taken.get(..4).unwrap_or("").to_string();
            let display = format!(
                "Street view of {label}: {kind} {:.0} m away ({}). Drag to look around, the wheel zooms, ← → step along the sequence.",
                d * 1000.0,
                shot.credit()
            );
            update(|s| {
                s.shot = Some(shot.clone());
                s.start_yaw = start_yaw;
                s.busy = false;
                s.note.clear();
            });
            crate::hud_dock::describe_block("street_view", &format!("STREET VIEW // {label}: {kind}, {}", shot.credit()));
            let when = if year.is_empty() { String::new() } else { format!(" from {year}") };
            let spoken = if shot.pano { format!("Street level at {label}, Sir — a full panorama{when}. Drag to look around.") } else { format!("Street level at {label}, Sir — a photo{when}.") };
            Ok(format!("{display}\nSpoken: {spoken}"))
        }
        Outcome::Nearest(far) => {
            update(|s| {
                s.busy = false;
                s.note = format!("No street-level photos within a kilometre of {label}. The nearest is {far:.1} km away.");
            });
            Ok(format!(
                "Nothing at street level within a kilometre of {label} (Panoramax, KartaView{}). The nearest photo is {far:.1} km away.\nSpoken: There are no street-level photos within a kilometre of {label}, Sir. The nearest one is {} away.",
                if token.is_some() { ", Mapillary" } else { "" },
                crate::location_engine::spoken_distance(far * 1000.0)
            ))
        }
        Outcome::Nothing => {
            update(|s| {
                s.busy = false;
                s.note = format!("No street-level photos within {SEARCH_FAR_KM:.0} km of {label}.");
            });
            Ok(format!("No street-level photos within {SEARCH_FAR_KM:.0} km of {label}.\nSpoken: There are no street-level photos within thirty kilometres of {label}, Sir."))
        }
    }
}

/// A place by name: the cameras' geocoding (cities), with OpenStreetMap first for landmarks.
async fn locate(tools: &ToolContext, place: &str) -> Result<(f64, f64, String), String> {
    if offline() {
        return crate::webcams::demo_place(place).ok_or_else(|| format!("'{place}' is not among the demo's places."));
    }
    if crate::earth::gazetteer(place).is_none() {
        if let Ok((lat, lon, name)) = tools.stark.location.geocode(place, None).await {
            let short = name.split(',').next().unwrap_or(&name).trim().to_string();
            return Ok((lat, lon, short));
        }
    }
    crate::webcams::locate(tools, place).await
}

// ------------------------------------------------------------------------- Sir's words ----

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid street view regex")
}

const SV: &str = r"(?:street ?view|street[- ]level(?: view| photos?| imagery| images?)?|straßenansicht|strassenansicht)";

static HERE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:(?:show|open|give|go to|switch to|zeig|zeige|öffne|oeffne)(?: me| mir)? )?(?:the |die )?{SV}(?: (?:here|of here|from here|right here|at this spot|of this place|of the centre|of the center|hier|von hier))?$")));
static OF: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:(?:show|open|display|give|take|bring up|pull up|put up|go to|find|get)(?: me| us)? )?(?:a |the )?{SV}(?: photos?| images?| imagery)? (?:of|at|in|for|from|near|around|on|outside) (?P<place>.+)$|^(?:(?:zeig|zeige|öffne|oeffne|hol)(?: mir| uns)? )?(?:die |eine )?{SV} (?:von|vom|der|des|in|im|am|an der|bei|zu|zur|zum) (?P<place2>.+)$"
    ))
});
static STEP: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:step|go|walk|move|drive) (?P<dir>forward|forwards|ahead|on|back|backward|backwards)(?: in street view| a step| one step)?$|^(?:geh|gehe|fahr|fahre|lauf|laufe) (?P<dir2>weiter|vorwärts|vor|zurück)$"));

/// `show_street_view` arguments for one of Sir's sentences (already normalised).
pub fn phrase(s: &str) -> Option<Value> {
    if HERE.is_match(s) {
        return Some(json!({"here": true}));
    }
    if let Some(c) = OF.captures(s) {
        let place = c.name("place").or_else(|| c.name("place2"))?.as_str().trim();
        let place = place.strip_suffix(" please").unwrap_or(place).trim();
        return Some(if matches!(place, "here" | "hier" | "this place" | "me") { json!({"here": true}) } else { json!({"place": place}) });
    }
    if is_open() {
        if let Some(c) = STEP.captures(s) {
            let dir = c.name("dir").or_else(|| c.name("dir2"))?.as_str();
            let back = matches!(dir, "back" | "backward" | "backwards" | "zurück");
            return Some(json!({"action": if back { "back" } else { "forward" }}));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intents::normalize_phrase;

    fn said(text: &str) -> Option<Value> {
        phrase(&normalize_phrase(text))
    }

    #[test]
    fn sirs_street_view_requests_route() {
        assert_eq!(said("Show me street view of the Eiffel Tower"), Some(json!({"place": "the eiffel tower"})));
        assert_eq!(said("street view here"), Some(json!({"here": true})));
        assert_eq!(said("JAI, open street view."), Some(json!({"here": true})));
        assert_eq!(said("Zeig mir die Straßenansicht vom Brandenburger Tor"), Some(json!({"place": "brandenburger tor"})));
        assert_eq!(said("show me street level photos of Times Square"), Some(json!({"place": "times square"})));
        assert_eq!(said("show me the street"), None);
    }

    #[test]
    fn source_formats_parse() {
        let pnx = json!({"id": "feb5593c", "geometry": {"type": "Point", "coordinates": [2.295155, 48.857780528]},
            "links": [{"rel": "license", "title": "License for this object (CC-BY-SA-4.0)"}, {"rel": "prev", "href": "https://api.panoramax.xyz/api/collections/c/items/p"}, {"rel": "next", "href": "https://api.panoramax.xyz/api/collections/c/items/n"}],
            "providers": [{"name": "Cyril5C"}],
            "properties": {"datetime": "2024-09-15T17:04:07+00:00", "view:azimuth": 53, "pers:interior_orientation": {"field_of_view": 360}},
            "assets": {"hd": {"href": "https://x/hd.jpg"}, "sd": {"href": "https://x/sd.jpg"}, "thumb": {"href": "https://x/thumb.jpg"}}});
        let s = parse_panoramax(&pnx).unwrap();
        assert!(s.pano);
        assert_eq!((s.full.as_str(), s.thumb.as_str(), s.license.as_str(), s.creator.as_str(), s.taken.as_str()), ("https://x/sd.jpg", "https://x/thumb.jpg", "CC-BY-SA-4.0", "Cyril5C", "2024-09-15"));
        assert_eq!(s.next, Some(Step::Panoramax("https://api.panoramax.xyz/api/collections/c/items/n".into())));
        assert_eq!(s.heading, Some(53.0));

        let kv = json!({"id": "5062217", "lat": "52.516396", "lng": "13.3794", "heading": "196.16", "projection": "PLANE", "sequenceId": "10728", "sequenceIndex": "0", "shotDate": "2016-07-17 20:27:12.000",
            "imageLthUrl": "https://cdn.kartaview.org/pr:sharp/lth", "imageProcUrl": "https://cdn.kartaview.org/pr:sharp/proc", "imageThUrl": "https://cdn.kartaview.org/pr:sharp/th", "fileurl": "https://storage1.openstreetcam.org/files/photo/{{sizeprefix}}/x.jpg"});
        let s = parse_kartaview(&kv).unwrap();
        assert!(!s.pano);
        assert_eq!((s.full.as_str(), s.alt.len(), s.prev.clone(), s.taken.as_str()), ("https://cdn.kartaview.org/pr:sharp/lth", 1, None, "2016-07-17"));
        assert_eq!(s.next, Some(Step::KartaView { sequence: "10728".into(), index: 1 }));

        let mly = json!({"id": "123", "computed_geometry": {"type": "Point", "coordinates": [2.29, 48.85]}, "thumb_2048_url": "https://scontent/x", "is_pano": true, "captured_at": 1726419847000i64, "sequence": "seqA", "compass_angle": 12.5, "creator": {"username": "someone"}});
        let s = parse_mapillary(&mly).unwrap();
        assert!(s.pano);
        assert_eq!((s.taken.as_str(), s.creator.as_str()), ("2024-09-15", "someone"));
    }

    #[test]
    fn geometry_and_tokens() {
        let paris = (48.8584, 2.2945);
        assert!((bearing((48.0, 2.2945), paris) - 0.0).abs() < 0.01, "due north");
        assert!((bearing(paris, (48.8584, 3.0)) - 90.0).abs() < 0.5, "due east");
        let [w, s, e, n] = bbox(paris, 1.0);
        assert!(w < paris.1 && e > paris.1 && s < paris.0 && n > paris.0);
        assert_eq!(clean_token("  MLY|1234567890|abcdef0123456789  ").as_deref(), Some("MLY|1234567890|abcdef0123456789"));
        assert_eq!(clean_token("hello"), None);
        assert_eq!(clean_token("MLY|12 34|with spaces inside"), None);
    }

    #[test]
    fn the_viewer_runs_offline_on_a_stand_in_sequence() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert_eq!(demo_search((0.0, -150.0)), Outcome::Nothing);
        let Outcome::Found(shot, d) = demo_search((48.8584, 2.2945)) else { panic!("a demo shot") };
        assert!(shot.pano && d < 0.1);
        shot.want_pixels();
        assert!(pixels::get(&shot.full_key()).is_some());
        open_with(|s| *s = Scene { label: "test".into(), target: Some((48.8584, 2.2945)), shot: Some(shot), start_yaw: 0.0, version: 0, note: String::new(), busy: false });
        assert_eq!(said("step forward"), Some(json!({"action": "forward"})));
        let out = rt.block_on(step(true)).unwrap();
        assert!(out.starts_with("One step forward"), "{out}");
        assert_eq!(scene().shot.unwrap().id, "demo1");
        let out = rt.block_on(step(false)).unwrap();
        assert!(out.starts_with("One step back"), "{out}");
        let out = rt.block_on(step(false)).unwrap();
        assert!(out.contains("as far as this sequence goes"), "{out}");
        close();
    }
}
