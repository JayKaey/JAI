//! Live public cameras (`show_live_camera`): traffic and weather webcams worldwide, from keyless
//! sources that publish their camera lists and still frames openly.
//!
//! * **Sources** (verified 2026-11-08/09): Transport for London JamCams, Caltrans CCTV (twelve
//!   districts), Hong Kong Transport Department, NZTA Waka Kotahi, foto-webcam.eu (the Alps and
//!   southern Germany) and the public camera maps of the 511 traveller sites (Pennsylvania,
//!   Florida, Arizona, Louisiana, Idaho, Alaska, New England, Connecticut). Each list is fetched
//!   only when a place near it is asked for (the whole world for the Earth's camera layer) and
//!   kept for 24 hours in `config/webcam_index.json`.
//! * **Viewer** (`gui_media.rs`): one camera large with the next nearest beside it, or a camera
//!   wall (2×2 or 3×3). Frames are still JPEGs fetched again every few seconds with
//!   `If-Modified-Since` (no video); the block shows the place, the source and the frame's age.
//! * **The Earth**: a camera layer (`earth::Layer::Cameras`); a click on a camera opens its feed.
//!
//! Tests and dry runs use a stand-in index and frames (`image_viewer::offline`).

use crate::image_viewer::{offline, pixels};
use crate::tools::{bool_arg, int_arg, str_arg, ToolContext};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock};
use std::time::Duration;
use tracing::{info, warn};

pub const TOOL: &str = "show_live_camera";
const INDEX_FILE: &str = "config/webcam_index.json";
const INDEX_TTL_SECS: i64 = 24 * 3600;
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Within this distance a camera is "in" the place asked for.
const IN_PLACE_KM: f64 = 30.0;
/// Camera lists within this distance of a place are fetched for it.
const NEAR_SOURCES_KM: f64 = 250.0;
/// Frames are decoded to at most this many pixels a side.
pub const FRAME_SIDE: u32 = 1280;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One public camera.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cam {
    /// "tfl:00002.00865", "511:ctroads.org:308".
    pub id: String,
    pub name: String,
    /// "London", "Oakland, California", "Wan Chai, Hong Kong".
    pub area: String,
    pub lat: f64,
    pub lon: f64,
    /// The still frame.
    pub image: String,
    /// "TfL JamCams", "Caltrans CCTV" …
    pub source: String,
    /// How often the source makes a new frame, in seconds (0: unknown).
    #[serde(default)]
    pub interval: u32,
}

impl Cam {
    pub fn key(&self) -> String {
        format!("cam:{}", self.id)
    }

    pub fn origin(&self) -> pixels::Origin {
        pixels::Origin::Web(vec![self.image.clone()])
    }

    /// Seconds between two fetches of its frame: a few seconds, longer for cameras that only
    /// take a picture every few minutes (a 304 is cheap, a fresh 300 kB frame is not).
    pub fn refresh_secs(&self, on_wall: bool) -> u64 {
        let base = if on_wall { 10 } else { 5 };
        match self.interval {
            n if n >= 300 => 60,
            n if n >= 60 => base.max(15),
            _ => base,
        }
    }

    /// Its name (a 511 camera's own name once it is known).
    pub fn title(&self) -> String {
        lock(&NAMES).get(&self.id).cloned().unwrap_or_else(|| self.name.clone())
    }
}

// ------------------------------------------------------------------------------- sources ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Tfl,
    Caltrans(u8),
    HongKong,
    Nzta,
    FotoWebcam,
    /// The camera map of a 511 traveller site: `/map/mapIcons/Cameras`, `/map/Cctv/{id}`.
    Roads511 { host: &'static str, region: &'static str, label: &'static str },
}

#[derive(Debug, Clone)]
struct SourceDef {
    key: String,
    url: String,
    kind: Kind,
    /// lat min, lat max, lon min, lon max.
    bbox: [f64; 4],
}

impl SourceDef {
    /// Whether the source has cameras within `km` of a point.
    fn covers(&self, lat: f64, lon: f64, km: f64) -> bool {
        let [s, n, w, e] = self.bbox;
        let dlat = km / 111.0;
        let dlon = km / (111.0 * lat.to_radians().cos().abs().max(0.1));
        lat >= s - dlat && lat <= n + dlat && lon >= w - dlon && lon <= e + dlon
    }
}

/// (The last eight checked on 2026-11-26: keyless, the same map, about 6,400 cameras more.)
const ROADS_511: [(&str, &str, &str, [f64; 4]); 16] = [
    ("www.511pa.com", "Pennsylvania", "511PA", [39.7, 42.3, -80.6, -74.6]),
    ("fl511.com", "Florida", "FL511", [24.4, 31.1, -87.7, -79.9]),
    ("az511.gov", "Arizona", "AZ511", [31.3, 37.1, -114.9, -109.0]),
    ("511la.org", "Louisiana", "511LA", [28.9, 33.1, -94.1, -88.8]),
    ("511.idaho.gov", "Idaho", "Idaho 511", [41.9, 49.1, -117.3, -111.0]),
    ("511.alaska.gov", "Alaska", "Alaska 511", [51.0, 71.5, -170.0, -129.0]),
    ("www.newengland511.org", "New England", "New England 511", [42.6, 47.5, -73.5, -66.9]),
    ("ctroads.org", "Connecticut", "CTroads", [40.9, 42.1, -73.8, -71.7]),
    ("511ga.org", "Georgia", "511GA", [30.3, 35.1, -85.7, -80.7]),
    ("511on.ca", "Ontario", "Ontario 511", [41.6, 52.0, -95.2, -74.3]),
    ("511.alberta.ca", "Alberta", "511 Alberta", [48.9, 60.1, -120.1, -109.9]),
    ("www.nvroads.com", "Nevada", "NVroads", [35.0, 42.1, -120.1, -114.0]),
    ("511.novascotia.ca", "Nova Scotia", "511 Nova Scotia", [43.4, 47.1, -66.4, -59.6]),
    ("511.gnb.ca", "New Brunswick", "New Brunswick 511", [44.9, 48.2, -69.1, -63.7]),
    ("511nl.ca", "Newfoundland and Labrador", "511 NL", [46.5, 60.5, -67.9, -52.5]),
    ("511yukon.ca", "Yukon", "511 Yukon", [59.9, 69.7, -141.1, -123.7]),
];

/// The twelve Caltrans districts, roughly.
const CALTRANS: [[f64; 4]; 12] = [
    [39.5, 42.1, -124.5, -122.5],
    [40.0, 42.1, -123.5, -119.9],
    [38.2, 40.5, -122.5, -119.9],
    [36.9, 38.9, -123.1, -121.2],
    [34.4, 37.3, -122.2, -119.4],
    [35.0, 37.6, -120.9, -117.9],
    [33.6, 34.9, -119.5, -117.6],
    [33.4, 35.8, -117.9, -114.1],
    [35.5, 38.5, -119.3, -115.6],
    [37.0, 38.9, -121.6, -119.2],
    [32.5, 33.6, -117.6, -114.4],
    [33.3, 34.0, -118.2, -117.4],
];

fn sources() -> Vec<SourceDef> {
    let mut out = vec![
        SourceDef { key: "tfl".into(), url: "https://api.tfl.gov.uk/Place/Type/JamCam".into(), kind: Kind::Tfl, bbox: [51.2, 51.8, -0.6, 0.4] },
        SourceDef { key: "hongkong".into(), url: "https://static.data.gov.hk/td/traffic-snapshot-images/code/Traffic_Camera_Locations_En.xml".into(), kind: Kind::HongKong, bbox: [22.1, 22.6, 113.8, 114.5] },
        SourceDef { key: "nzta".into(), url: "https://www.journeys.nzta.govt.nz/assets/map-data-cache/cameras.json".into(), kind: Kind::Nzta, bbox: [-47.5, -34.0, 166.0, 179.0] },
        SourceDef { key: "fotowebcam".into(), url: "https://www.foto-webcam.eu/webcam/include/metadata.php".into(), kind: Kind::FotoWebcam, bbox: [44.0, 52.0, 5.0, 17.0] },
    ];
    for (i, bbox) in CALTRANS.iter().enumerate() {
        let d = i + 1;
        out.push(SourceDef { key: format!("caltrans{d}"), url: format!("https://cwwp2.dot.ca.gov/data/d{d}/cctv/cctvStatusD{d:02}.json"), kind: Kind::Caltrans(d as u8), bbox: *bbox });
    }
    for (host, region, label, bbox) in ROADS_511 {
        out.push(SourceDef { key: format!("511:{host}"), url: format!("https://{host}/map/mapIcons/Cameras"), kind: Kind::Roads511 { host, region, label }, bbox });
    }
    out
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x: &f64| x.is_finite())
}

fn valid(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && !(lat == 0.0 && lon == 0.0)
}

fn parse(kind: Kind, text: &str) -> Vec<Cam> {
    let json = || serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
    match kind {
        Kind::Tfl => parse_tfl(&json()),
        Kind::Caltrans(d) => parse_caltrans(&json(), d),
        Kind::HongKong => parse_hongkong(text),
        Kind::Nzta => parse_nzta(&json()),
        Kind::FotoWebcam => parse_fotowebcam(&json()),
        Kind::Roads511 { host, region, label } => parse_511(&json(), host, region, label),
    }
}

/// Transport for London's JamCams.
pub fn parse_tfl(v: &Value) -> Vec<Cam> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let props: HashMap<&str, &str> = c["additionalProperties"].as_array()?.iter().filter_map(|p| Some((p["key"].as_str()?, p["value"].as_str()?))).collect();
            if props.get("available") == Some(&"false") {
                return None;
            }
            let (lat, lon) = (num(&c["lat"])?, num(&c["lon"])?);
            let id = c["id"].as_str()?.trim_start_matches("JamCams_");
            let image = props.get("imageUrl")?;
            valid(lat, lon).then(|| Cam {
                id: format!("tfl:{id}"),
                name: c["commonName"].as_str().unwrap_or("JamCam").to_string(),
                area: "London".into(),
                lat,
                lon,
                image: image.to_string(),
                source: "TfL JamCams".into(),
                interval: 180,
            })
        })
        .collect()
}

/// Caltrans CCTV of one district.
pub fn parse_caltrans(v: &Value, district: u8) -> Vec<Cam> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let c = &row["cctv"];
            if c["inService"].as_str() == Some("false") {
                return None;
            }
            let loc = &c["location"];
            let (lat, lon) = (num(&loc["latitude"])?, num(&loc["longitude"])?);
            let image = c["imageData"]["static"]["currentImageURL"].as_str().filter(|u| u.starts_with("http"))?;
            let raw = loc["locationName"].as_str().unwrap_or("Caltrans camera");
            let name = raw.split_once(" -- ").map_or(raw, |(_, rest)| rest).trim();
            let place = loc["nearbyPlace"].as_str().filter(|s| !s.is_empty()).or(loc["county"].as_str()).unwrap_or("California");
            let minutes = num(&c["imageData"]["static"]["currentImageUpdateFrequency"]).unwrap_or(5.0);
            valid(lat, lon).then(|| Cam {
                id: format!("caltrans:{district}:{}", c["index"].as_str().unwrap_or(raw)),
                name: name.to_string(),
                area: format!("{place}, California"),
                lat,
                lon,
                image: image.to_string(),
                source: "Caltrans CCTV".into(),
                interval: (minutes * 60.0) as u32,
            })
        })
        .collect()
}

/// The Hong Kong Transport Department's camera list (XML).
pub fn parse_hongkong(xml: &str) -> Vec<Cam> {
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<image>(.*?)</image>").expect("image regex"));
    let tag = |block: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let start = block.find(&open)? + open.len();
        let end = block[start..].find(&format!("</{name}>"))? + start;
        Some(crate::image_viewer::html_unescape(block[start..end].trim()))
    };
    IMAGE
        .captures_iter(xml)
        .filter_map(|c| {
            let block = c.get(1)?.as_str();
            let lat: f64 = tag(block, "latitude")?.parse().ok()?;
            let lon: f64 = tag(block, "longitude")?.parse().ok()?;
            let description = tag(block, "description").unwrap_or_default();
            let name = description.rsplit_once(" [").map_or(description.as_str(), |(n, _)| n).to_string();
            valid(lat, lon).then(|| Cam {
                id: format!("hk:{}", tag(block, "key").unwrap_or_default()),
                name,
                area: format!("{}, Hong Kong", tag(block, "district").unwrap_or_else(|| "Hong Kong".into())),
                lat,
                lon,
                image: tag(block, "url").unwrap_or_default(),
                source: "Hong Kong Transport Department".into(),
                interval: 120,
            })
        })
        .filter(|c| c.image.starts_with("http"))
        .collect()
}

/// NZTA Waka Kotahi's traffic cameras (GeoJSON).
pub fn parse_nzta(v: &Value) -> Vec<Cam> {
    v["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let p = &f["properties"];
            if num(&p["Offline"]).unwrap_or(0.0) != 0.0 || num(&p["UnderMaintenance"]).unwrap_or(0.0) != 0.0 {
                return None;
            }
            let (lat, lon) = (num(&p["Latitude"])?, num(&p["Longitude"])?);
            let id = p["id"].as_i64().map(|i| i.to_string()).or_else(|| p["ExternalId"].as_i64().map(|i| i.to_string()))?;
            let image = p["ImageUrl"].as_str()?;
            valid(lat, lon).then(|| Cam {
                id: format!("nzta:{id}"),
                name: p["Name"].as_str().unwrap_or("Traffic camera").to_string(),
                area: "New Zealand".into(),
                lat,
                lon,
                image: image.to_string(),
                source: "NZTA Waka Kotahi".into(),
                interval: 120,
            })
        })
        .collect()
}

fn country(code: &str) -> &str {
    match code {
        "de" => "Germany",
        "at" => "Austria",
        "it" => "Italy",
        "ch" => "Switzerland",
        "si" => "Slovenia",
        "fr" => "France",
        "li" => "Liechtenstein",
        "cz" => "Czechia",
        "hr" => "Croatia",
        other => other,
    }
}

/// foto-webcam.eu: high-resolution weather cameras, a frame every few minutes.
pub fn parse_fotowebcam(v: &Value) -> Vec<Cam> {
    v["cams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            if c["offline"].as_bool() == Some(true) || c["hidden"].as_bool() == Some(true) {
                return None;
            }
            let (lat, lon) = (num(&c["latitude"])?, num(&c["longitude"])?);
            let thumb = c["imgurl"].as_str()?;
            let id = c["id"].as_str()?;
            let title = c["title"].as_str().or(c["name"].as_str()).unwrap_or(id);
            let code = c["country"].as_str().unwrap_or("").to_lowercase();
            valid(lat, lon).then(|| Cam {
                id: format!("fw:{id}"),
                name: title.to_string(),
                area: format!("{}, {}", c["name"].as_str().unwrap_or(id), country(&code)),
                lat,
                lon,
                image: thumb.replace("/current/400.jpg", "/current/1200.jpg"),
                source: "foto-webcam.eu".into(),
                interval: num(&c["captureInterval"]).unwrap_or(600.0) as u32,
            })
        })
        .collect()
}

/// A 511 traveller site's camera map: positions and ids (names come with the camera's card).
pub fn parse_511(v: &Value, host: &str, region: &str, label: &str) -> Vec<Cam> {
    // (A camera of the list far outside its own region is a typing slip: 511GA has one in Tibet.)
    let [s, n, w, e] = ROADS_511.iter().find(|r| r.0 == host).map_or([-90.0, 90.0, -180.0, 180.0], |r| r.3);
    let home = |lat: f64, lon: f64| (s - 1.0..=n + 1.0).contains(&lat) && (w - 1.0..=e + 1.0).contains(&lon);
    v["item2"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let id = c["itemId"].as_str().map(str::to_string).or_else(|| c["itemId"].as_i64().map(|i| i.to_string()))?;
            let loc = c["location"].as_array()?;
            let (lat, lon) = (num(loc.first()?)?, num(loc.get(1)?)?);
            (valid(lat, lon) && home(lat, lon)).then(|| Cam {
                id: format!("511:{host}:{id}"),
                name: format!("{label} camera {id}"),
                area: region.to_string(),
                lat,
                lon,
                image: format!("https://{host}/map/Cctv/{id}"),
                source: label.to_string(),
                interval: 60,
            })
        })
        .collect()
}

/// A 511 camera's own name, from its card ("CAM 1 Vernon I-84 WB Exit 64 …").
pub fn parse_511_card(html: &str) -> Option<String> {
    static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"data-fs-title="([^"]+)""#).expect("title regex"));
    TITLE.captures(html).map(|c| crate::image_viewer::html_unescape(c[1].trim())).filter(|t| !t.is_empty())
}

// --------------------------------------------------------------------------- the index ----

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredSource {
    fetched: i64,
    cams: Vec<Cam>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    sources: BTreeMap<String, StoredSource>,
}

static STORE: LazyLock<Mutex<Option<Stored>>> = LazyLock::new(Default::default);
static ALL: LazyLock<RwLock<Arc<Vec<Cam>>>> = LazyLock::new(Default::default);
static NAMES: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
static GATE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Every camera known right now (the Earth's layer draws them).
pub fn all_cams() -> Arc<Vec<Cam>> {
    ALL.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn rebuild(store: &Stored) {
    let mut all: Vec<Cam> = store.sources.values().flat_map(|s| s.cams.iter().cloned()).collect();
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all.dedup_by(|a, b| a.id == b.id);
    *ALL.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(all);
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The stand-in index of tests and dry runs (fictional ids, real places).
fn demo_store() -> Stored {
    let cam = |id: &str, name: &str, area: &str, lat: f64, lon: f64, source: &str| Cam { id: format!("demo:{id}"), name: name.into(), area: area.into(), lat, lon, image: format!("demo://cam/{id}"), source: source.into(), interval: 60 };
    let cams = vec![
        cam("lon1", "Tower Bridge Approach", "London", 51.5055, -0.0754, "TfL JamCams"),
        cam("lon2", "Trafalgar Square", "London", 51.508, -0.128, "TfL JamCams"),
        cam("lon3", "Marylebone Road", "London", 51.522, -0.155, "TfL JamCams"),
        cam("lon4", "Elephant and Castle", "London", 51.4946, -0.1003, "TfL JamCams"),
        cam("lon5", "Hyde Park Corner", "London", 51.5027, -0.1527, "TfL JamCams"),
        cam("ct1", "I-95 Greenwich", "Connecticut", 41.0262, -73.6282, "CTroads"),
        cam("ct2", "I-95 Stamford", "Connecticut", 41.0534, -73.5387, "CTroads"),
        cam("fw1", "Buchkopfturm", "Oppenau, Germany", 48.47, 8.2, "foto-webcam.eu"),
        cam("fw2", "Deutsches Museum", "München, Germany", 48.13, 11.58, "foto-webcam.eu"),
        cam("hk1", "Aberdeen Praya Road", "Southern, Hong Kong", 22.248, 114.15, "Hong Kong Transport Department"),
    ];
    let mut sources = BTreeMap::new();
    sources.insert("demo".to_string(), StoredSource { fetched: now(), cams });
    Stored { sources }
}

async fn load_disk() -> Stored {
    let raw = tokio::fs::read_to_string(INDEX_FILE).await.unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_default()
}

async fn save_disk(store: Stored) {
    let Ok(text) = serde_json::to_string(&store) else { return };
    if let Some(dir) = Path::new(INDEX_FILE).parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let tmp = format!("{INDEX_FILE}.tmp");
    if tokio::fs::write(&tmp, text).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, INDEX_FILE).await;
    }
}

/// The camera lists near a point (`None`: all of them) are loaded: from memory, from the disk
/// while they are younger than a day, else from their sources. Which lists failed.
async fn ensure_sources(near: Option<(f64, f64, f64)>) -> Vec<String> {
    if offline() {
        ensure_demo();
        return Vec::new();
    }
    let _gate = GATE.lock().await;
    let first = lock(&STORE).is_none();
    if first {
        let disk = load_disk().await;
        rebuild(&disk);
        *lock(&STORE) = Some(disk);
    }
    let wanted: Vec<SourceDef> = sources().into_iter().filter(|s| near.is_none_or(|(lat, lon, km)| s.covers(lat, lon, km))).collect();
    let stale: Vec<SourceDef> = {
        let store = lock(&STORE);
        let store = store.as_ref().expect("loaded above");
        wanted.into_iter().filter(|s| store.sources.get(&s.key).is_none_or(|got| now() - got.fetched > INDEX_TTL_SECS)).collect()
    };
    if stale.is_empty() {
        return Vec::new();
    }
    let started = std::time::Instant::now();
    let fetched = futures_util::future::join_all(stale.iter().map(|s| async move {
        let text = crate::image_viewer::get_text(&s.url, LIST_TIMEOUT).await;
        (s.key.clone(), s.kind, text)
    }))
    .await;
    let mut failed = Vec::new();
    let snapshot = {
        let mut guard = lock(&STORE);
        let store = guard.as_mut().expect("loaded above");
        for (key, kind, text) in fetched {
            match text.map(|t| parse(kind, &t)) {
                Ok(cams) if !cams.is_empty() => {
                    store.sources.insert(key, StoredSource { fetched: now(), cams });
                }
                Ok(_) => failed.push(format!("{key}: no cameras in its list")),
                Err(e) => failed.push(format!("{key}: {e}")),
            }
        }
        rebuild(store);
        store.clone()
    };
    info!("[CAMERAS] {} camera list(s) fetched in {} ms; {} cameras known{}", stale.len() - failed.len(), started.elapsed().as_millis(), all_cams().len(), if failed.is_empty() { String::new() } else { format!(" (failed: {})", failed.join("; ")) });
    save_disk(snapshot).await;
    failed
}

static LOADING_ALL: AtomicBool = AtomicBool::new(false);

/// The stand-in index, once (tests and dry runs).
pub(crate) fn ensure_demo() {
    let mut store = lock(&STORE);
    if store.is_none() {
        let demo = demo_store();
        rebuild(&demo);
        *store = Some(demo);
    }
}

/// Every list, in the background (the Earth's layer).
fn load_all_in_background() {
    if offline() {
        ensure_demo();
        return;
    }
    if LOADING_ALL.swap(true, Ordering::AcqRel) {
        return;
    }
    let task = async {
        let _ = ensure_sources(None).await;
        LOADING_ALL.store(false, Ordering::Release);
        crate::gui::refresh();
    };
    // (Also from the GUI thread, which has no runtime of its own: the one the HUD handed over.)
    match tokio::runtime::Handle::try_current().ok().or_else(|| RUNTIME.get().cloned()) {
        Some(rt) => {
            rt.spawn(task);
        }
        None => {
            LOADING_ALL.store(false, Ordering::Release);
            warn!("[CAMERAS] No runtime to load the camera lists on");
        }
    }
}

/// The `n` cameras nearest a point, with their distance in km.
pub fn nearest(lat: f64, lon: f64, n: usize) -> Vec<(Cam, f64)> {
    let all = all_cams();
    let mut by: Vec<(f64, usize)> = all.iter().enumerate().map(|(i, c)| (crate::earth::haversine_km((lat, lon), (c.lat, c.lon)), i)).collect();
    let n = n.min(by.len());
    if n == 0 {
        return Vec::new();
    }
    by.select_nth_unstable_by(n - 1, |a, b| a.0.total_cmp(&b.0));
    by.truncate(n);
    by.sort_by(|a, b| a.0.total_cmp(&b.0));
    by.into_iter().map(|(km, i)| (all[i].clone(), km)).collect()
}

/// A 511 camera's name is looked up once it is on screen.
pub fn ensure_name(cam: &Cam) {
    let Some(rest) = cam.id.strip_prefix("511:") else { return };
    if offline() || lock(&NAMES).contains_key(&cam.id) {
        return;
    }
    let Some((host, item)) = rest.rsplit_once(':') else { return };
    lock(&NAMES).insert(cam.id.clone(), cam.name.clone());
    let (id, url) = (cam.id.clone(), format!("https://{host}/tooltip/Cameras/{item}?lang=en"));
    let task = async move {
        if let Ok(html) = crate::image_viewer::get_text(&url, Duration::from_secs(10)).await {
            if let Some(name) = parse_511_card(&html) {
                lock(&NAMES).insert(id, name);
                bump();
            }
        }
    };
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(task);
    } else if let Some(rt) = RUNTIME.get() {
        rt.spawn(task);
    }
}

static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

/// The runtime the GUI's requests run on.
pub fn set_runtime(rt: &tokio::runtime::Handle) {
    let _ = RUNTIME.set(rt.clone());
}

// ---------------------------------------------------------------------------- the viewer ----

/// What the camera block shows.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// "London", "you".
    pub label: String,
    /// The cameras, nearest first, with their distance from the place in km.
    pub cams: Vec<(Cam, f64)>,
    /// 0: one camera large (`index`); 4 or 9: a wall.
    pub wall: usize,
    pub index: usize,
    pub version: u64,
    pub note: String,
    pub busy: bool,
}

impl View {
    pub fn current(&self) -> Option<&(Cam, f64)> {
        self.cams.get(self.index)
    }
}

static VIEW: LazyLock<Mutex<View>> = LazyLock::new(Default::default);
/// Tests that open the one camera view take turns (one opened London's second camera while
/// another drew Tower Bridge's).
#[cfg(test)]
pub(crate) fn view_turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    lock(&TURN)
}
static OPEN: AtomicBool = AtomicBool::new(false);
static VIEW_VERSION: AtomicU64 = AtomicU64::new(1);
static EARTH_LAYER: AtomicBool = AtomicBool::new(false);

pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

pub fn close() {
    OPEN.store(false, Ordering::Release);
    crate::gui::refresh();
}

pub fn view() -> View {
    lock(&VIEW).clone()
}

fn bump() {
    lock(&VIEW).version = VIEW_VERSION.fetch_add(1, Ordering::AcqRel);
    crate::gui::refresh();
}

fn update(f: impl FnOnce(&mut View)) {
    f(&mut lock(&VIEW));
    bump();
}

fn open_with(f: impl FnOnce(&mut View)) {
    update(f);
    OPEN.store(true, Ordering::Release);
    crate::hud_dock::cancel_close("webcams");
    crate::gui::show();
}

/// A tile of the wall, or a camera of the list beside the large one: that camera large.
pub fn select(index: usize) {
    update(|v| {
        if index < v.cams.len() {
            v.index = index;
            v.wall = 0;
        }
    });
}

/// Back from one camera to the wall it came from (when there was one).
pub fn show_wall(size: usize) {
    update(|v| v.wall = size);
}

/// A camera clicked on the Earth: it, large, with the cameras around it.
pub fn open_cam(cam: &Cam) {
    let around: Vec<(Cam, f64)> = nearest(cam.lat, cam.lon, 9).into_iter().filter(|(c, _)| c.id != cam.id).collect();
    let mut cams = vec![(cam.clone(), 0.0)];
    cams.extend(around);
    let label = cam.area.clone();
    open_with(|v| *v = View { label, cams, wall: 0, index: 0, version: 0, note: String::new(), busy: false });
}

pub fn earth_layer() -> bool {
    EARTH_LAYER.load(Ordering::Acquire)
}

/// The Earth's camera layer on or off (its key, "hide the cameras").
pub fn set_earth_layer(on: bool) {
    EARTH_LAYER.store(on, Ordering::Release);
    if on && all_cams().len() < 1000 {
        load_all_in_background();
    }
    crate::gui::refresh();
}

/// `open_jai_view {"view": "earth", "action": "show_cameras"}` (the Earth is open already).
pub async fn earth_layer_action(offline_run: bool) -> (String, String) {
    set_earth_layer(true);
    if offline_run {
        let _ = ensure_sources(None).await;
    }
    let n = all_cams().len();
    if n == 0 {
        ("The camera layer is on; the camera lists are loading.".into(), "The live cameras are coming onto the globe, Sir.".into())
    } else {
        (format!("The camera layer is on: {n} public cameras (more load in the background). A click on one opens its feed."), "The live cameras are on the globe, Sir. Click one to see its feed.".into())
    }
}

// ---------------------------------------------------------------------------- the tool ----

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "Live public webcams on the HUD: near a place or Sir, a wall (size 4/9), the Earth's layer.",
        "parameters": {"type": "object", "properties": {
            "place": {"type": "string"},
            "near_me": {"type": "boolean"},
            "size": {"type": "integer", "enum": [1, 4, 9]},
            "action": {"type": "string", "enum": ["show", "wall", "next", "earth_layer_on", "earth_layer_off"]}
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
        "next" => {
            let v = view();
            if !is_open() || v.cams.is_empty() {
                return Ok("No camera is open.\nSpoken: There's no camera up, Sir. Name a place and I'll find one.".into());
            }
            let i = if v.wall > 0 { 0 } else { (v.index + 1) % v.cams.len() };
            select(i);
            let (cam, km) = view().current().cloned().ok_or("No camera is open.")?;
            Ok(format!("Camera {} of {}: {} ({}, {}), {:.1} km from {}.\nSpoken: {}, Sir.", i + 1, v.cams.len(), cam.title(), cam.area, cam.source, km, v.label, cam.title()))
        }
        "earth_layer_on" | "earth" | "layer_on" => Box::pin(crate::earth::run(&json!({"action": "show_cameras"}))).await,
        "earth_layer_off" | "layer_off" => {
            set_earth_layer(false);
            Ok("The camera layer is off.\nSpoken: The cameras are off the globe, Sir.".into())
        }
        "close" | "hide" => {
            if is_open() && crate::hud_dock::close_named("live cameras").is_empty() {
                close();
            }
            Ok("The camera block is closed.\nSpoken: Cameras closed, Sir.".into())
        }
        _ => show(tools, args, action == "wall").await,
    }
}

/// Where a place is: the open-meteo geocoder (cities), then OpenStreetMap's (anything), then
/// the Earth's own list; the stand-in list offline.
pub(crate) async fn locate(tools: &ToolContext, place: &str) -> Result<(f64, f64, String), String> {
    let place = place.trim();
    let bare = place.strip_prefix("the ").unwrap_or(place);
    if offline() {
        return demo_place(place).ok_or_else(|| format!("'{place}' is not among the demo's places."));
    }
    if let Ok(v) = crate::image_viewer::get_json(&crate::earth::geocode_url(bare), Duration::from_secs(8)).await {
        if let Some(hit) = crate::earth::parse_geocode(&v) {
            return Ok((hit.lat, hit.lon, hit.name));
        }
    }
    if let Ok((lat, lon, name)) = tools.stark.location.geocode(place, None).await {
        return Ok((lat, lon, name.split(',').next().unwrap_or(&name).trim().to_string()));
    }
    crate::earth::gazetteer(bare).map(|h| (h.lat, h.lon, h.name)).ok_or_else(|| format!("I couldn't find '{place}' on the map, Sir."))
}

/// The places of tests and dry runs.
pub(crate) fn demo_place(place: &str) -> Option<(f64, f64, String)> {
    let key = place.trim().to_lowercase();
    let key = key.strip_prefix("the ").unwrap_or(&key);
    let extra: [(&str, f64, f64, &str); 6] = [
        ("stuttgart", 48.7823, 9.177, "Stuttgart"),
        ("eiffel tower", 48.8584, 2.2945, "Eiffel Tower"),
        ("brandenburg gate", 52.5163, 13.3777, "Brandenburg Gate"),
        ("times square", 40.758, -73.9855, "Times Square"),
        ("big ben", 51.5007, -0.1246, "Big Ben"),
        ("middle of the pacific", 0.0, -150.0, "the middle of the Pacific"),
    ];
    extra.iter().find(|(n, ..)| *n == key).map(|(_, lat, lon, name)| (*lat, *lon, name.to_string())).or_else(|| crate::earth::gazetteer(key).map(|h| (h.lat, h.lon, h.name)))
}

fn spoken_km(km: f64) -> String {
    crate::location_engine::spoken_distance(km * 1000.0)
}

/// "Anywhere", "somewhere", "random": no place at all, a camera at random.
fn anywhere(place: &str) -> bool {
    matches!(place.trim().to_lowercase().as_str(), "anywhere" | "somewhere" | "random" | "anywhere in the world" | "somewhere in the world" | "the world" | "irgendwo" | "zufällig")
}

/// Where a camera picked at random stands: a network first, each as likely as another (or the
/// biggest would nearly always win), then one of its cameras (Sir, 2026-11-26: "JAI only ever
/// shows the same locations").
async fn random_place() -> Result<(f64, f64, String), String> {
    ensure_sources(None).await;
    let all = all_cams();
    let mut networks: Vec<&str> = all.iter().map(|c| c.source.as_str()).collect();
    networks.sort_unstable();
    networks.dedup();
    let roll = |n: usize| (uuid::Uuid::new_v4().as_u128() % n.max(1) as u128) as usize;
    let network = *networks.get(roll(networks.len())).ok_or("I can't reach any of the public camera networks right now, Sir.")?;
    let cams: Vec<&Cam> = all.iter().filter(|c| c.source == network).collect();
    let cam = cams[roll(cams.len())];
    Ok((cam.lat, cam.lon, cam.area.clone()))
}

async fn show(tools: &ToolContext, args: &Value, wall_asked: bool) -> Result<String, String> {
    let place = str_arg(args, &["place", "city", "location", "query"]).map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
    let near_me = bool_arg(args, "near_me") == Some(true) || place.as_deref().is_some_and(|p| matches!(p.to_lowercase().as_str(), "me" | "here" | "my location" | "where i am" | "mir" | "hier"));
    let size = match int_arg(args, "size") {
        Some(n) if n >= 9 => 9,
        Some(n) if n >= 2 => 4,
        Some(_) => 1,
        None if wall_asked => 4,
        None => 1,
    };
    let (lat, lon, label) = if near_me {
        if offline() {
            // (Dry runs stand in Stuttgart, the demo's own city.)
            (48.7823, 9.177, "you".to_string())
        } else {
            let fix = tools.stark.location.current(tools).await?;
            (fix.lat, fix.lon, "you".to_string())
        }
    } else {
        let Some(place) = place else {
            return Err("Which place, Sir? Name a city (\"cameras in London\"), say \"the camera nearest to me\" or \"a random camera\".".into());
        };
        if anywhere(&place) {
            random_place().await?
        } else {
            locate(tools, &place).await?
        }
    };
    open_with(|v| *v = View { label: label.clone(), cams: Vec::new(), wall: if size > 1 { size } else { 0 }, index: 0, version: 0, note: format!("Looking for live cameras near {label}…"), busy: true });
    let mut failed = ensure_sources(Some((lat, lon, NEAR_SOURCES_KM))).await;
    let want = size.max(9);
    let mut found = nearest(lat, lon, want);
    // Nothing near: every list, for the nearest camera anywhere.
    if found.first().is_none_or(|(_, km)| *km > NEAR_SOURCES_KM) {
        failed.extend(ensure_sources(None).await);
        found = nearest(lat, lon, want);
    }
    let Some((first, km)) = found.first().cloned() else {
        update(|v| {
            v.busy = false;
            v.note = "No camera list could be loaded.".into();
        });
        return Ok(format!("No public camera lists could be loaded ({}).\nSpoken: I can't reach any of the public camera networks right now, Sir.", failed.join("; ")));
    };
    for (cam, _) in found.iter().take(size.max(1)) {
        pixels::want(&cam.key(), &cam.origin(), FRAME_SIDE, false);
        ensure_name(cam);
    }
    let n = found.len();
    update(|v| {
        v.cams = found.clone();
        v.index = 0;
        v.busy = false;
        v.note.clear();
    });
    let wall_n = size.min(n);
    crate::hud_dock::describe_block("webcams", &format!("LIVE CAMERAS // {label}: {}", found.iter().take(9).map(|(c, km)| format!("{} ({}, {km:.1} km)", c.title(), c.source)).collect::<Vec<_>>().join("; ")));
    let near = km <= IN_PLACE_KM;
    let display = format!(
        "{} for {label}: nearest {} ({}, {}) at {:.1} km; {} cameras listed, nearest first. Frames refresh every few seconds.",
        if size > 1 { format!("A {}×{} camera wall", if wall_n > 4 { 3 } else { 2 }, if wall_n > 4 { 3 } else { 2 }) } else { "A live camera".to_string() },
        first.title(),
        first.area,
        first.source,
        km,
        n
    );
    let spoken = if near_me {
        format!("The nearest public camera is {} away, Sir — {}.", spoken_km(km), first.title())
    } else if near && size > 1 {
        format!("{wall_n} live cameras from {label}, Sir.")
    } else if near {
        format!("Live from {label}, Sir — {}.", first.title())
    } else {
        format!("There's no public camera I can reach in {label}, Sir. The nearest is in {}, {} away; it's on the HUD.", first.area, spoken_km(km))
    };
    Ok(format!("{display}\nSpoken: {spoken}"))
}

// ------------------------------------------------------------------------- Sir's words ----

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid camera regex")
}

const CAM: &str = r"(?:live ?cams?|web ?cams?|cams?|cameras?|camera feeds?|live feeds?|live streams?|kameras?|livecams?|verkehrskameras?)";

static EARTH_ON: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:show|display|put|turn on|switch on|enable|add)(?: me)? (?:the |all )?(?:live |public )?{CAM}(?: layer)? (?:on|onto) (?:the )?(?:earth|globe|map|world map)$|^(?:zeig|zeige)(?: mir)? (?:die )?(?:webcams|kameras) auf (?:der |dem )?(?:erde|karte|globus)$")));
static EARTH_OFF: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:hide|remove|turn off|switch off|disable) (?:the )?(?:live |public )?{CAM}(?: layer)?(?: (?:on|from) (?:the )?(?:earth|globe|map))?$")));
static WALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:show|open|display|give|put up|bring up|build|make)(?: me)? )?(?:a |the )?(?:(?P<n>2 ?(?:x|by) ?2|3 ?(?:x|by) ?3|four|nine|4|9) )?(?:camera|webcam|cam|video) ?wall (?:of|in|for|from) (?P<place>.+)$|^(?:(?:show|open|display)(?: me)? )?(?:a )?wall of (?:live )?(?:cameras|webcams|cams) (?:of|in|for|from) (?P<place2>.+)$|^(?:zeig|zeige|öffne)(?: mir)? (?:eine |die )?(?:kamerawand|webcamwand|kamera wand|webcam wand)(?: von| in| aus| für)? (?P<place3>.+)$")
});
static NEAR_ME: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:(?:show|open|find|give)(?: me)? )?(?:the |a )?(?:(?:nearest|closest) (?:live |public )?{CAM}(?: to me| near me)?|(?:live |public )?{CAM} (?:nearest|closest|near) (?:to )?me|(?:live |public )?{CAM} (?:around|near) here)$|^(?:zeig|zeige|finde)(?: mir)? (?:die |eine )?(?:webcams?|kameras?) in meiner n(?:ä|ae)he$")
    )
});
/// "Show me a random live camera", "a camera from anywhere in the world", "zeig mir eine
/// zufällige Kamera": one picked at random (`random_place`).
static ANYWHERE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:(?:show|open|give|find|pick)(?: me)? )?(?:a |an |some )?(?:random|any) (?:live |public )?{CAM}(?: (?:from )?(?:anywhere|somewhere)(?: in the world| on earth)?)?$|^(?:(?:show|open|give|find)(?: me)? )?(?:a |an )?(?:live |public )?{CAM} (?:from )?(?:anywhere|somewhere)(?: in the world| on earth)?$|^(?:zeig|zeige)(?: mir)? (?:eine )?(?:zufällige|zufaellige|irgendeine) (?:live ?)?(?:kamera|webcam)$"))
});
static NEXT_DE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:zeig|zeige)(?: mir)? die n(?:ä|ae)chste (?:webcam|kamera)$"));
static NEXT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show (?:me )?)?(?:the )?(?:next|another|other) (?:live )?(?:camera|webcam|cam)$|^n(?:ä|ae)chste (?:kamera|webcam)$"));
static PLACE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:(?:show|open|display|find|give|get|bring up|pull up|put up)(?: me| us)? )?(?:a |an |some |the |all |any )?(?:(?:live|public|traffic|street|weather|city|real time|real-time) )*(?P<noun>{CAM}) (?:in|from|of|at|near|around|over|for|covering) (?P<place>.+)$|^(?:zeig|zeige|öffne|oeffne|such|suche|finde)(?: mir| uns)? (?:eine |einen |die |ein paar |einige )?(?:live[- ]?)?(?P<noun2>webcams?|kameras?|livecams?|verkehrskameras?) (?:in|aus|von|bei|am|vom) (?P<place2>.+)$"
    ))
});

/// `show_live_camera` arguments for one of Sir's sentences (already normalised).
pub fn phrase(s: &str) -> Option<Value> {
    if EARTH_ON.is_match(s) {
        return Some(json!({"action": "earth_layer_on"}));
    }
    if EARTH_OFF.is_match(s) && earth_layer() {
        return Some(json!({"action": "earth_layer_off"}));
    }
    if let Some(c) = WALL.captures(s) {
        let place = c.name("place").or_else(|| c.name("place2")).or_else(|| c.name("place3")).map(|m| tidy_place(m.as_str()))?;
        let size = match c.name("n").map(|m| m.as_str().replace(' ', "")) {
            Some(n) if n.starts_with('3') || n == "nine" || n == "9" => 9,
            _ => 4,
        };
        return (!place.is_empty()).then(|| json!({"action": "wall", "place": place, "size": size}));
    }
    if NEAR_ME.is_match(s) {
        return Some(json!({"near_me": true}));
    }
    if NEXT_DE.is_match(s) {
        return Some(if is_open() { json!({"action": "next"}) } else { json!({"near_me": true}) });
    }
    if NEXT.is_match(s) {
        return is_open().then(|| json!({"action": "next"}));
    }
    if ANYWHERE.is_match(s) {
        return Some(json!({"place": "anywhere"}));
    }
    let c = PLACE.captures(s)?;
    let noun = c.name("noun").or_else(|| c.name("noun2"))?.as_str();
    let place = tidy_place(c.name("place").or_else(|| c.name("place2"))?.as_str());
    // (Sir's own camera and screen are not a place: "show the camera on my second monitor".)
    if place.is_empty() || place.starts_with("my ") || place.starts_with("meine") || place.contains("monitor") || place.contains("screen") || place.contains("bildschirm") {
        return None;
    }
    if matches!(place.as_str(), "me" | "here" | "mir" | "hier") {
        return Some(json!({"near_me": true}));
    }
    let plural = noun.ends_with('s') && !noun.ends_with("ss") || noun == "kameras" || noun == "webcams";
    Some(if plural { json!({"action": "wall", "place": place, "size": 4}) } else { json!({"place": place}) })
}

fn tidy_place(raw: &str) -> String {
    let mut p = raw.trim().to_string();
    for tail in [" right now", " now", " live", " please", " jetzt", " gerade", " on the hud", " on my screen"] {
        if let Some(rest) = p.strip_suffix(tail) {
            p = rest.trim().to_string();
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intents::normalize_phrase;

    /// Normalised as the fast path does, then the phrase.
    fn said(text: &str) -> Option<Value> {
        phrase(&normalize_phrase(text))
    }

    #[test]
    fn sirs_camera_requests_route() {
        assert_eq!(said("show me cameras in London"), Some(json!({"action": "wall", "place": "london", "size": 4})));
        assert_eq!(said("Show me a live camera from New York"), Some(json!({"place": "new york"})));
        assert_eq!(said("Zeig mir eine Webcam in Stuttgart"), Some(json!({"place": "stuttgart"})));
        assert_eq!(said("show me the camera nearest to me"), Some(json!({"near_me": true})));
        assert_eq!(said("show me a camera wall of London"), Some(json!({"action": "wall", "place": "london", "size": 4})));
        assert_eq!(said("show me a 3 by 3 camera wall of Hong Kong"), Some(json!({"action": "wall", "place": "hong kong", "size": 9})));
        assert_eq!(said("Zeig mir eine Kamerawand von Berlin"), Some(json!({"action": "wall", "place": "berlin", "size": 4})));
        assert_eq!(said("show the cameras on the earth"), Some(json!({"action": "earth_layer_on"})));
        assert_eq!(said("webcams in meiner Nähe"), None, "needs a verb in German");
        assert_eq!(said("zeig mir webcams in meiner Nähe"), Some(json!({"near_me": true})));
        for text in ["show my camera", "turn on the camera", "show the camera on my second monitor", "open the camera app", "show me a picture of a camera"] {
            assert_eq!(said(text), None, "{text}");
        }
        // Sir, 2026-11-26: "JAI only ever shows the same locations": one at random.
        for text in ["Show me a random live camera", "show me a camera from anywhere in the world", "Show me any webcam", "Zeig mir eine zufällige Kamera"] {
            assert_eq!(said(text), Some(json!({"place": "anywhere"})), "{text}");
        }
        assert!(anywhere("Anywhere") && anywhere("somewhere in the world") && !anywhere("London"));
    }

    #[test]
    fn source_formats_parse() {
        let tfl = json!([{"id": "JamCams_00002.00865", "commonName": "A406 Billet Upass E", "lat": 51.60067, "lon": -0.01594, "additionalProperties": [{"key": "available", "value": "true"}, {"key": "imageUrl", "value": "https://s3-eu-west-1.amazonaws.com/jamcams.tfl.gov.uk/00002.00865.jpg"}]},
                         {"id": "JamCams_1", "commonName": "Off", "lat": 51.5, "lon": -0.1, "additionalProperties": [{"key": "available", "value": "false"}, {"key": "imageUrl", "value": "x"}]}]);
        let cams = parse_tfl(&tfl);
        assert_eq!(cams.len(), 1);
        assert_eq!((cams[0].id.as_str(), cams[0].area.as_str()), ("tfl:00002.00865", "London"));

        let ct = json!({"data": [{"cctv": {"index": "1", "inService": "true", "location": {"locationName": "TV102 -- I-580 : West of SR-24", "nearbyPlace": "Oakland", "latitude": "37.82539", "longitude": "-122.27291"}, "imageData": {"static": {"currentImageUpdateFrequency": "5", "currentImageURL": "https://cwwp2.dot.ca.gov/data/d4/cctv/image/tv102/tv102.jpg"}}}}]});
        let cams = parse_caltrans(&ct, 4);
        assert_eq!((cams[0].name.as_str(), cams[0].area.as_str(), cams[0].interval), ("I-580 : West of SR-24", "Oakland, California", 300));

        let hk = "<image-list><image><key>H429F</key><region>Hong Kong Island</region><district>Southern</district><description>Aberdeen Praya Road near Fish Market [H429F]</description><latitude>22.24845</latitude><longitude>114.1505</longitude><url>https://tdcctv.data.one.gov.hk/H429F.JPG</url></image></image-list>";
        let cams = parse_hongkong(hk);
        assert_eq!((cams[0].name.as_str(), cams[0].area.as_str(), cams[0].id.as_str()), ("Aberdeen Praya Road near Fish Market", "Southern, Hong Kong", "hk:H429F"));

        let nz = json!({"features": [{"properties": {"Name": "SH1 Groynes", "Offline": 0, "UnderMaintenance": 0, "ImageUrl": "https://www.trafficnz.info/camera/706.jpg", "Latitude": "-43.449229", "Longitude": "172.61162", "id": 1430}},
                                     {"properties": {"Name": "Down", "Offline": 1, "ImageUrl": "x", "Latitude": "-43", "Longitude": "172", "id": 1}}]});
        assert_eq!(parse_nzta(&nz).len(), 1);

        let fw = json!({"cams": [{"id": "bardolino", "name": "Bardolino", "title": "Color Hotel - Bardolino", "offline": false, "imgurl": "https://www.foto-webcam.eu/webcam/bardolino/current/400.jpg", "country": "it", "latitude": 45.54, "longitude": 10.72, "captureInterval": 1800},
                                {"id": "gone", "offline": true, "imgurl": "x", "latitude": 45.0, "longitude": 10.0}]});
        let cams = parse_fotowebcam(&fw);
        assert_eq!(cams.len(), 1);
        assert_eq!((cams[0].image.as_str(), cams[0].area.as_str()), ("https://www.foto-webcam.eu/webcam/bardolino/current/1200.jpg", "Bardolino, Italy"));
        assert_eq!(cams[0].refresh_secs(false), 60);

        let ct511 = json!({"item1": {}, "item2": [{"itemId": "308", "location": [41.823175, -72.501513]}]});
        let cams = parse_511(&ct511, "ctroads.org", "Connecticut", "CTroads");
        assert_eq!((cams[0].image.as_str(), cams[0].id.as_str()), ("https://ctroads.org/map/Cctv/308", "511:ctroads.org:308"));
        // (511GA's own list as it was on 2026-11-26: one camera with its longitude's sign lost.)
        let ga = json!({"item2": [{"itemId": "11141", "location": [34.37697, -84.91445]}, {"itemId": "9", "location": [33.7, 84.3]}]});
        assert_eq!(parse_511(&ga, "511ga.org", "Georgia", "511GA").iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["511:511ga.org:11141"]);
        let card = r#"<img class="carouselCctvImage" data-lazy="/map/Cctv/308" data-fs-title="CAM 1 Vernon I-84 WB Exit 64 - Rt. 30 &amp; 83" />"#;
        assert_eq!(parse_511_card(card).as_deref(), Some("CAM 1 Vernon I-84 WB Exit 64 - Rt. 30 & 83"));
    }

    #[test]
    fn sources_are_chosen_by_distance() {
        let all = sources();
        let near = |lat: f64, lon: f64| all.iter().filter(|s| s.covers(lat, lon, NEAR_SOURCES_KM)).map(|s| s.key.clone()).collect::<Vec<_>>();
        assert_eq!(near(51.5072, -0.1276), vec!["tfl".to_string()], "London needs only TfL");
        let ny = near(40.7128, -74.006);
        assert!(ny.contains(&"511:ctroads.org".to_string()) && ny.contains(&"511:www.511pa.com".to_string()), "{ny:?}");
        assert!(near(48.78, 9.18).contains(&"fotowebcam".to_string()));
        assert!(near(0.0, -150.0).is_empty());
    }

    #[test]
    fn the_viewer_runs_offline_on_stand_ins() {
        let _turn = view_turn();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let _ = ensure_sources(None).await;
        });
        let london = nearest(51.5072, -0.1276, 4);
        assert_eq!(london.len(), 4);
        assert!(london.iter().all(|(c, km)| c.area == "London" && *km < 10.0));
        let stuttgart = nearest(48.7823, 9.177, 1);
        assert_eq!(stuttgart[0].0.name, "Buchkopfturm");
        assert!(stuttgart[0].1 > IN_PLACE_KM, "Stuttgart has no camera of its own in the demo");
        open_cam(&london[1].0);
        assert!(is_open());
        assert_eq!(view().cams[0].0.id, london[1].0.id);
        select(2);
        assert_eq!(view().index, 2);
        close();
    }
}
