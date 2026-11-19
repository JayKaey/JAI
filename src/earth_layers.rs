//! The Earth's live layers (C1): every satellite (CelesTrak's element sets propagated with our
//! own SGP4/SDP4, `sat_orbit`), the trains between their stations (Transitous, Finland's
//! Digitraffic) and the road incidents (Autobahn GmbH, TfL).
//!
//! `earth.rs` and `gui_earth.rs` only call in through small hooks (each marked
//! `// live layers (C1)`): the actions (`parse_action`, `act`), the resets, the schedule, and the
//! painting (`paint` gets the projection as closures and returns labels, glyph rects and the
//! thing under the pointer). The state lives here, behind its own lock; it is always taken
//! AFTER the Earth's (never call `earth::state()` while holding it).
//!
//! What is fetched and how often: a CelesTrak group at most once in six hours (and kept on disk,
//! CelesTrak asks for no more than one download in two hours), the trains in view at most every
//! 90 s and only while zoomed in, Finland's trains every 20 s while it is in view, one motorway
//! every 2 s while Germany is in view and the traffic layer is on (each refreshed after ten
//! minutes), London's disruptions every five minutes while London is in view.

use crate::crash_guard::lock;
use crate::sat_orbit::{self, Elements, Observer, Sgp4};
use crate::transit::{FiTrain, Segment};
use eframe::egui::{vec2, Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tracing::{debug, info};

const CELESTRAK: &str = "https://celestrak.org/NORAD/elements/gp.php";
const CACHE_DIR: &str = "config/live_cache";
/// A group's element sets count as fresh this long.
const SAT_FRESH: Duration = Duration::from_secs(6 * 3600);
/// After a failed download, the next try waits this long.
const SAT_RETRY: Duration = Duration::from_secs(3600);
/// How often the positions of all satellites are recomputed (in between they are extrapolated).
const SNAPSHOT_EVERY: f64 = 3.0;
/// Trains are drawn and fetched below this camera altitude.
const TRAINS_MAX_ALT_KM: f64 = 2_000.0;
const TRAINS_EVERY: Duration = Duration::from_secs(90);
const TRAINS_MIN_GAP: Duration = Duration::from_secs(20);
const FINLAND_EVERY: Duration = Duration::from_secs(20);
/// Road incidents are fetched below this altitude while Germany or London is in view.
const TRAFFIC_MAX_ALT_KM: f64 = 2_500.0;
const ROAD_REFRESH: Duration = Duration::from_secs(600);
const ROAD_GAP: Duration = Duration::from_secs(2);

/// The demo's satellites: real public element sets (CelesTrak, 8 Nov 2026).
const DEMO_SATELLITES: &str = include_str!("../assets/live_layers/demo_satellites.json");

// ----------------------------------------------------------------------------- groups ----

/// (key, CelesTrak group, how JAI says it).
pub const GROUPS: &[(&str, &str, &str)] = &[
    ("active", "active", "active satellites"),
    ("starlink", "starlink", "Starlink satellites"),
    ("oneweb", "oneweb", "OneWeb satellites"),
    ("gps", "gps-ops", "GPS satellites"),
    ("galileo", "galileo", "Galileo satellites"),
    ("glonass", "glo-ops", "GLONASS satellites"),
    ("beidou", "beidou", "BeiDou satellites"),
    ("iridium", "iridium-NEXT", "Iridium satellites"),
    ("weather", "weather", "weather satellites"),
    ("geo", "geo", "geostationary satellites"),
    ("stations", "stations", "space stations and their visitors"),
    ("visual", "visual", "brightest satellites"),
    ("amateur", "amateur", "amateur radio satellites"),
    ("science", "science", "science satellites"),
];

fn group(key: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    GROUPS.iter().find(|(k, g, _)| *k == key || g.eq_ignore_ascii_case(key))
}

/// The group a sentence names ("starlink", "the GPS satellites", "alle Satelliten" → active).
pub fn group_in(s: &str) -> Option<&'static str> {
    let s = &s.to_lowercase().replace('-', " ");
    let padded = format!(" {s} ");
    let has = |w: &[&str]| w.iter().any(|x| s.contains(x));
    let word = |w: &[&str]| w.iter().any(|x| padded.contains(&format!(" {x} ")));
    Some(if has(&["starlink"]) {
        "starlink"
    } else if has(&["oneweb", "one web"]) {
        "oneweb"
    } else if word(&["gps", "navstar"]) {
        "gps"
    } else if word(&["galileo"]) {
        "galileo"
    } else if word(&["glonass"]) {
        "glonass"
    } else if word(&["beidou"]) {
        "beidou"
    } else if word(&["iridium"]) {
        "iridium"
    } else if has(&["weather satellite", "wettersatellit", "meteorological satellite"]) {
        "weather"
    } else if has(&["geostationary", "geostationär", "geostationaer", "geosynchronous"]) {
        "geo"
    } else if has(&["space stations", "raumstationen"]) {
        "stations"
    } else if has(&["amateur", "ham radio", "amateurfunk"]) {
        "amateur"
    } else if has(&["science satellite", "scientific satellite", "wissenschaftssatellit", "forschungssatellit"]) {
        "science"
    } else if has(&["brightest", "visible satellites", "hellsten", "sichtbaren satelliten", "naked eye"]) {
        "visual"
    } else if has(&["satellites", "satelliten"]) {
        "active"
    } else {
        return None;
    })
}

// --------------------------------------------------------------------------- the model ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SatKind {
    Station,
    Starlink,
    Navigation,
    Weather,
    Geo,
    Other,
}

/// The ISS, Tiangong and Hubble: the only satellites labelled on the globe.
const LANDMARKS: [u32; 3] = [25544, 48274, 20580];

pub fn kind_of(el: &Elements, group: &str) -> SatKind {
    let n = el.name.to_uppercase();
    // The stations' own modules ("CSS (WENTIAN)"), not what was set loose from them ("ISS OBJECT YQ").
    if LANDMARKS.contains(&el.norad) || n.starts_with("ISS (") || n.starts_with("CSS (") {
        SatKind::Station
    } else if n.starts_with("STARLINK") {
        SatKind::Starlink
    } else if n.starts_with("GPS") || n.starts_with("NAVSTAR") || n.starts_with("GSAT0") || n.starts_with("GALILEO") || n.starts_with("BEIDOU") || n.contains("GLONASS") || matches!(group, "gps" | "galileo" | "glonass" | "beidou") {
        SatKind::Navigation
    } else if group == "weather" || ["NOAA", "GOES", "METEOSAT", "METOP", "HIMAWARI", "FENGYUN", "ARKTIKA", "ELEKTRO", "JPSS", "DMSP", "METEOR"].iter().any(|p| n.starts_with(p)) {
        SatKind::Weather
    } else if el.mean_motion > 0.9 && el.mean_motion < 1.1 && el.eccentricity < 0.05 {
        SatKind::Geo
    } else {
        SatKind::Other
    }
}

fn kind_colour(kind: SatKind, accent: Color32) -> Color32 {
    match kind {
        SatKind::Station => crate::holo::theme().hot(),
        SatKind::Starlink => crate::holo::theme().ink(),
        SatKind::Navigation => crate::holo::theme().warn(),
        SatKind::Weather => crate::holo::theme().deep(),
        SatKind::Geo => crate::holo::mix(crate::holo::theme().deep(), crate::holo::theme().ink(), 0.5),
        SatKind::Other => accent,
    }
}

/// One satellite: its elements and its propagator.
#[derive(Debug, Clone)]
pub struct Sat {
    pub el: Elements,
    pub model: Sgp4,
    pub kind: SatKind,
}

fn build(elements: Vec<Elements>, group: &str) -> Vec<Sat> {
    elements.into_iter().filter_map(|el| Some(Sat { model: Sgp4::new(&el).ok()?, kind: kind_of(&el, group), el })).collect()
}

/// How JAI names a satellite: "the ISS", "Hubble", "Tiangong", else its catalogue name.
pub fn display_name(el: &Elements) -> String {
    match el.norad {
        25544 => "the ISS".into(),
        20580 => "Hubble".into(),
        48274 => "Tiangong".into(),
        _ => el.name.trim().to_string(),
    }
}

/// A satellite where it was at a snapshot, and how fast its sub-point moves (degrees a second).
#[derive(Debug, Clone, Copy)]
struct Dot {
    idx: u32,
    lat: f64,
    lon: f64,
    alt_km: f64,
    speed_kms: f64,
    dlat: f64,
    dlon: f64,
}

struct Snapshot {
    key: &'static str,
    sats: Arc<Vec<Sat>>,
    dots: Vec<Dot>,
    at: f64,
}

fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

fn snapshot(key: &'static str, sats: Arc<Vec<Sat>>, at: f64) -> Snapshot {
    let dots = sats
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let a = s.model.fix(at).ok()?;
            let b = s.model.fix(at + 10.0).ok()?;
            Some(Dot { idx: i as u32, lat: a.lat, lon: a.lon, alt_km: a.alt_km, speed_kms: a.speed_kms, dlat: (b.lat - a.lat) / 10.0, dlon: wrap_lon(b.lon - a.lon) / 10.0 })
        })
        .collect();
    Snapshot { key, sats, dots, at }
}

/// The selected satellite: its ground track (±45 min) and its footprint.
#[derive(Debug, Clone)]
struct Selected {
    sat: Sat,
    track: Vec<(f64, f64)>,
    at: f64,
}

impl Selected {
    fn new(sat: Sat) -> Self {
        let now = sat_orbit::now_unix();
        let track = sat_orbit::ground_track(&sat.model, now, -45, 45);
        Self { sat, track, at: now }
    }
}

/// The train Sir asked about ("where is that train?").
#[derive(Debug, Clone, PartialEq)]
struct Followed {
    trip_id: String,
    title: String,
    lat: f64,
    lon: f64,
    words: String,
    at: Instant,
}

/// A Finnish train between its last two fixes.
#[derive(Debug, Clone)]
struct FiTrack {
    train: FiTrain,
    from: (f64, f64),
}

/// The box in view (from the last frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewBox {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl ViewBox {
    fn contains_box(&self, inner: &ViewBox) -> bool {
        inner.south >= self.south && inner.north <= self.north && inner.west >= self.west && inner.east <= self.east
    }

    fn intersects(&self, south: f64, west: f64, north: f64, east: f64) -> bool {
        self.south <= north && self.north >= south && self.west <= east && self.east >= west
    }
}

#[derive(Default)]
struct Live {
    demo: bool,
    // ---- satellites
    group: Option<&'static str>,
    catalogs: HashMap<&'static str, (Instant, Arc<Vec<Sat>>)>,
    loading: HashSet<&'static str>,
    failed: HashMap<&'static str, Instant>,
    snap: Option<Arc<Snapshot>>,
    computing: bool,
    selected: Option<Selected>,
    /// Satellites found by name, drawn even without their group.
    singles: Vec<Sat>,
    // ---- trains
    trains: bool,
    segments: Arc<Vec<Segment>>,
    trains_at: Option<Instant>,
    trains_box: Option<ViewBox>,
    trains_loading: bool,
    finland: Arc<Vec<FiTrack>>,
    finland_at: Option<(Instant, f64)>,
    finland_loading: bool,
    followed: Option<Followed>,
    follow_loading: bool,
    // ---- traffic
    traffic: bool,
    traffic_road: Option<String>,
    traffic_loading: bool,
    traffic_last: Option<Instant>,
    roads_loading: bool,
    tfl_loading: bool,
    // ---- the view
    view: Option<ViewBox>,
}

static LIVE: LazyLock<Mutex<Live>> = LazyLock::new(|| Mutex::new(Live::default()));

/// Whether a live layer ("satellites", "trains", "traffic") is on the globe now.
pub fn shown(layer: &str) -> Option<bool> {
    let l = live();
    match layer {
        "satellites" => Some(l.group.is_some() || !l.singles.is_empty()),
        "trains" => Some(l.trains),
        "traffic" => Some(l.traffic),
        _ => None,
    }
}

fn live() -> std::sync::MutexGuard<'static, Live> {
    lock(&LIVE)
}

// ------------------------------------------------------------------- element sets ----

fn cache_file(name: &str) -> PathBuf {
    PathBuf::from(CACHE_DIR).join(name)
}

fn fresh_on_disk(path: &PathBuf, max_age: Duration) -> bool {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < max_age)
}

/// A CelesTrak query's JSON: from the disk cache while fresh, else downloaded (and cached), else
/// the cache however old.
async fn celestrak(query: &str, file: &str, max_age: Duration) -> Result<Value, String> {
    let path = cache_file(file);
    if fresh_on_disk(&path, max_age) {
        if let Some(v) = tokio::fs::read(&path).await.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            return Ok(v);
        }
    }
    let url = format!("{CELESTRAK}?{query}&FORMAT=json");
    match crate::live_layers::get_json_within(&url, Duration::from_secs(60)).await {
        Ok(v) if v.is_array() => {
            let _ = tokio::fs::create_dir_all(CACHE_DIR).await;
            if let Ok(bytes) = serde_json::to_vec(&v) {
                let _ = tokio::fs::write(&path, bytes).await;
            }
            Ok(v)
        }
        other => {
            let stale = tokio::fs::read(&path).await.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            stale.ok_or_else(|| other.err().unwrap_or_else(|| "CelesTrak has no such satellite".into()))
        }
    }
}

fn demo_elements() -> Vec<Elements> {
    serde_json::from_str::<Value>(DEMO_SATELLITES).map(|v| sat_orbit::parse_omm(&v)).unwrap_or_default()
}

/// A group's satellites, loaded (and propagators built) off the GUI thread.
async fn load_group(key: &'static str) -> Result<Arc<Vec<Sat>>, String> {
    if crate::live_layers::offline() || live().demo {
        let sats = Arc::new(build(demo_elements(), key));
        live().catalogs.insert(key, (Instant::now(), sats.clone()));
        return Ok(sats);
    }
    if let Some((at, sats)) = live().catalogs.get(key) {
        if at.elapsed() < SAT_FRESH {
            return Ok(sats.clone());
        }
    }
    let (_, celestrak_group, _) = group(key).ok_or("no such group")?;
    let v = celestrak(&format!("GROUP={celestrak_group}"), &format!("celestrak_{key}.json"), SAT_FRESH).await?;
    let sats = tokio::task::spawn_blocking(move || Arc::new(build(sat_orbit::parse_omm(&v), key))).await.map_err(|e| e.to_string())?;
    if sats.is_empty() {
        return Err("CelesTrak sent no usable element sets".into());
    }
    info!("[EARTH] {} satellites in '{key}'", sats.len());
    live().catalogs.insert(key, (Instant::now(), sats.clone()));
    Ok(sats)
}

/// (alias, NORAD number).
const ALIASES: &[(&str, u32)] = &[
    ("iss", 25544),
    ("the iss", 25544),
    ("international space station", 25544),
    ("space station", 25544),
    ("raumstation", 25544),
    ("hubble", 20580),
    ("hst", 20580),
    ("hubble space telescope", 20580),
    ("tiangong", 48274),
    ("tianhe", 48274),
    ("css", 48274),
    ("chinese space station", 48274),
];

fn norad_of(name: &str) -> Option<u32> {
    let n = name.trim().to_lowercase();
    ALIASES.iter().find(|(a, _)| *a == n).map(|(_, id)| *id).or_else(|| n.parse().ok())
}

/// The best of several element sets for a name: exact first, then the shortest that contains it.
fn best_match(list: Vec<Elements>, name: &str) -> Option<Elements> {
    let want = name.trim().to_uppercase().replace(' ', "");
    let squash = |s: &str| s.to_uppercase().replace([' ', '-', '(', ')'], "");
    let mut hits: Vec<Elements> = list.into_iter().filter(|e| squash(&e.name).contains(&want.replace('-', ""))).collect();
    hits.sort_by_key(|e| (squash(&e.name) != want.replace('-', ""), e.name.len()));
    hits.into_iter().next()
}

/// A satellite's elements by name or NORAD number: the loaded groups first, then CelesTrak (two
/// hours' cache, as CelesTrak asks).
pub async fn elements_for(name: &str) -> Result<Elements, String> {
    let norad = norad_of(name);
    if crate::live_layers::offline() || live().demo {
        let list = demo_elements();
        return match norad {
            Some(id) => list.into_iter().find(|e| e.norad == id),
            None => best_match(list, name),
        }
        .ok_or_else(|| format!("I don't have {name} in the demo's catalogue, Sir."));
    }
    {
        let l = live();
        for (_, sats) in l.catalogs.values() {
            let found = match norad {
                Some(id) => sats.iter().find(|s| s.el.norad == id).map(|s| s.el.clone()),
                None => best_match(sats.iter().map(|s| s.el.clone()).collect(), name),
            };
            if let Some(el) = found {
                return Ok(el);
            }
        }
    }
    let (query, file) = match norad {
        Some(id) => (format!("CATNR={id}"), format!("celestrak_cat_{id}.json")),
        None => {
            let clean: String = name.trim().chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-')).collect();
            (format!("NAME={}", urlencoding::encode(&clean)), format!("celestrak_name_{}.json", clean.to_lowercase().replace(' ', "_")))
        }
    };
    let v = celestrak(&query, &file, Duration::from_secs(2 * 3600)).await.map_err(|_| format!("I couldn't find a satellite called {name}, Sir."))?;
    let list = sat_orbit::parse_omm(&v);
    match norad {
        Some(id) => list.into_iter().find(|e| e.norad == id),
        None => best_match(list, name),
    }
    .ok_or_else(|| format!("I couldn't find a satellite called {name}, Sir."))
}

// ------------------------------------------------------------------------- the actions ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveLayer {
    Satellites,
    Trains,
    Traffic,
}

/// What `open_jai_view {"view": "earth", …}` asks of the live layers.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveAction {
    /// A group's satellites ("active", "starlink", …).
    Satellites(String),
    /// One satellite by name: selected, with its track; the camera goes to it.
    FindSatellite(String),
    /// What is above Sir's horizon now.
    Overhead,
    /// The trains (around a place).
    Trains(Option<String>),
    /// The train of the departures board.
    Train,
    /// The road incidents (on a motorway / around a place).
    Traffic { road: Option<String>, place: Option<String> },
    Hide(LiveLayer),
}

/// The Earth's `parse_action` hook: None when the action is not one of ours.
pub fn parse_action(action: &str, args: &Value) -> Option<Result<LiveAction, String>> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    // (Only `show_…` names and a few unmistakable ones: "traffic", "passes" … are view `live`'s.)
    Some(Ok(match action {
        "show_satellites" | "show_all_satellites" => {
            let key = text("group").or_else(|| text("name")).map(|g| g.to_lowercase()).and_then(|g| group(&g).map(|(k, ..)| *k).or_else(|| group_in(&g))).unwrap_or("active");
            LiveAction::Satellites(key.to_string())
        }
        "find_satellite" | "where_is_satellite" | "track_satellite" => match text("name").or_else(|| text("satellite")) {
            Some(name) => LiveAction::FindSatellite(name),
            None => return Some(Err("find_satellite needs the satellite's name".into())),
        },
        "show_overhead" | "satellites_overhead" => LiveAction::Overhead,
        "show_trains" => LiveAction::Trains(text("place").or_else(|| text("name"))),
        "show_train" | "where_is_train" => LiveAction::Train,
        "show_traffic" => {
            // (A `name` that is no motorway is the place.)
            let named = text("road").or_else(|| text("name"));
            let road = named.as_deref().and_then(crate::traffic::normalize_road);
            let place = text("place").or_else(|| named.filter(|_| road.is_none()));
            LiveAction::Traffic { road, place }
        }
        "hide_layer" => match text("layer").map(|l| l.to_lowercase()).as_deref() {
            Some("satellites" | "satellite" | "sats" | "starlink" | "satelliten") => LiveAction::Hide(LiveLayer::Satellites),
            Some("trains" | "train" | "züge" | "zuege" | "railways") => LiveAction::Hide(LiveLayer::Trains),
            Some("traffic" | "jams" | "verkehr" | "staus" | "roads") => LiveAction::Hide(LiveLayer::Traffic),
            _ => return None,
        },
        _ => return None,
    }))
}

fn digits(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Flies the Earth's camera (the Earth's lock only; ours is not held).
fn fly(lat: f64, lon: f64, alt: f64) {
    crate::earth::state().fly_to(crate::earth::Camera { lat, lon, alt });
}

fn camera_alt() -> f64 {
    crate::earth::state().cam.alt
}

/// The Earth's `act` hook: (the block, the line JAI says).
pub async fn act(action: LiveAction, offline: bool) -> (String, String) {
    let result = match action {
        LiveAction::Satellites(key) => show_group(&key).await,
        LiveAction::FindSatellite(name) => find(&name).await,
        LiveAction::Overhead => overhead().await,
        LiveAction::Trains(place) => trains(place, offline).await,
        LiveAction::Train => train().await,
        LiveAction::Traffic { road, place } => traffic(road, place).await,
        LiveAction::Hide(layer) => {
            let mut l = live();
            let what = match layer {
                LiveLayer::Satellites => {
                    l.group = None;
                    l.selected = None;
                    l.singles.clear();
                    "satellites"
                }
                LiveLayer::Trains => {
                    l.trains = false;
                    l.followed = None;
                    "trains"
                }
                LiveLayer::Traffic => {
                    l.traffic = false;
                    l.traffic_road = None;
                    "traffic"
                }
            };
            Ok((format!("Layer off: {what}."), format!("The {what} are off the map, Sir.").replace("The traffic are", "The traffic is")))
        }
    };
    crate::gui::refresh();
    result.unwrap_or_else(|e| (e.clone(), if e.ends_with("Sir.") { e } else { format!("{e} Sir.").replace(". Sir.", ", Sir.") }))
}

async fn show_group(key: &str) -> Result<(String, String), String> {
    let key: &'static str = group(key).map(|(k, ..)| *k).unwrap_or("active");
    let (_, _, said) = group(key).copied().unwrap_or(("active", "active", "active satellites"));
    {
        let mut l = live();
        l.group = Some(key);
        l.snap = None;
    }
    let sats = load_group(key).await.map_err(|e| format!("CelesTrak didn't answer: {e}."))?;
    if camera_alt() < 9_000.0 {
        let cam = crate::earth::state().cam;
        fly(cam.lat, cam.lon, if key == "geo" { 42_000.0 } else { 14_000.0 });
    }
    let n = sats.len();
    let notable: Vec<&str> = [(25544, "the ISS"), (48274, "Tiangong"), (20580, "Hubble")].iter().filter(|(id, _)| sats.iter().any(|s| s.el.norad == *id)).map(|(_, n)| *n).collect();
    let tail = match notable.as_slice() {
        [] => String::new(),
        [one] => format!("; {one} is labelled"),
        many => format!("; {} and {} are labelled", many[..many.len() - 1].join(", "), many[many.len() - 1]),
    };
    Ok((format!("Satellites: {} {said} · CelesTrak, positions by SGP4", digits(n as i64)), format!("Here are {} {said}, Sir{tail}.", digits(n as i64))))
}

async fn find(name: &str) -> Result<(String, String), String> {
    let el = elements_for(name).await?;
    let label = display_name(&el);
    let model = Sgp4::new(&el)?;
    let sat = Sat { kind: kind_of(&el, ""), model, el };
    let now = sat_orbit::now_unix();
    let at = if crate::live_layers::offline() { sat.el.epoch_unix } else { now };
    let fix = sat.model.fix(at)?;
    {
        let mut l = live();
        if !l.singles.iter().any(|s| s.el.norad == sat.el.norad) {
            l.singles.push(sat.clone());
        }
        l.selected = Some(Selected::new(sat.clone()));
    }
    fly(fix.lat, fix.lon, (fix.alt_km * 12.0).clamp(6_000.0, 40_000.0));
    let over = crate::live_layers::over_what(fix.lat, fix.lon).await;
    let place = over.map(|o| format!("over {o}")).unwrap_or_else(|| format!("over {:.0}° {}, {:.0}° {}", fix.lat.abs(), if fix.lat >= 0.0 { "N" } else { "S" }, fix.lon.abs(), if fix.lon >= 0.0 { "E" } else { "W" }));
    let kmh = digits((fix.speed_kms * 3600.0 / 100.0).round() as i64 * 100);
    let display = format!("{} (NORAD {}) · {place} · {} km up · {kmh} km/h · period {:.0} min", sat.el.name.trim(), sat.el.norad, digits(fix.alt_km.round() as i64), sat.el.period_min());
    let mut lead = label.clone();
    if let Some(c) = lead.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    let spoken_place = place.replace("° N", " degrees north").replace("° S", " degrees south").replace("° E", " degrees east").replace("° W", " degrees west");
    Ok((display, format!("{lead} is {spoken_place} right now, Sir, {} kilometres up and moving at {kmh} kilometres an hour.", digits(fix.alt_km.round() as i64))))
}

/// Everything above Sir's horizon now: (name, elevation, azimuth), highest first.
fn above(sats: &[Sat], o: &Observer, at: f64) -> Vec<(String, f64, f64, u32)> {
    let gmst = sat_orbit::gmst_unix(at);
    let mut out: Vec<(String, f64, f64, u32)> = sats
        .iter()
        .filter_map(|s| {
            let fix = s.model.fix(at).ok()?;
            let (az, el, _) = sat_orbit::look_angles(o, sat_orbit::teme_to_ecef(fix.teme, gmst));
            (el >= sat_orbit::VISIBLE_EL).then(|| (display_name(&s.el), el, az, s.el.norad))
        })
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

async fn overhead() -> Result<(String, String), String> {
    let spot = crate::live_layers::here().await?;
    let key = live().group.unwrap_or("active");
    {
        let mut l = live();
        l.group = Some(key);
    }
    let sats = load_group(key).await.map_err(|e| format!("CelesTrak didn't answer: {e}."))?;
    let observer = Observer { lat: spot.lat, lon: spot.lon, alt_km: 0.3 };
    let at = if crate::live_layers::offline() { sats.first().map_or(sat_orbit::now_unix(), |s| s.el.epoch_unix) } else { sat_orbit::now_unix() };
    let list = {
        let sats = sats.clone();
        tokio::task::spawn_blocking(move || above(&sats, &observer, at)).await.map_err(|e| e.to_string())?
    };
    fly(spot.lat, spot.lon, 6_000.0);
    if let Some(top) = list.first() {
        if let Some(sat) = sats.iter().find(|s| s.el.norad == top.3) {
            live().selected = Some(Selected::new(sat.clone()));
        }
    }
    let mut display = format!("Above your horizon now ({} at 10° or higher)", list.len());
    for (name, el, az, _) in list.iter().take(12) {
        display.push_str(&format!("\n{name} · {el:.0}° {}", sat_orbit::compass(*az)));
    }
    let spoken = match list.as_slice() {
        [] => "Nothing I'm tracking is above your horizon right now, Sir.".to_string(),
        [(name, el, az, _)] => format!("Just one right now, Sir: {name}, {el:.0} degrees up in the {}.", sat_orbit::compass_word(*az)),
        [(name, el, az, _), rest @ ..] => {
            let station = rest.iter().find(|(_, _, _, id)| [25544, 48274].contains(id)).map(|(n, e, _, _)| format!(" {} is up there too, at {e:.0} degrees.", capital(n))).unwrap_or_default();
            format!("{} satellites are above your horizon right now, Sir. The highest is {name}, {el:.0} degrees up in the {}.{station}", digits(list.len() as i64), sat_orbit::compass_word(*az))
        }
    };
    Ok((display, spoken))
}

fn capital(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(c) = out.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    out
}

/// A box around a point for the train map at about 250 km up.
fn train_box(lat: f64, lon: f64) -> (f64, f64, f64, f64) {
    (lat - 0.6, lon - 0.9, lat + 0.6, lon + 0.9)
}

fn in_finland(lat: f64, lon: f64) -> bool {
    (59.5..=70.5).contains(&lat) && (19.0..=32.0).contains(&lon)
}

async fn trains(place: Option<String>, offline: bool) -> Result<(String, String), String> {
    let spot = match place.as_deref() {
        Some(name) => crate::live_layers::place(name).await?,
        None => crate::live_layers::here().await?,
    };
    live().trains = true;
    fly(spot.lat, spot.lon, 250.0);
    let name = if spot.is_here { crate::live_layers::area_name(spot.lat, spot.lon).await.unwrap_or_else(|| "you".into()) } else { spot.name.clone() };
    if in_finland(spot.lat, spot.lon) {
        let list = if offline { demo_finland() } else { crate::transit::finland().await? };
        let n = list.len();
        {
            let mut l = live();
            l.finland = Arc::new(list.into_iter().map(|t| FiTrack { from: (t.lat, t.lon), train: t }).collect());
            l.finland_at = Some((Instant::now(), sat_orbit::now_unix()));
        }
        return Ok((format!("Trains in Finland: {n} with live GPS · Digitraffic"), format!("{n} trains are running in Finland right now, Sir; they're on the map.")));
    }
    let segments = if offline || live().demo {
        demo_segments(chrono::Utc::now())
    } else {
        let (s, w, n, e) = train_box(spot.lat, spot.lon);
        crate::transit::trains_in(s, w, n, e, 11).await.map_err(|e| format!("The train data didn't come through: {e}."))?
    };
    let now = chrono::Utc::now();
    let moving: HashSet<&str> = segments.iter().filter(|s| s.at(now).is_some()).map(|s| s.trip_id.as_str()).collect();
    let n = moving.len();
    {
        let mut l = live();
        let (s, w, nn, e) = train_box(spot.lat, spot.lon);
        l.trains_box = Some(ViewBox { lat: spot.lat, lon: spot.lon, alt: 250.0, south: s, west: w, north: nn, east: e });
        l.segments = Arc::new(segments);
        l.trains_at = Some(Instant::now());
    }
    let spoken = if n == 0 { format!("No trains are moving around {name} at the moment, Sir; I'll keep watching.") } else { format!("Here are the {n} trains moving around {name} right now, Sir.") };
    Ok((format!("Trains around {name}: {n} under way · Transitous (open timetable and real-time data)"), spoken))
}

async fn train() -> Result<(String, String), String> {
    let departure = match crate::transit::focused() {
        Some(d) => d,
        None => {
            // No board yet: the next train near Sir.
            let _ = crate::transit::open_board(&serde_json::json!({ "view": "departures" })).await?;
            crate::transit::focused().ok_or("There's no train on the board to follow, Sir.")?
        }
    };
    let title = format!("{} → {}", departure.line, departure.headsign);
    let (lat, lon, words) = if crate::live_layers::offline() || departure.trip_id.starts_with("demo") {
        let (lat, lon) = (48.7935, 9.2066);
        (lat, lon, "is between Stuttgart Hbf and Bad Cannstatt".to_string())
    } else {
        crate::transit::locate_trip(&departure.trip_id).await?
    };
    {
        let mut l = live();
        l.trains = true;
        l.followed = Some(Followed { trip_id: departure.trip_id.clone(), title: title.clone(), lat, lon, words: words.clone(), at: Instant::now() });
    }
    fly(lat, lon, 150.0);
    Ok((format!("{title}: {words}"), format!("The {} to {} {words}, Sir.", departure.line, departure.headsign)))
}

async fn traffic(road: Option<String>, place: Option<String>) -> Result<(String, String), String> {
    {
        let mut l = live();
        l.traffic = true;
        l.traffic_road = road.clone();
    }
    if let Some(road) = road {
        let list = crate::traffic::road_incidents(&road, true).await.map_err(|e| format!("The Autobahn service didn't answer: {e}."))?;
        let (lat, lon) = if list.is_empty() { (51.0, 10.3) } else { (list.iter().map(|i| i.lat).sum::<f64>() / list.len() as f64, list.iter().map(|i| i.lon).sum::<f64>() / list.len() as f64) };
        fly(lat, lon, if list.is_empty() { 1_400.0 } else { 900.0 });
        let (display, spoken) = crate::traffic::road_answer(&road, &list);
        return Ok((display, spoken.replace(", Sir:", " — it's on the map, Sir:").replace("is clear, Sir", "is clear, Sir — it's on the map")));
    }
    let spot = match place.as_deref() {
        Some(name) => crate::live_layers::place(name).await?,
        None => crate::live_layers::here().await?,
    };
    fly(spot.lat, spot.lon, 400.0);
    let covered = crate::traffic::in_germany(spot.lat, spot.lon) || crate::traffic::in_london(spot.lat, spot.lon);
    let name = if spot.is_here { "you".to_string() } else { spot.name.clone() };
    let spoken = if covered {
        format!("Live traffic around {name} is on the map, Sir: jams, accidents and closures, filling in as the reports arrive.")
    } else {
        "Live traffic is on, Sir, but I only have it for the German motorways and London's roads.".to_string()
    };
    Ok(("Traffic: Autobahn GmbH (German motorways), TfL (London)".into(), spoken))
}

// ----------------------------------------------------------------------- earth's hooks ----

/// Every live layer off (the Earth's "hide all layers").
pub fn hide_all() {
    let mut l = live();
    l.group = None;
    l.selected = None;
    l.singles.clear();
    l.trains = false;
    l.followed = None;
    l.traffic = false;
    l.traffic_road = None;
}

/// A fresh open of the Earth: the live layers start off.
pub fn reset() {
    hide_all();
    let mut l = live();
    l.snap = None;
    l.view = None;
}

/// The demo's data (no network): the satellites of the fixture, made-up trains and incidents.
pub fn load_demo() {
    let mut l = live();
    l.demo = true;
    l.catalogs.clear();
    l.snap = None;
}

/// Back to the live data.
pub fn leave_demo() {
    let mut l = live();
    l.demo = false;
    l.catalogs.clear();
    l.snap = None;
    l.segments = Arc::new(Vec::new());
    l.trains_at = None;
}

/// The Earth's schedule hook (every frame, the Earth's lock held): starts what is due.
pub fn schedule(rt: &Handle, demo: bool) {
    let mut l = live();
    l.demo = demo || crate::live_layers::offline();
    let now = sat_orbit::now_unix();
    // ---- satellites
    if let Some(key) = l.group {
        let loaded = l.catalogs.get(key).map(|(_, sats)| sats.clone());
        let failed_recently = l.failed.get(key).is_some_and(|t| t.elapsed() < SAT_RETRY);
        match loaded {
            Some(sats) if !l.computing && l.snap.as_ref().is_none_or(|s| s.key != key || now - s.at >= SNAPSHOT_EVERY) => {
                l.computing = true;
                rt.spawn_blocking(move || {
                    let snap = Arc::new(snapshot(key, sats, sat_orbit::now_unix()));
                    let mut l = live();
                    l.computing = false;
                    if l.group == Some(key) {
                        l.snap = Some(snap);
                    }
                    drop(l);
                    crate::gui::refresh();
                });
            }
            Some(_) => {}
            None if !l.loading.contains(key) && !failed_recently => {
                l.loading.insert(key);
                rt.spawn(async move {
                    let result = load_group(key).await;
                    let mut l = live();
                    l.loading.remove(key);
                    if let Err(e) = result {
                        debug!("[EARTH] Satellites '{key}': {e}");
                        l.failed.insert(key, Instant::now());
                    }
                    drop(l);
                    crate::gui::refresh();
                });
            }
            None => {}
        }
        // A stale catalogue is fetched again in the background (the old one stays on screen).
        if let Some((at, _)) = l.catalogs.get(key) {
            if !l.demo && at.elapsed() >= SAT_FRESH && !l.loading.contains(key) && !failed_recently {
                l.catalogs.remove(key);
            }
        }
    }
    if let Some(sel) = &mut l.selected {
        if now - sel.at >= 60.0 {
            *sel = Selected::new(sel.sat.clone());
        }
    }
    if l.demo {
        return;
    }
    let Some(view) = l.view else { return };
    // ---- trains
    if l.trains && view.alt <= TRAINS_MAX_ALT_KM && !l.trains_loading {
        let stale = l.trains_at.is_none_or(|t| t.elapsed() >= TRAINS_EVERY);
        let moved = l.trains_box.is_none_or(|b| !b.contains_box(&view));
        let gap_ok = l.trains_at.is_none_or(|t| t.elapsed() >= TRAINS_MIN_GAP);
        let small = view.north - view.south <= 6.0 && view.east - view.west <= 9.0;
        if (stale || moved) && gap_ok && small {
            l.trains_loading = true;
            let pad_lat = (view.north - view.south) * 0.3;
            let pad_lon = (view.east - view.west) * 0.3;
            let fetch = ViewBox { south: view.south - pad_lat, north: view.north + pad_lat, west: view.west - pad_lon, east: view.east + pad_lon, ..view };
            let zoom = if view.alt < 150.0 { 13 } else if view.alt < 400.0 { 11 } else if view.alt < 1_000.0 { 9 } else { 8 };
            rt.spawn(async move {
                let result = crate::transit::trains_in(fetch.south, fetch.west, fetch.north, fetch.east, zoom).await;
                let mut l = live();
                l.trains_loading = false;
                l.trains_at = Some(Instant::now());
                match result {
                    Ok(segments) => {
                        l.segments = Arc::new(segments);
                        l.trains_box = Some(fetch);
                    }
                    Err(e) => debug!("[EARTH] Trains: {e}"),
                }
                drop(l);
                crate::gui::refresh();
            });
        }
    }
    if l.trains && view.alt <= 3_000.0 && view.intersects(59.5, 19.0, 70.5, 32.0) && !l.finland_loading && l.finland_at.is_none_or(|(t, _)| t.elapsed() >= FINLAND_EVERY) {
        l.finland_loading = true;
        rt.spawn(async move {
            let result = crate::transit::finland().await;
            let mut l = live();
            l.finland_loading = false;
            match result {
                Ok(list) => {
                    // Each train glides from where it was drawn to its new fix.
                    let t = l.finland_at.map_or(0.0, |(_, unix)| sat_orbit::now_unix() - unix);
                    let old: HashMap<i64, (f64, f64)> = l.finland.iter().map(|f| (f.train.number, fi_position(f, t))).collect();
                    l.finland = Arc::new(list.into_iter().map(|t| FiTrack { from: old.get(&t.number).copied().unwrap_or((t.lat, t.lon)), train: t }).collect());
                    l.finland_at = Some((Instant::now(), sat_orbit::now_unix()));
                }
                Err(e) => {
                    debug!("[EARTH] Finnish trains: {e}");
                    l.finland_at = Some((Instant::now(), sat_orbit::now_unix()));
                }
            }
            drop(l);
            crate::gui::refresh();
        });
    }
    if let Some(f) = l.followed.clone() {
        if f.at.elapsed() >= Duration::from_secs(30) && !l.follow_loading {
            l.follow_loading = true;
            rt.spawn(async move {
                let result = crate::transit::locate_trip(&f.trip_id).await;
                let mut l = live();
                l.follow_loading = false;
                if let Some(followed) = l.followed.as_mut().filter(|x| x.trip_id == f.trip_id) {
                    followed.at = Instant::now();
                    if let Ok((lat, lon, words)) = result {
                        followed.lat = lat;
                        followed.lon = lon;
                        followed.words = words;
                    }
                }
                drop(l);
                crate::gui::refresh();
            });
        }
    }
    // ---- traffic
    if l.traffic && view.alt <= TRAFFIC_MAX_ALT_KM {
        if view.intersects(47.2, 5.8, 55.1, 15.1) {
            match crate::traffic::cached_roads() {
                None if !l.roads_loading => {
                    l.roads_loading = true;
                    rt.spawn(async {
                        let _ = crate::traffic::roads().await;
                        live().roads_loading = false;
                    });
                }
                Some(roads) if !l.traffic_loading && l.traffic_last.is_none_or(|t| t.elapsed() >= ROAD_GAP) => {
                    // The motorway Sir named first, then the stalest.
                    let next = l.traffic_road.clone().filter(|r| crate::traffic::stalest(std::slice::from_ref(r), crate::traffic::FRESH).is_some()).or_else(|| crate::traffic::stalest(&roads, ROAD_REFRESH));
                    if let Some(road) = next {
                        l.traffic_loading = true;
                        l.traffic_last = Some(Instant::now());
                        rt.spawn(async move {
                            if let Err(e) = crate::traffic::road_incidents(&road, false).await {
                                debug!("[EARTH] Traffic {road}: {e}");
                            }
                            live().traffic_loading = false;
                            crate::gui::refresh();
                        });
                    }
                }
                _ => {}
            }
        }
        if view.alt <= 1_500.0 && view.intersects(51.25, -0.55, 51.75, 0.35) && !l.tfl_loading && crate::traffic::tfl_age().is_none_or(|a| a >= crate::traffic::FRESH) {
            l.tfl_loading = true;
            rt.spawn(async {
                if let Err(e) = crate::traffic::tfl().await {
                    debug!("[EARTH] TfL: {e}");
                }
                live().tfl_loading = false;
                crate::gui::refresh();
            });
        }
    }
}

fn fi_position(f: &FiTrack, secs_since_fix: f64) -> (f64, f64) {
    let t = (secs_since_fix / FINLAND_EVERY.as_secs_f64()).clamp(0.0, 1.0);
    (f.from.0 + (f.train.lat - f.from.0) * t, f.from.1 + (f.train.lon - f.from.1) * t)
}

// ---------------------------------------------------------------------------- painting ----

/// The map as `gui_earth` draws it this frame.
pub struct MapView<'a> {
    pub map: Rect,
    pub alt_km: f64,
    pub centre: (f64, f64),
    pub px_per_km: f64,
    /// Where a point is on screen and how much of it shows (None: behind the globe).
    pub at: &'a dyn Fn(f64, f64) -> Option<(Pos2, f32)>,
    /// A line as runs on screen.
    pub runs: &'a dyn Fn(&[(f64, f64)]) -> Vec<Vec<Pos2>>,
    pub pointer: Option<Pos2>,
    pub rim: Color32,
    pub accent: Color32,
    /// The Earth's own ISS layer is on (it draws and labels the ISS itself).
    pub iss_layer: bool,
}

/// A label the layers ask for (the Earth places it).
pub struct LiveLabel {
    pub at: Pos2,
    pub gap: f32,
    pub text: String,
    pub colour: Color32,
    pub rank: f32,
}

/// What the live layers drew this frame.
#[derive(Default)]
pub struct Painted {
    pub labels: Vec<LiveLabel>,
    pub glyphs: Vec<Rect>,
    /// The thing nearest the pointer: where, its radius, what.
    pub hover: Option<(Pos2, f32, Hit)>,
    /// Something moves on its own (repaint soon).
    pub moving: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Satellite(u32),
    Train(String),
    FinnishTrain(i64),
    Incident(String),
}

/// The thing under the pointer, with the lines of its card.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub target: Target,
    lines: Vec<String>,
}

impl Hit {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

use crate::holo::{stroke, theme};

fn white() -> Color32 {
    theme().hot()
}
fn jam_red() -> Color32 {
    theme().alert()
}

fn train_colour(s: &Segment) -> Color32 {
    if let Some([r, g, b]) = s.colour {
        // Too dark a route colour would vanish on the night side.
        let lum = 0.3 * r as f32 + 0.59 * g as f32 + 0.11 * b as f32;
        if lum > 60.0 {
            return Color32::from_rgb(r, g, b);
        }
    }
    match s.mode.as_str() {
        "HIGHSPEED_RAIL" | "LONG_DISTANCE" | "NIGHT_RAIL" => white(),
        "SUBURBAN" | "METRO" => theme().deep(),
        "SUBWAY" => theme().ink(),
        "TRAM" => theme().warn(),
        _ => theme().accent(),
    }
}

fn incident_colour(kind: crate::traffic::Kind) -> Color32 {
    use crate::traffic::Kind;
    match kind {
        Kind::Accident | Kind::Jam | Kind::Closure => jam_red(),
        Kind::Hazard => theme().warn(),
        Kind::Roadworks => crate::holo::mix(theme().warn(), theme().alert(), 0.5),
    }
}

/// Points on a circle of `radius_km` around a point.
/// A dashed line, only where it crosses `map`: a footprint thousands of kilometres round is
/// millions of pixels at street level, and dashing all of it took most of a frame.
pub fn dashed_on(map: Rect, run: &[Pos2], stroke: Stroke, dash: f32, gap: f32) -> Vec<Shape> {
    let map = map.expand(stroke.width + 2.0);
    let mut pieces: Vec<Vec<Pos2>> = Vec::new();
    let mut open = false;
    for w in run.windows(2) {
        let Some((a, b)) = clip_segment(w[0], w[1], map) else {
            open = false;
            continue;
        };
        match pieces.last_mut().filter(|_| open) {
            Some(piece) => piece.push(b),
            None => pieces.push(vec![a, b]),
        }
        open = b == w[1];
    }
    pieces.iter().flat_map(|piece| Shape::dashed_line(piece, stroke, dash, gap)).collect()
}

/// The part of segment `a`–`b` inside `r` (Liang–Barsky).
fn clip_segment(a: Pos2, b: Pos2, r: Rect) -> Option<(Pos2, Pos2)> {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-d.x, a.x - r.min.x), (d.x, r.max.x - a.x), (-d.y, a.y - r.min.y), (d.y, r.max.y - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
        if t0 > t1 {
            return None;
        }
    }
    Some((a + d * t0, a + d * t1))
}

fn circle(centre: (f64, f64), radius_km: f64, steps: usize) -> Vec<(f64, f64)> {
    let (lat, lon) = (centre.0.to_radians(), centre.1.to_radians());
    let d = radius_km / 6371.0;
    (0..=steps)
        .map(|k| {
            let b = std::f64::consts::TAU * k as f64 / steps as f64;
            let lat2 = (lat.sin() * d.cos() + lat.cos() * d.sin() * b.cos()).asin();
            let lon2 = lon + (b.sin() * d.sin() * lat.cos()).atan2(d.cos() - lat.sin() * lat2.sin());
            (lat2.to_degrees(), wrap_lon(lon2.to_degrees()))
        })
        .collect()
}

/// The footprint of a satellite: how far from its sub-point it is above the horizon.
fn footprint_km(alt_km: f64) -> f64 {
    6371.0 * (6371.0 / (6371.0 + alt_km.max(0.0))).acos()
}

/// Draws the live layers; called by the Earth's `paint_layers` with its projection.
pub fn paint(p: &Painter, v: &MapView) -> Painted {
    let mut out = Painted::default();
    let inside = |at: Pos2, r: f32| v.map.expand(r).contains(at);
    let mut best: Option<(f32, Pos2, f32, Hit)> = None;
    let mut consider = |at: Pos2, r: f32, hit: &dyn Fn() -> Hit| {
        let Some(pointer) = v.pointer else { return };
        let d = (pointer - at).length();
        if d <= r.max(5.0) + 3.0 && best.as_ref().is_none_or(|(b, ..)| d < *b) {
            best = Some((d, at, r, hit()));
        }
    };
    // The view box, for the schedule (and what is asked for next).
    let km_per_px = 1.0 / v.px_per_km.max(1e-9);
    let half_lat = (v.map.height() as f64 / 2.0 * km_per_px / 111.2).min(85.0);
    let half_lon = (v.map.width() as f64 / 2.0 * km_per_px / (111.2 * v.centre.0.to_radians().cos().max(0.05))).min(180.0);
    let (snap, singles, selected, segments, finland, finland_unix, followed, traffic, road) = {
        let mut l = live();
        l.view = Some(ViewBox { lat: v.centre.0, lon: v.centre.1, alt: v.alt_km, south: (v.centre.0 - half_lat).max(-85.0), north: (v.centre.0 + half_lat).min(85.0), west: v.centre.1 - half_lon, east: v.centre.1 + half_lon });
        let snap = l.group.and(l.snap.clone());
        let segments = l.trains.then(|| l.segments.clone());
        let finland = l.trains.then(|| l.finland.clone());
        (snap, l.singles.clone(), l.selected.clone(), segments, finland, l.finland_at.map(|(_, u)| u), l.followed.clone(), l.traffic, l.traffic_road.clone())
    };
    let now = sat_orbit::now_unix();

    // ---- traffic (under everything else)
    if traffic && v.alt_km <= TRAFFIC_MAX_ALT_KM * 1.2 {
        let incidents = crate::traffic::cached();
        let close = v.alt_km <= 700.0;
        for i in incidents.iter().filter(|i| road.as_ref().is_none_or(|r| &i.road == r)) {
            if i.kind == crate::traffic::Kind::Roadworks && !close && road.is_none() {
                continue;
            }
            let c = incident_colour(i.kind);
            if matches!(i.kind, crate::traffic::Kind::Jam | crate::traffic::Kind::Closure) && i.line.len() >= 2 {
                for run in (v.runs)(&i.line) {
                    p.add(Shape::line(run, stroke(3.0, c.gamma_multiply(0.85))));
                }
            }
            let Some((at, vis)) = (v.at)(i.lat, i.lon).filter(|(at, _)| inside(*at, 8.0)) else { continue };
            incident_glyph(p, at, i.kind, c.gamma_multiply(vis));
            out.glyphs.push(Rect::from_center_size(at, Vec2::splat(12.0)));
            if close && matches!(i.kind, crate::traffic::Kind::Jam | crate::traffic::Kind::Accident) {
                let text = match i.delay_min {
                    Some(m) if m > 0 => format!("{} +{m}'", i.road),
                    _ => i.road.clone(),
                };
                out.labels.push(LiveLabel { at, gap: 9.0, text, colour: c.gamma_multiply(vis), rank: 3.0 });
            }
            consider(at, 6.0, &|| Hit { target: Target::Incident(i.id.clone()), lines: incident_lines(i) });
        }
    }

    // ---- trains
    if let Some(segments) = segments.filter(|_| v.alt_km <= TRAINS_MAX_ALT_KM * 1.2) {
        let t = chrono::Utc::now();
        let mut drawn: HashSet<&str> = HashSet::new();
        let mut draw = |s: &Segment, pos: (f64, f64), dwelling: bool| {
            let Some((at, vis)) = (v.at)(pos.0, pos.1).filter(|(at, _)| inside(*at, 5.0)) else { return };
            let c = train_colour(s);
            p.circle_filled(at, 4.2, crate::holo::a(theme().void(), 0.78 * vis));
            p.circle_filled(at, 3.0, c.gamma_multiply(vis * if dwelling { 0.7 } else { 1.0 }));
            consider(at, 4.0, &|| Hit { target: Target::Train(s.trip_id.clone()), lines: segment_lines(s, dwelling) });
        };
        for s in segments.iter() {
            if let Some(pos) = s.at(t) {
                if drawn.insert(s.trip_id.as_str()) {
                    draw(s, pos, false);
                }
            }
        }
        for s in segments.iter() {
            if s.departs > t && s.departs - t <= chrono::Duration::minutes(2) && !drawn.contains(s.trip_id.as_str()) {
                if let Some(first) = s.path.first() {
                    drawn.insert(s.trip_id.as_str());
                    draw(s, *first, true);
                }
            }
        }
        out.moving |= !segments.is_empty();
    }
    if let Some(finland) = finland.filter(|f| !f.is_empty() && v.alt_km <= 3_600.0) {
        let since = finland_unix.map_or(0.0, |u| now - u);
        for f in finland.iter() {
            let pos = fi_position(f, since);
            let Some((at, vis)) = (v.at)(pos.0, pos.1).filter(|(at, _)| inside(*at, 5.0)) else { continue };
            p.circle_filled(at, 4.0, crate::holo::a(theme().void(), 0.78 * vis));
            p.circle_filled(at, 2.8, theme().deep().gamma_multiply(vis));
            consider(at, 4.0, &|| Hit { target: Target::FinnishTrain(f.train.number), lines: vec![format!("Train {}", f.train.number), format!("{:.0} km/h · live GPS", f.train.speed_kmh), "Finland · Digitraffic".into()] });
        }
        out.moving = true;
    }
    if let Some(f) = &followed {
        if let Some((at, vis)) = (v.at)(f.lat, f.lon).filter(|(at, _)| inside(*at, 12.0)) {
            p.circle_stroke(at, 9.0, stroke(1.5, white().gamma_multiply(vis)));
            p.circle_filled(at, 4.0, v.accent.gamma_multiply(vis));
            out.glyphs.push(Rect::from_center_size(at, Vec2::splat(20.0)));
            out.labels.push(LiveLabel { at, gap: 12.0, text: f.title.clone(), colour: white().gamma_multiply(vis), rank: 0.2 });
            consider(at, 9.0, &|| Hit { target: Target::Train(f.trip_id.clone()), lines: vec![f.title.clone(), f.words.clone()] });
        }
    }

    // ---- satellites
    if let Some(sel) = &selected {
        let fix = sel.sat.model.fix(now).ok();
        for run in (v.runs)(&sel.track) {
            p.add(Shape::line(run, stroke(1.3, white().gamma_multiply(0.55))));
        }
        if let Some(fix) = fix {
            for run in (v.runs)(&circle((fix.lat, fix.lon), footprint_km(fix.alt_km), 96)) {
                p.extend(dashed_on(v.map, &run, stroke(1.0, v.rim.gamma_multiply(0.75)), 5.0, 4.0));
            }
        }
    }
    let mut labelled: HashSet<u32> = HashSet::new();
    if v.iss_layer {
        labelled.insert(25544);
    }
    if let Some(snap) = &snap {
        let t = (now - snap.at).clamp(0.0, 30.0);
        let size = if snap.dots.len() > 3_000 { 2.4 } else { 3.4 };
        let mut mesh = Mesh::default();
        for d in &snap.dots {
            let (lat, lon) = ((d.lat + d.dlat * t).clamp(-90.0, 90.0), wrap_lon(d.lon + d.dlon * t));
            let Some((at, vis)) = (v.at)(lat, lon).filter(|(at, _)| inside(*at, 2.0)) else { continue };
            let sat = &snap.sats[d.idx as usize];
            let c = kind_colour(sat.kind, v.accent).gamma_multiply(vis);
            mesh.add_colored_rect(Rect::from_center_size(at, Vec2::splat(size)), c);
            if LANDMARKS.contains(&sat.el.norad) && labelled.insert(sat.el.norad) {
                out.labels.push(LiveLabel { at, gap: 6.0, text: display_name(&sat.el).trim_start_matches("the ").to_string(), colour: white().gamma_multiply(vis), rank: 0.6 });
                out.glyphs.push(Rect::from_center_size(at, Vec2::splat(6.0)));
            }
            consider(at, size, &|| Hit { target: Target::Satellite(sat.el.norad), lines: sat_lines(&sat.el, d.alt_km, d.speed_kms) });
        }
        if !mesh.is_empty() {
            p.add(Shape::mesh(mesh));
        }
        out.moving = true;
    }
    // The satellites found by name (and the selected one) on top.
    for sat in singles.iter().chain(selected.as_ref().map(|s| &s.sat)) {
        let Ok(fix) = sat.model.fix(now) else { continue };
        let Some((at, vis)) = (v.at)(fix.lat, fix.lon).filter(|(at, _)| inside(*at, 8.0)) else { continue };
        let is_selected = selected.as_ref().is_some_and(|s| s.sat.el.norad == sat.el.norad);
        p.circle_filled(at, 4.0, kind_colour(sat.kind, v.accent).gamma_multiply(vis));
        if is_selected {
            p.circle_stroke(at, 8.0, stroke(1.4, white().gamma_multiply(vis)));
        }
        if labelled.insert(sat.el.norad) {
            out.labels.push(LiveLabel { at, gap: 10.0, text: display_name(&sat.el).trim_start_matches("the ").to_string(), colour: white().gamma_multiply(vis), rank: if is_selected { 0.1 } else { 0.7 } });
        }
        out.glyphs.push(Rect::from_center_size(at, Vec2::splat(10.0)));
        consider(at, 6.0, &|| Hit { target: Target::Satellite(sat.el.norad), lines: sat_lines(&sat.el, fix.alt_km, fix.speed_kms) });
        out.moving = true;
    }
    out.hover = best.map(|(_, at, r, hit)| (at, r, hit));
    out
}

fn sat_lines(el: &Elements, alt_km: f64, speed_kms: f64) -> Vec<String> {
    vec![
        el.name.trim().to_string(),
        format!("{} km · {} km/h", digits(alt_km.round() as i64), digits((speed_kms * 3600.0).round() as i64)),
        format!("NORAD {} · {} · period {:.0} min", el.norad, el.object_id, el.period_min()),
    ]
}

fn segment_lines(s: &Segment, dwelling: bool) -> Vec<String> {
    let tz = crate::transit::local_offset();
    let clock = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&tz).format("%H:%M").to_string();
    vec![
        format!("{} · {}", s.line, crate::transit::mode_label(&s.mode)),
        if dwelling { format!("At {} until {}", s.from, clock(s.departs)) } else { format!("{} → {}", s.from, s.to) },
        format!("{} – {}{}", clock(s.departs), clock(s.arrives), if s.realtime { " · live" } else { " · timetable" }),
    ]
}

fn incident_lines(i: &crate::traffic::Incident) -> Vec<String> {
    let mut lines = vec![format!("{} · {}", i.road, i.kind.label())];
    if !i.section.is_empty() {
        lines.push(i.section.clone());
    }
    if !i.direction.trim().is_empty() {
        lines.push(i.direction.trim().to_string());
    }
    lines.extend(i.details.iter().take(2).map(|d| d.chars().take(90).collect::<String>()));
    if let Some(m) = i.delay_min {
        lines.push(format!("+{m} min"));
    }
    lines
}

/// A road incident's icon: a jam's bar, a closure's no-entry, a warning's triangle, roadworks'
/// diamond.
fn incident_glyph(p: &Painter, at: Pos2, kind: crate::traffic::Kind, c: Color32) {
    use crate::traffic::Kind;
    let dark = crate::holo::a(theme().void(), 0.82);
    match kind {
        Kind::Closure => {
            p.circle_filled(at, 5.5, c);
            p.line_segment([at - vec2(3.0, 0.0), at + vec2(3.0, 0.0)], stroke(1.6, white()));
        }
        Kind::Jam => {
            p.circle_filled(at, 5.5, c);
            p.line_segment([at - vec2(3.0, 1.6), at + vec2(3.0, -1.6)], stroke(1.3, dark));
            p.line_segment([at - vec2(3.0, -1.6), at + vec2(3.0, 1.6)], stroke(1.3, dark));
        }
        Kind::Roadworks => {
            p.add(Shape::convex_polygon(vec![at + vec2(0.0, -5.0), at + vec2(5.0, 0.0), at + vec2(0.0, 5.0), at + vec2(-5.0, 0.0)], c, stroke(1.0, dark)));
        }
        Kind::Accident | Kind::Hazard => {
            p.add(Shape::convex_polygon(vec![at + vec2(0.0, -6.0), at + vec2(5.5, 4.0), at + vec2(-5.5, 4.0)], c, stroke(1.0, dark)));
            p.line_segment([at + vec2(0.0, -2.5), at + vec2(0.0, 1.0)], stroke(1.3, dark));
            p.circle_filled(at + vec2(0.0, 2.6), 0.7, dark);
        }
    }
}

/// A click on the map: a satellite is selected (its track and footprint), a train followed;
/// a click on nothing lets the selection go.
pub fn clicked(hit: Option<&Hit>) {
    let mut l = live();
    match hit.map(|h| &h.target) {
        Some(Target::Satellite(norad)) => {
            let sat = l.snap.as_ref().and_then(|s| s.sats.iter().find(|x| x.el.norad == *norad).cloned()).or_else(|| l.singles.iter().find(|x| x.el.norad == *norad).cloned());
            if let Some(sat) = sat {
                l.selected = Some(Selected::new(sat));
            }
        }
        Some(Target::Train(trip)) => {
            let segment = l.segments.iter().find(|s| &s.trip_id == trip).cloned();
            if let Some(s) = segment {
                let pos = s.at(chrono::Utc::now()).or_else(|| s.path.first().copied()).unwrap_or((0.0, 0.0));
                l.followed = Some(Followed { trip_id: s.trip_id.clone(), title: format!("{} → {}", s.line, s.to), lat: pos.0, lon: pos.1, words: format!("between {} and {}", s.from, s.to), at: Instant::now() - Duration::from_secs(25) });
            }
        }
        Some(_) => {}
        None => {
            l.selected = None;
            l.followed = None;
        }
    }
    drop(l);
    crate::gui::refresh();
}

/// The legend's entries for the layers that are on: (colour, text).
pub fn legend() -> Vec<(Color32, String)> {
    let l = live();
    let mut out = Vec::new();
    if l.group.is_some() {
        let n = l.snap.as_ref().map_or(0, |s| s.dots.len());
        out.push((theme().ink(), if n == 0 { "SAT …".to_string() } else { format!("{} SAT", digits(n as i64)) }));
    }
    if l.trains {
        let t = chrono::Utc::now();
        let n = l.segments.iter().filter(|s| s.at(t).is_some()).map(|s| s.trip_id.as_str()).collect::<HashSet<_>>().len() + l.finland.len();
        out.push((theme().deep(), format!("{n} TRAINS")));
    }
    if l.traffic {
        drop(l);
        let n = crate::traffic::cached().iter().filter(|i| i.kind != crate::traffic::Kind::Roadworks).count();
        out.push((jam_red(), format!("{n} ROAD")));
    }
    out
}

/// The sources of the layers that are on.
pub fn credits() -> Vec<&'static str> {
    let l = live();
    let mut out = Vec::new();
    if l.group.is_some() || !l.singles.is_empty() {
        out.push("Satellites: CelesTrak (SGP4)");
    }
    if l.trains {
        out.push("Trains: Transitous, Digitraffic");
    }
    if l.traffic {
        out.push("Traffic: Autobahn GmbH, TfL");
    }
    out
}

// -------------------------------------------------------------------------------- demo ----

/// A few trains around Stuttgart, moving now (made up, on real lines).
pub fn demo_segments(now: chrono::DateTime<chrono::Utc>) -> Vec<Segment> {
    let seg = |id: &str, line: &str, mode: &str, from: (&str, f64, f64), to: (&str, f64, f64), started: i64, minutes: i64| Segment {
        trip_id: format!("demo-{id}"),
        line: line.into(),
        mode: mode.into(),
        from: from.0.into(),
        to: to.0.into(),
        departs: now - chrono::Duration::minutes(started),
        arrives: now - chrono::Duration::minutes(started) + chrono::Duration::minutes(minutes),
        path: vec![(from.1, from.2), (to.1, to.2)],
        colour: None,
        realtime: true,
    };
    let hbf = ("Stuttgart Hbf", 48.7848, 9.1832);
    vec![
        seg("s2", "S2", "METRO", hbf, ("Bad Cannstatt", 48.8027, 9.2156), 1, 4),
        seg("re5", "RE 5", "REGIONAL_RAIL", ("Esslingen", 48.7394, 9.3047), hbf, 3, 9),
        seg("ice", "ICE 594", "HIGHSPEED_RAIL", ("Vaihingen (Enz)", 48.9286, 8.9568), hbf, 6, 14),
        seg("u6", "U6", "SUBWAY", hbf, ("Pragsattel", 48.8106, 9.1825), 2, 6),
    ]
}

fn demo_finland() -> Vec<FiTrain> {
    vec![FiTrain { number: 73, lat: 63.9317, lon: 27.4542, speed_kmh: 126.0, at: None }, FiTrain { number: 9, lat: 60.4, lon: 24.9, speed_kmh: 160.0, at: None }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_demo_catalogue_loads_and_propagates() {
        let list = demo_elements();
        assert!(list.len() >= 12, "{}", list.len());
        let sats = build(list, "active");
        assert_eq!(sats.iter().filter(|s| s.kind == SatKind::Station).count(), 3, "ISS, Tiangong, Hubble");
        // A cubesat set loose from the ISS is no station; a module of Tiangong is.
        let named = |name: &str| Elements { name: name.into(), norad: 99_999, ..sats[0].el.clone() };
        assert_ne!(kind_of(&named("ISS OBJECT YQ"), "active"), SatKind::Station);
        assert_eq!(kind_of(&named("CSS (WENTIAN)"), "active"), SatKind::Station);
        assert!(sats.iter().any(|s| s.kind == SatKind::Navigation));
        assert!(sats.iter().any(|s| s.kind == SatKind::Weather || s.kind == SatKind::Geo));
        let at = sats[0].el.epoch_unix;
        let snap = snapshot("active", Arc::new(sats), at);
        assert!(snap.dots.len() >= 12);
        // The ISS's sub-point moves about 0.06° a second.
        let iss = snap.dots.iter().find(|d| snap.sats[d.idx as usize].el.norad == 25544).unwrap();
        let rate = (iss.dlat.powi(2) + (iss.dlon * iss.lat.to_radians().cos()).powi(2)).sqrt();
        assert!((0.03..0.09).contains(&rate), "{rate}");
        assert!((380.0..460.0).contains(&iss.alt_km), "{}", iss.alt_km);
    }

    #[test]
    fn actions_are_ours_only_when_they_are() {
        let a = |v: Value| parse_action(v["action"].as_str().unwrap(), &v);
        assert_eq!(a(json!({"action": "show_satellites", "group": "Starlink"})), Some(Ok(LiveAction::Satellites("starlink".into()))));
        assert_eq!(a(json!({"action": "show_satellites", "group": "gps-ops"})), Some(Ok(LiveAction::Satellites("gps".into()))));
        assert_eq!(a(json!({"action": "show_satellites"})), Some(Ok(LiveAction::Satellites("active".into()))));
        assert_eq!(a(json!({"action": "find_satellite", "name": "Hubble"})), Some(Ok(LiveAction::FindSatellite("Hubble".into()))));
        assert!(matches!(a(json!({"action": "find_satellite"})), Some(Err(_))));
        assert_eq!(a(json!({"action": "show_traffic", "road": "a 8"})), Some(Ok(LiveAction::Traffic { road: Some("A8".into()), place: None })));
        assert_eq!(a(json!({"action": "show_trains", "place": "Stuttgart"})), Some(Ok(LiveAction::Trains(Some("Stuttgart".into())))));
        assert_eq!(a(json!({"action": "hide_layer", "layer": "satellites"})), Some(Ok(LiveAction::Hide(LiveLayer::Satellites))));
        // The Earth's own.
        assert_eq!(a(json!({"action": "hide_layer", "layer": "flights"})), None);
        assert_eq!(a(json!({"action": "show_satellite"})), None);
        assert_eq!(a(json!({"action": "show_iss"})), None);
    }

    #[test]
    fn groups_and_names() {
        assert_eq!(group_in("show me starlink"), Some("starlink"));
        assert_eq!(group_in("zeig mir die gps satelliten"), Some("gps"));
        assert_eq!(group_in("show me all satellites"), Some("active"));
        assert_eq!(group_in("show me the weather satellites"), Some("weather"));
        assert_eq!(group_in("show me the satellite view"), None);
        assert_eq!(norad_of("Hubble"), Some(20580));
        assert_eq!(norad_of("25544"), Some(25544));
        let list = demo_elements();
        assert_eq!(best_match(list.clone(), "noaa 20").map(|e| e.norad), list.iter().find(|e| e.name.starts_with("NOAA 20")).map(|e| e.norad));
        assert_eq!(digits(27_580), "27,580");
        assert_eq!(digits(-1_234_567), "-1,234,567");
    }

    #[test]
    fn overhead_and_geometry() {
        let sats = build(demo_elements(), "active");
        let at = sats[0].el.epoch_unix;
        // Somewhere on the equator under the geostationary belt something is always up.
        let o = Observer { lat: 0.0, lon: 0.0, alt_km: 0.0 };
        let list = above(&sats, &o, at);
        assert!(list.windows(2).all(|w| w[0].1 >= w[1].1));
        let c = circle((48.0, 9.0), 100.0, 36);
        assert!(c.iter().all(|p| (crate::live_layers::km((48.0, 9.0), *p) - 100.0).abs() < 0.5));
        assert!((footprint_km(420.0) - 2_250.0).abs() < 60.0, "{}", footprint_km(420.0));
    }

    #[test]
    fn the_offline_actions_answer() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // (The camera calls need the Earth's state; it exists without a window.)
        let (_, spoken) = rt.block_on(show_group("active")).unwrap();
        assert!(spoken.starts_with("Here are ") && spoken.contains("the ISS"), "{spoken}");
        let (_, spoken) = rt.block_on(find("Hubble")).unwrap();
        assert!(spoken.starts_with("Hubble is over "), "{spoken}");
        let (_, spoken) = rt.block_on(overhead()).unwrap();
        assert!(spoken.contains("Sir"), "{spoken}");
        let (_, spoken) = rt.block_on(trains(Some("Stuttgart".into()), true)).unwrap();
        assert!(spoken.contains("Stuttgart"), "{spoken}");
        hide_all();
    }

    #[test]
    fn demo_trains_move() {
        let now = chrono::Utc::now();
        let segs = demo_segments(now);
        assert!(segs.iter().all(|s| s.at(now).is_some()));
    }

    #[test]
    fn dashes_only_where_the_line_crosses_the_map() {
        // A footprint 10 million pixels round through a 1000 px map: a handful of dashes.
        let map = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0));
        let r = 1.6e6_f32;
        let ring: Vec<Pos2> = (0..=96).map(|k| {
            let a = std::f32::consts::TAU * k as f32 / 96.0;
            Pos2::new(500.0 + r * a.cos() - r, 300.0f32 + r * a.sin())
        }).collect();
        let dashes = dashed_on(map, &ring, Stroke::new(1.0f32, white()), 5.0, 4.0);
        assert!((50..400).contains(&dashes.len()), "{} dashes", dashes.len());
        assert!(dashed_on(map, &[Pos2::new(-50.0f32, -50.0f32), Pos2::new(-10.0f32, 900.0f32)], Stroke::new(1.0f32, white()), 5.0, 4.0).is_empty());
    }
}
