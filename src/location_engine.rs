//! Location awareness: where Sir is, what the weather is doing exactly there, and what is around.
//!
//! * Position, best first: the live GPS fix his phone streams from the mobile satellite
//!   (`navigator.geolocation.watchPosition`, fresh for 15 minutes), the saved home coordinates,
//!   then IP geolocation (ip-api.com) — so a location question always has an answer.
//! * Hyper-local weather from Open-Meteo (no API key) for those exact coordinates: current
//!   conditions plus an hourly forecast (rain probability, precipitation, wind and gusts).
//! * Reverse geocoding (Nominatim), nearby places (Overpass API) and distance / driving time to a
//!   destination (Nominatim + the OSRM router).
//! * The home geofence: when the phone moves from more than 500 m away to within 150 m of home,
//!   the arrival protocol runs on the PC (wake the HUD, a playlist, a greeting).

use crate::config::LocationSettings;
use crate::context_graph::{ContextGraph, GeoFix};
use crate::stark::StarkEvent;
use crate::tools::{str_arg, ToolContext};
use chrono::{Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, Timelike};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::info;

/// A phone fix older than this no longer counts as "where Sir is".
const PHONE_FIX_TTL: Duration = Duration::from_secs(15 * 60);
const IP_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const USER_AGENT: &str = concat!("JARVIS-desktop/", env!("CARGO_PKG_VERSION"), " (personal assistant; local use)");

/// One sample from the phone's browser.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct PhoneSample {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub accuracy: Option<f64>,
    #[serde(default)]
    pub altitude: Option<f64>,
    /// Metres per second, as the Geolocation API reports it.
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub heading: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fence {
    Unknown,
    Home,
    Away,
}

struct Inner {
    phone: RwLock<Option<GeoFix>>,
    ip: Mutex<Option<(GeoFix, Instant)>>,
    addresses: Mutex<HashMap<(i64, i64), String>>,
    fence: Mutex<Fence>,
    http: reqwest::Client,
}

#[derive(Clone)]
pub struct LocationEngine {
    inner: Arc<Inner>,
}

impl Default for LocationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Great-circle distance in metres.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

fn bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> &'static str {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    let deg = (y.atan2(x).to_degrees() + 360.0) % 360.0;
    ["north", "north-east", "east", "south-east", "south", "south-west", "west", "north-west"][((deg + 22.5) / 45.0) as usize % 8]
}

pub fn spoken_distance(m: f64) -> String {
    if m < 950.0 {
        format!("{} m", ((m / 10.0).round() * 10.0) as i64)
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

/// WMO weather interpretation codes (Open-Meteo `weather_code`).
pub fn weather_code_text(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 | 77 => "snow",
        80 => "light showers",
        81 => "showers",
        82 => "violent showers",
        85 | 86 => "snow showers",
        95 => "thunderstorms",
        96 | 99 => "thunderstorms with hail",
        _ => "unsettled weather",
    }
}

#[derive(Debug, Clone, Default)]
pub struct CurrentWeather {
    pub temperature: f64,
    pub apparent: f64,
    pub precipitation: f64,
    pub rain: f64,
    pub code: i64,
    pub wind_kmh: f64,
    pub gusts_kmh: f64,
}

#[derive(Debug, Clone)]
pub struct HourWeather {
    pub time: NaiveDateTime,
    pub temperature: f64,
    pub precip_probability: f64,
    pub precipitation: f64,
    pub wind_kmh: f64,
    pub gusts_kmh: f64,
    pub code: i64,
}

/// What `get_precise_location_weather` and the commute advisor work from.
#[derive(Debug, Clone)]
pub struct WeatherReport {
    pub fix: GeoFix,
    pub current: CurrentWeather,
    /// The whole report for the LLM.
    pub text: String,
    /// One or two sentences (briefings, teleports, the satellite).
    pub short: String,
}

impl LocationEngine {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(6))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: Arc::new(Inner { phone: RwLock::new(None), ip: Mutex::new(None), addresses: Mutex::new(HashMap::new()), fence: Mutex::new(Fence::Unknown), http }),
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    /// The phone's latest fix (while fresh).
    pub fn phone_fix(&self) -> Option<GeoFix> {
        self.inner.phone.read().unwrap_or_else(|e| e.into_inner()).clone().filter(|f| f.at.elapsed() < PHONE_FIX_TTL)
    }

    /// A GPS sample from the satellite: updates the working memory and the home geofence.
    pub fn update_from_phone(&self, s: PhoneSample, cfg: &LocationSettings, graph: &ContextGraph, events: &broadcast::Sender<StarkEvent>) {
        if !(-90.0..=90.0).contains(&s.latitude) || !(-180.0..=180.0).contains(&s.longitude) {
            return;
        }
        let address = self.cached_address(s.latitude, s.longitude);
        let fix = GeoFix {
            lat: s.latitude,
            lon: s.longitude,
            accuracy_m: s.accuracy.unwrap_or(50.0),
            speed_kmh: s.speed.filter(|v| v.is_finite() && *v >= 0.0).map(|v| v * 3.6),
            altitude_m: s.altitude.filter(|v| v.is_finite()),
            heading: s.heading.filter(|v| v.is_finite()),
            address,
            source: "phone GPS".into(),
            at: Instant::now(),
        };
        let first = self.inner.phone.read().unwrap_or_else(|e| e.into_inner()).is_none();
        *self.inner.phone.write().unwrap_or_else(|e| e.into_inner()) = Some(fix.clone());
        graph.set_location(fix.clone());
        if first {
            info!("[GPS] Phone GPS lock: {:.5}, {:.5} ±{:.0} m", fix.lat, fix.lon, fix.accuracy_m);
            let _ = events.send(StarkEvent::Pulse { path: vec!["hivemind".into(), "gps".into(), "context".into()], alert: false });
        }
        // Resolve the street in the background (cached per ~100 m cell).
        if fix.address.is_none() {
            let (me, lat, lon) = (self.clone(), fix.lat, fix.lon);
            let graph = graph.clone();
            tokio::spawn(async move {
                if let Some(a) = me.reverse_geocode(lat, lon).await {
                    let updated = me.inner.phone.write().unwrap_or_else(|e| e.into_inner()).as_mut().map(|f| {
                        f.address = Some(a.clone());
                        f.clone()
                    });
                    if let Some(f) = updated {
                        graph.set_location(f);
                    }
                }
            });
        }
        // The home geofence (hysteresis: leave beyond `geofence_leave_m`, arrive within `geofence_arrive_m`).
        if let (Some(hl), Some(ho), true) = (cfg.home_lat, cfg.home_lon, cfg.geofence_enabled) {
            let d = haversine_m(fix.lat, fix.lon, hl, ho);
            let mut fence = self.inner.fence.lock().unwrap_or_else(|e| e.into_inner());
            let next = match *fence {
                Fence::Away if d < cfg.geofence_arrive_m => {
                    info!("[GEOFENCE] Arrived home ({d:.0} m)");
                    let _ = events.send(StarkEvent::Arrived { distance_m: d });
                    Fence::Home
                }
                Fence::Unknown if d < cfg.geofence_arrive_m => Fence::Home,
                _ if d > cfg.geofence_leave_m => Fence::Away,
                same => same,
            };
            if next != *fence && next == Fence::Away {
                info!("[GEOFENCE] Left home ({d:.0} m away)");
            }
            *fence = next;
        }
    }

    fn cached_address(&self, lat: f64, lon: f64) -> Option<String> {
        let key = ((lat * 1000.0).round() as i64, (lon * 1000.0).round() as i64);
        self.inner.addresses.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
    }

    /// "Schillerstraße 12, Musterstadt" for a coordinate (Nominatim, cached).
    pub async fn reverse_geocode(&self, lat: f64, lon: f64) -> Option<String> {
        if let Some(a) = self.cached_address(lat, lon) {
            return Some(a);
        }
        let url = format!("https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat={lat:.6}&lon={lon:.6}&zoom=18&addressdetails=1");
        let v: Value = self.inner.http.get(url).send().await.ok()?.json().await.ok()?;
        let a = &v["address"];
        let road = a["road"].as_str().or(a["pedestrian"].as_str()).or(a["footway"].as_str()).unwrap_or("");
        let number = a["house_number"].as_str().unwrap_or("");
        let town = a["city"].as_str().or(a["town"].as_str()).or(a["village"].as_str()).or(a["suburb"].as_str()).unwrap_or("");
        let street = format!("{road} {number}").trim().to_string();
        let text = match (street.is_empty(), town.is_empty()) {
            (false, false) => format!("{street}, {town}"),
            (false, true) => street,
            (true, false) => town.to_string(),
            _ => v["display_name"].as_str()?.split(',').take(3).collect::<Vec<_>>().join(","),
        };
        let key = ((lat * 1000.0).round() as i64, (lon * 1000.0).round() as i64);
        self.inner.addresses.lock().unwrap_or_else(|e| e.into_inner()).insert(key, text.clone());
        Some(text)
    }

    /// A place name or address → coordinates (biased towards Sir's position).
    pub async fn geocode(&self, query: &str, near: Option<(f64, f64)>) -> Result<(f64, f64, String), String> {
        let mut url = reqwest::Url::parse("https://nominatim.openstreetmap.org/search").map_err(|e| e.to_string())?;
        {
            let mut q = url.query_pairs_mut();
            // English names (he speaks English), and Sir's words without their article.
            q.append_pair("format", "jsonv2").append_pair("limit", "1").append_pair("accept-language", "en").append_pair("q", without_article(query));
            if let Some((lat, lon)) = near {
                q.append_pair("viewbox", &format!("{},{},{},{}", lon - 0.6, lat + 0.4, lon + 0.6, lat - 0.4));
            }
        }
        let v: Value = self.inner.http.get(url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
        let hit = v.as_array().and_then(|a| a.first()).ok_or_else(|| format!("I couldn't find '{query}' on the map."))?;
        let lat: f64 = hit["lat"].as_str().and_then(|s| s.parse().ok()).ok_or("Nominatim returned no latitude")?;
        let lon: f64 = hit["lon"].as_str().and_then(|s| s.parse().ok()).ok_or("Nominatim returned no longitude")?;
        let name = hit["display_name"].as_str().unwrap_or(query).split(',').take(3).collect::<Vec<_>>().join(",");
        Ok((lat, lon, name))
    }

    /// The PC's approximate position from its public IP (cached for 30 minutes).
    pub async fn ip_location(&self) -> Result<GeoFix, String> {
        if let Some((fix, at)) = self.inner.ip.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if at.elapsed() < IP_CACHE_TTL {
                return Ok(fix);
            }
        }
        let v: Value = self
            .inner
            .http
            .get("http://ip-api.com/json/?fields=status,message,lat,lon,city,regionName,country,query")
            .send()
            .await
            .map_err(|e| format!("IP geolocation failed: {e}"))?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if v["status"] != "success" {
            return Err(format!("IP geolocation failed: {}", v["message"].as_str().unwrap_or("unknown error")));
        }
        let fix = GeoFix {
            lat: v["lat"].as_f64().ok_or("no latitude")?,
            lon: v["lon"].as_f64().ok_or("no longitude")?,
            accuracy_m: 5000.0,
            speed_kmh: None,
            altitude_m: None,
            heading: None,
            address: Some(format!("{}, {}", v["city"].as_str().unwrap_or("?"), v["country"].as_str().unwrap_or("?"))),
            source: "IP geolocation".into(),
            at: Instant::now(),
        };
        *self.inner.ip.lock().unwrap_or_else(|e| e.into_inner()) = Some((fix.clone(), Instant::now()));
        Ok(fix)
    }

    /// Where Sir is right now: phone GPS → home → IP.
    pub async fn current(&self, tools: &ToolContext) -> Result<GeoFix, String> {
        if let Some(f) = self.phone_fix() {
            return Ok(f);
        }
        let cfg = tools.config.read(|c| c.location.clone());
        if let (Some(lat), Some(lon)) = (cfg.home_lat, cfg.home_lon) {
            return Ok(GeoFix {
                lat,
                lon,
                accuracy_m: 30.0,
                speed_kmh: None,
                altitude_m: None,
                heading: None,
                address: Some(if cfg.home_label.is_empty() { "home".into() } else { cfg.home_label.clone() }),
                source: "home (no phone GPS)".into(),
                at: Instant::now(),
            });
        }
        if cfg.ip_fallback {
            let fix = self.ip_location().await?;
            tools.stark.graph.set_location(fix.clone());
            return Ok(fix);
        }
        Err("I don't know where you are, Sir: open the satellite on your phone, or tell me your home address (\"set my home to …\").".into())
    }
}

/// "the Eiffel Tower" → "Eiffel Tower": with the article, OpenStreetMap's first hit is the
/// replica in Nikko, Japan. A one-word rest keeps it ("the Hague", "the Netherlands").
fn without_article(query: &str) -> &str {
    let q = query.trim();
    match q.get(..4) {
        Some(head) if head.eq_ignore_ascii_case("the ") && q[4..].trim().split_whitespace().count() >= 2 => q[4..].trim(),
        _ => q,
    }
}

// ------------------------------------------------------------------------- weather ----

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// Open-Meteo for exact coordinates: current conditions and the hourly forecast.
pub async fn fetch_weather(http: &reqwest::Client, lat: f64, lon: f64) -> Result<(CurrentWeather, Vec<HourWeather>), String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.5}&longitude={lon:.5}&current=temperature_2m,apparent_temperature,precipitation,rain,weather_code,wind_speed_10m,wind_gusts_10m&hourly=temperature_2m,precipitation_probability,precipitation,wind_speed_10m,wind_gusts_10m,weather_code&timezone=auto&forecast_days=3"
    );
    let resp = http.get(url).send().await.map_err(|e| format!("Open-Meteo is not reachable ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("Open-Meteo answered HTTP {}", resp.status()));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let c = &v["current"];
    let current = CurrentWeather {
        temperature: f(&c["temperature_2m"]),
        apparent: f(&c["apparent_temperature"]),
        precipitation: f(&c["precipitation"]),
        rain: f(&c["rain"]),
        code: c["weather_code"].as_i64().unwrap_or(-1),
        wind_kmh: f(&c["wind_speed_10m"]),
        gusts_kmh: f(&c["wind_gusts_10m"]),
    };
    let h = &v["hourly"];
    let times = h["time"].as_array().cloned().unwrap_or_default();
    let at = |key: &str, i: usize| h[key].get(i).map(f).unwrap_or(0.0);
    let hourly = times
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let time = NaiveDateTime::parse_from_str(t.as_str()?, "%Y-%m-%dT%H:%M").ok()?;
            Some(HourWeather {
                time,
                temperature: at("temperature_2m", i),
                precip_probability: at("precipitation_probability", i),
                precipitation: at("precipitation", i),
                wind_kmh: at("wind_speed_10m", i),
                gusts_kmh: at("wind_gusts_10m", i),
                code: h["weather_code"].get(i).and_then(Value::as_i64).unwrap_or(-1),
            })
        })
        .collect();
    Ok((current, hourly))
}

/// The weather at Sir's position: the next `hours` hours, or a whole `day`.
pub async fn weather_report(tools: &ToolContext, hours: usize, day: Option<NaiveDate>) -> Result<WeatherReport, String> {
    let engine = &tools.stark.location;
    let fix = engine.current(tools).await?;
    tools.stark.pulse(&["brain", "gps"], false);
    let (current, hourly) = fetch_weather(engine.http(), fix.lat, fix.lon).await?;
    let now = Local::now().naive_local();
    let today = now.date();
    let window: Vec<&HourWeather> = match day.filter(|d| *d != today) {
        Some(d) => hourly.iter().filter(|h| h.time.date() == d && (6..=22).contains(&h.time.hour())).collect(),
        None => hourly.iter().filter(|h| h.time + ChronoDuration::minutes(59) >= now).take(hours.clamp(1, 48)).collect(),
    };
    let place = fix.address.clone().unwrap_or_else(|| format!("{:.4}, {:.4}", fix.lat, fix.lon));
    let mut text = format!(
        "Weather at Sir's exact position ({place}; {} ±{:.0} m) from Open-Meteo.\nNow: {:.0}°C (feels like {:.0}°C), {}, wind {:.0} km/h, gusts {:.0} km/h, precipitation {:.1} mm (rain {:.1} mm).",
        fix.source, fix.accuracy_m, current.temperature, current.apparent, weather_code_text(current.code), current.wind_kmh, current.gusts_kmh, current.precipitation, current.rain
    );
    if let Some(d) = day.filter(|d| *d != today) {
        text.push_str(&format!("\nForecast for {}:", d.format("%A %-d %B")));
    } else {
        text.push_str(&format!("\nNext {} hours:", window.len()));
    }
    for h in window.iter().step_by(if window.len() > 12 { 2 } else { 1 }) {
        text.push_str(&format!(
            "\n{}: {:.0}°C, {}, rain {:.0}% ({:.1} mm), wind {:.0} km/h, gusts {:.0}",
            h.time.format("%H:%M"),
            h.temperature,
            weather_code_text(h.code),
            h.precip_probability,
            h.precipitation,
            h.wind_kmh,
            h.gusts_kmh
        ));
    }
    let max_rain = window.iter().map(|h| h.precip_probability).fold(0.0, f64::max);
    let first_rain = window.iter().find(|h| h.precip_probability >= 50.0);
    let (lo, hi) = window.iter().fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.temperature), hi.max(h.temperature)));
    let short = match day.filter(|d| *d != today) {
        Some(d) => format!(
            "{}: {:.0} to {:.0} degrees, up to {:.0} percent chance of rain{}.",
            d.format("%A"),
            lo,
            hi,
            max_rain,
            first_rain.map(|h| format!(" (likely from about {})", h.time.format("%H:%M"))).unwrap_or_default()
        ),
        None => format!(
            "It's {:.0} degrees and {} where you are{}{}.",
            current.temperature,
            weather_code_text(current.code),
            if max_rain >= 30.0 { format!("; up to {:.0} percent chance of rain in the next hours", max_rain) } else { "; it should stay dry".into() },
            first_rain.map(|h| format!(", most likely from about {}", h.time.format("%H:%M"))).unwrap_or_default()
        ),
    };
    text.push_str(&format!("\nSummary: {short}"));
    Ok(WeatherReport { fix, current, text, short })
}

// --------------------------------------------------------------- nearby & routes ----

/// Spoken words → OpenStreetMap tags.
fn poi_filter(query: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let q = query.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| q.contains(w));
    Some(if has(&["gas station", "petrol", "fuel", "tankstelle", "tanken"]) {
        ("amenity", "fuel", "petrol stations")
    } else if has(&["charging", "ladesäule", "ladestation", "e-charging"]) {
        ("amenity", "charging_station", "charging stations")
    } else if has(&["restaurant", "essen gehen", "dinner"]) {
        ("amenity", "restaurant", "restaurants")
    } else if has(&["fast food", "burger", "pizza", "döner", "doener", "kebab", "imbiss"]) {
        ("amenity", "fast_food", "fast-food places")
    } else if has(&["cafe", "café", "coffee", "kaffee"]) {
        ("amenity", "cafe", "cafés")
    } else if has(&["supermarket", "supermarkt", "groceries", "lebensmittel", "einkaufen"]) {
        ("shop", "supermarket", "supermarkets")
    } else if has(&["bakery", "bäckerei", "baeckerei", "bäcker"]) {
        ("shop", "bakery", "bakeries")
    } else if has(&["pharmacy", "apotheke", "chemist"]) {
        ("amenity", "pharmacy", "pharmacies")
    } else if has(&["atm", "cash machine", "geldautomat", "bank"]) {
        ("amenity", "atm", "cash machines")
    } else if has(&["hospital", "krankenhaus", "klinik"]) {
        ("amenity", "hospital", "hospitals")
    } else if has(&["parking", "parkplatz", "parkhaus"]) {
        ("amenity", "parking", "car parks")
    } else if has(&["motorcycle", "motorrad"]) {
        ("shop", "motorcycle", "motorcycle shops")
    } else if has(&["bus stop", "bushaltestelle", "haltestelle"]) {
        ("highway", "bus_stop", "bus stops")
    } else if has(&["train station", "bahnhof", "railway"]) {
        ("railway", "station", "stations")
    } else if has(&["kiosk", "späti"]) {
        ("shop", "kiosk", "kiosks")
    } else if has(&["toilet", "toilette", "wc"]) {
        ("amenity", "toilets", "toilets")
    } else {
        return None;
    })
}

static ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:route|directions|the way|a route|navigate|how far is it|how far|how long|distance|travel time|wie weit ist es|wie weit|wie lange|entfernung|route)\s*(?:to|zu|zum|zur|nach|is it to|bis|from here to|von hier zu|von hier nach)?\s+(.+)$")
        .expect("valid route regex")
});

/// Driving time and distance (OSRM public router).
pub async fn driving(http: &reqwest::Client, from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
    let url = format!("https://router.project-osrm.org/route/v1/driving/{:.6},{:.6};{:.6},{:.6}?overview=false", from.1, from.0, to.1, to.0);
    let v: Value = http.get(url).send().await.ok()?.json().await.ok()?;
    let r = v["routes"].get(0)?;
    Some((r["duration"].as_f64()?, r["distance"].as_f64()?))
}

pub fn minutes_text(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    match m {
        0..=1 => "about a minute".into(),
        2..=59 => format!("{m} minutes"),
        _ => format!("{} h {:02} min", m / 60, m % 60),
    }
}

async fn nearby(tools: &ToolContext, query: &str, radius: f64) -> Result<String, String> {
    let engine = &tools.stark.location;
    let fix = engine.current(tools).await?;
    tools.stark.pulse(&["brain", "gps", "context"], false);
    let q = query.trim();
    let q = q.strip_prefix("JAI").or_else(|| q.strip_prefix("jai")).unwrap_or(q).trim_start_matches([',', ' ']).trim();
    let lower = q.to_lowercase();
    if lower.is_empty() || ["where am i", "wo bin ich", "my location", "mein standort", "current location"].iter().any(|w| lower.contains(w)) {
        let address = match &fix.address {
            Some(a) => a.clone(),
            None => engine.reverse_geocode(fix.lat, fix.lon).await.unwrap_or_else(|| format!("{:.5}, {:.5}", fix.lat, fix.lon)),
        };
        return Ok(format!(
            "Sir is at {address} ({:.5}, {:.5}, ±{:.0} m, source: {}){}{}.",
            fix.lat,
            fix.lon,
            fix.accuracy_m,
            fix.source,
            fix.speed_kmh.filter(|s| *s > 3.0).map(|s| format!(", moving at {s:.0} km/h")).unwrap_or_default(),
            fix.altitude_m.map(|a| format!(", altitude {a:.0} m")).unwrap_or_default()
        ));
    }
    // "How far is it to Freiburg?", "route to school".
    if let Some(c) = ROUTE.captures(q) {
        let dest = c[1].trim().trim_end_matches('?').to_string();
        let cfg = tools.config.read(|c| c.location.clone());
        let target = if ["school", "schule"].iter().any(|w| dest.to_lowercase() == *w) && cfg.school_lat.is_some() {
            (cfg.school_lat.unwrap_or(0.0), cfg.school_lon.unwrap_or(0.0), if cfg.school_label.is_empty() { "school".into() } else { cfg.school_label.clone() })
        } else if ["home", "nach hause", "zuhause"].iter().any(|w| dest.to_lowercase() == *w) && cfg.home_lat.is_some() {
            (cfg.home_lat.unwrap_or(0.0), cfg.home_lon.unwrap_or(0.0), "home".into())
        } else {
            engine.geocode(&dest, Some((fix.lat, fix.lon))).await?
        };
        let crow = haversine_m(fix.lat, fix.lon, target.0, target.1);
        let drive = driving(engine.http(), (fix.lat, fix.lon), (target.0, target.1)).await;
        let mut text = format!("{} is {} away as the crow flies, to the {}.", target.2, spoken_distance(crow), bearing(fix.lat, fix.lon, target.0, target.1));
        match drive {
            Some((secs, metres)) => text.push_str(&format!(" By road: {} ({}), about {} by car or motorcycle.", spoken_distance(metres), metres.round(), minutes_text(secs))),
            None => text.push_str(" (The router was not reachable for a driving time.)"),
        }
        text.push_str(&format!(" Cycling about {}, walking about {}.", minutes_text(crow * 1.3 / (16.0 / 3.6)), minutes_text(crow * 1.25 / (4.8 / 3.6))));
        return Ok(text);
    }
    let radius = radius.clamp(100.0, 25_000.0);
    let selector = match poi_filter(q) {
        Some((k, v, _)) => format!("nwr[\"{k}\"=\"{v}\"](around:{radius:.0},{:.6},{:.6});", fix.lat, fix.lon),
        None => {
            let safe: String = q.chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect();
            format!("nwr[\"name\"~\"{}\",i](around:{radius:.0},{:.6},{:.6});", safe.trim(), fix.lat, fix.lon)
        }
    };
    let label = poi_filter(q).map(|(_, _, l)| l.to_string()).unwrap_or_else(|| format!("places matching \"{q}\""));
    let ql = format!("[out:json][timeout:20];({selector});out center 40;");
    let resp = engine
        .http()
        .post("https://overpass-api.de/api/interpreter")
        .form(&[("data", ql)])
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| format!("The Overpass API is not reachable ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("The Overpass API answered HTTP {}", resp.status()));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut found: Vec<(f64, String)> = v["elements"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let lat = e["lat"].as_f64().or(e["center"]["lat"].as_f64())?;
                    let lon = e["lon"].as_f64().or(e["center"]["lon"].as_f64())?;
                    let t = &e["tags"];
                    let name = t["name"].as_str().or(t["brand"].as_str()).or(t["operator"].as_str()).unwrap_or("(unnamed)");
                    let street = format!("{} {}", t["addr:street"].as_str().unwrap_or(""), t["addr:housenumber"].as_str().unwrap_or("")).trim().to_string();
                    let d = haversine_m(fix.lat, fix.lon, lat, lon);
                    let mut line = format!("{name} — {} {}", spoken_distance(d), bearing(fix.lat, fix.lon, lat, lon));
                    if !street.is_empty() {
                        line.push_str(&format!(", {street}"));
                    }
                    if let Some(h) = t["opening_hours"].as_str() {
                        line.push_str(&format!(" (hours: {h})"));
                    }
                    Some((d, line))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.dedup_by(|a, b| a.1 == b.1);
    if found.is_empty() {
        return Ok(format!("No {label} within {} of Sir's position ({}).", spoken_distance(radius), fix.source));
    }
    Ok(format!(
        "{} {label} within {} of Sir ({}), nearest first:\n{}",
        found.len().min(8),
        spoken_distance(radius),
        fix.source,
        found.iter().take(8).map(|(_, l)| format!("- {l}")).collect::<Vec<_>>().join("\n")
    ))
}

// --------------------------------------------------------------------------- tools ----

static EXPLICIT_PLACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:in|for|at|bei|für|fuer|near|around)\s+(\p{Lu}[\p{L}\-]{2,})").expect("valid explicit place regex")
});
static RIDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(motorcycle|motorbike|motorrad|ride|riding|bike ride|commute|pendeln|leave for (?:school|work)|need to leave|have to leave|should i leave|when (?:do|should) i (?:leave|go)|wann muss ich (?:los|losfahren|aufbrechen)|losfahren|aufbrechen|mit dem roller|scooter)\b")
        .expect("valid ride regex")
});
static WEATHER_HERE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(weather|wetter|forecast|vorhersage|rain|raining|regen|regnet|temperature|temperatur|degrees outside|wind|windy|umbrella|regenschirm|jacket|jacke|cold outside|kalt draußen|warm outside)\b").expect("valid weather regex")
});
static NEARBY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(nearest|nearby|closest|near me|around here|in der nähe|nächste|naechste|where am i|wo bin ich|how far (?:is it )?(?:to|from here)|wie weit (?:ist es )?(?:nach|bis|zu)|route to|directions to)\b").expect("valid nearby regex")
});

/// Location questions answered from Sir's live position up front (before the generic web search).
pub fn upfront(text: &str) -> Option<(&'static str, Value)> {
    let lower = text.to_lowercase();
    let day = if lower.contains("tomorrow") || (lower.contains("morgen") && !lower.contains("heute morgen")) {
        Some("tomorrow")
    } else if lower.contains("tonight") || lower.contains("later") || lower.contains("heute") || lower.contains("today") {
        Some("today")
    } else {
        None
    };
    if RIDE.is_match(text) {
        let mode = if ["motorcycle", "motorbike", "motorrad", "ride", "riding"].iter().any(|w| lower.contains(w)) {
            "motorcycle"
        } else if lower.contains("bike") || lower.contains("fahrrad") || lower.contains("cycle") {
            "bike"
        } else if lower.contains("walk") || lower.contains("zu fuß") {
            "walk"
        } else if lower.contains("bus") || lower.contains("train") || lower.contains("bahn") {
            "transit"
        } else if lower.contains("car") || lower.contains("auto") {
            "car"
        } else {
            ""
        };
        let leaving = ["leave", "los", "aufbrechen", "commute", "pendeln"].iter().any(|w| lower.contains(w));
        let destination = if lower.contains("school") || lower.contains("schule") {
            "school"
        } else if leaving {
            "next"
        } else {
            ""
        };
        let mut args = json!({ "travel_mode": mode, "destination": destination });
        if let Some(d) = day {
            args["day"] = json!(d);
        }
        return Some(("check_ride_and_commute_status", args));
    }
    if NEARBY.is_match(text) {
        return Some(("find_nearby_places_or_route", json!({ "query": text.trim().trim_end_matches(['?', '.', '!']) })));
    }
    // "CPU temperature" is the Guardian's, not the weather.
    let about_pc = ["cpu", "gpu", "graphics", "processor", "grafikkarte", "prozessor", "pc ", "computer", "system", "drive", "ssd"].iter().any(|w| lower.contains(w));
    if WEATHER_HERE.is_match(text) && !EXPLICIT_PLACE.is_match(text) && !about_pc {
        let mut args = json!({ "forecast_hours": 12 });
        if let Some(d) = day {
            args["day"] = json!(d);
        }
        return Some(("get_precise_location_weather", args));
    }
    None
}

pub fn is_tool(name: &str) -> bool {
    matches!(name, "get_precise_location_weather" | "find_nearby_places_or_route" | "manage_location_settings")
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "get_precise_location_weather",
            "description": "Hyper-local weather at Sir's exact position (live phone GPS, else home, else IP) from Open-Meteo: now plus the hourly forecast (rain probability, precipitation, wind, gusts). Use it for any weather question without a named city.",
            "parameters": {"type": "object", "properties": {
                "forecast_hours": {"type": "integer", "description": "Hours ahead (default 12)"},
                "day": {"type": "string", "description": "today, tomorrow, a weekday or YYYY-MM-DD for a whole day"}
            }, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "find_nearby_places_or_route",
            "description": "Around Sir's live position: 'where am I' (street address), nearby places (petrol stations, restaurants, supermarkets, pharmacies, cafés … or any name) within radius_meters, or distance and driving time to a destination ('how far is it to Freiburg', 'route to school').",
            "parameters": {"type": "object", "properties": {
                "query": {"type": "string"},
                "radius_meters": {"type": "integer"}
            }, "required": ["query"]}
        }}),
        json!({"type": "function", "function": {
            "name": "manage_location_settings",
            "description": "Location settings: status; set_home_here / set_school_here (from the phone's GPS); set_home / set_school with an address; geofence_on / geofence_off (home-arrival protocol); set_arrival_playlist / set_arrival_protocol; satellite (the phone link and its QR code).",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["status", "set_home_here", "set_home", "set_school_here", "set_school", "geofence_on", "geofence_off", "set_arrival_playlist", "set_arrival_protocol", "satellite"]},
                "address": {"type": "string"},
                "value": {"type": "string", "description": "Playlist or protocol name"}
            }, "required": ["action"]}
        }}),
    ]
}

pub async fn run_tool(tools: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "get_precise_location_weather" => {
            let hours = args.get("forecast_hours").and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())).unwrap_or(12) as usize;
            let today = Local::now().date_naive();
            let day = str_arg(args, &["day", "date"]).and_then(|d| crate::pim::parse_day(d, today));
            let report = weather_report(tools, hours, day).await?;
            Ok(report.text)
        }
        "find_nearby_places_or_route" => {
            let query = str_arg(args, &["query", "place", "destination"]).unwrap_or("where am i");
            let radius = args.get("radius_meters").and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok())).unwrap_or(3000.0);
            nearby(tools, query, radius).await
        }
        "manage_location_settings" => settings_tool(tools, args).await,
        other => Err(format!("Unknown location tool '{other}'.")),
    }
}

async fn settings_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = str_arg(args, &["action"]).unwrap_or("status").to_lowercase();
    let value = str_arg(args, &["value", "playlist", "protocol", "name"]).unwrap_or("").to_string();
    let engine = &tools.stark.location;
    match action.as_str() {
        "set_home_here" | "set_school_here" => {
            let fix = engine.phone_fix().ok_or("I have no live GPS fix from your phone yet, Sir; open the satellite on it first.")?;
            let label = engine.reverse_geocode(fix.lat, fix.lon).await.unwrap_or_default();
            let home = action == "set_home_here";
            tools.config.update(|c| {
                if home {
                    c.location.home_lat = Some(fix.lat);
                    c.location.home_lon = Some(fix.lon);
                    c.location.home_label = label.clone();
                } else {
                    c.location.school_lat = Some(fix.lat);
                    c.location.school_lon = Some(fix.lon);
                    c.location.school_label = label.clone();
                }
            });
            Ok(format!("{} set to your current position ({:.5}, {:.5}{}).", if home { "Home" } else { "School" }, fix.lat, fix.lon, if label.is_empty() { String::new() } else { format!(", {label}") }))
        }
        "set_home" | "set_school" => {
            let address = str_arg(args, &["address", "value", "place"]).ok_or("Which address?")?;
            let (lat, lon, label) = engine.geocode(address, None).await?;
            let home = action == "set_home";
            tools.config.update(|c| {
                if home {
                    c.location.home_lat = Some(lat);
                    c.location.home_lon = Some(lon);
                    c.location.home_label = label.clone();
                } else {
                    c.location.school_lat = Some(lat);
                    c.location.school_lon = Some(lon);
                    c.location.school_label = label.clone();
                }
            });
            Ok(format!("{} set to {label} ({lat:.5}, {lon:.5}).", if home { "Home" } else { "School" }))
        }
        "geofence_on" | "geofence_off" => {
            let on = action == "geofence_on";
            tools.config.update(|c| c.location.geofence_enabled = on);
            let home = tools.config.read(|c| c.location.home_lat.is_some());
            Ok(format!("Home geofence {}.{}", if on { "on" } else { "off" }, if on && !home { " Note: set your home first (\"set my home here\")." } else { "" }))
        }
        "set_arrival_playlist" => {
            tools.config.update(|c| c.location.arrival_playlist = value.clone());
            Ok(if value.is_empty() { "No playlist on arrival any more.".into() } else { format!("When you get home I'll start '{value}'.") })
        }
        "set_arrival_protocol" => {
            tools.config.update(|c| c.location.arrival_protocol = value.clone());
            Ok(if value.is_empty() { "No protocol on arrival any more.".into() } else { format!("When you get home I'll engage the {value} protocol.") })
        }
        "satellite" => {
            if crate::gui::windows_supported() {
                crate::gui::open_integrations(Some(crate::stark::QUICK_CONNECTIONS));
            }
            Ok(format!("The mobile satellite is at {} — the QR code is in Quick Connections.", crate::satellite::pairing_url(&tools.config)))
        }
        _ => {
            let cfg = tools.config.read(|c| c.location.clone());
            let fix = engine.phone_fix();
            Ok(format!(
                "Phone GPS: {}. Home: {}. School: {}. Geofence: {} (arrive < {:.0} m, leave > {:.0} m; on arrival: {}). IP fallback: {}.",
                fix.map(|f| format!("{:.5}, {:.5} ±{:.0} m", f.lat, f.lon, f.accuracy_m)).unwrap_or_else(|| "no live fix".into()),
                match (cfg.home_lat, cfg.home_lon) {
                    (Some(a), Some(b)) => format!("{a:.5}, {b:.5} {}", cfg.home_label),
                    _ => "not set".into(),
                },
                match (cfg.school_lat, cfg.school_lon) {
                    (Some(a), Some(b)) => format!("{a:.5}, {b:.5} {}", cfg.school_label),
                    _ => "not set".into(),
                },
                if cfg.geofence_enabled { "on" } else { "off" },
                cfg.geofence_arrive_m,
                cfg.geofence_leave_m,
                [
                    cfg.arrival_wake_hud.then_some("wake the HUD".to_string()),
                    (!cfg.arrival_playlist.is_empty()).then(|| format!("play {}", cfg.arrival_playlist)),
                    (!cfg.arrival_protocol.is_empty()).then(|| format!("engage {}", cfg.arrival_protocol)),
                    cfg.arrival_greeting.then_some("greet you".to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
                if cfg.ip_fallback { "on" } else { "off" }
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landmarks_lose_their_article_places_keep_it() {
        assert_eq!(without_article("the Eiffel Tower"), "Eiffel Tower");
        assert_eq!(without_article("The white house"), "white house");
        assert_eq!(without_article("the Hague"), "the Hague");
        assert_eq!(without_article("the Netherlands"), "the Netherlands");
        assert_eq!(without_article("Brandenburger Tor"), "Brandenburger Tor");
        assert_eq!(without_article("theatre district"), "theatre district");
    }

    #[test]
    fn geometry_and_wording() {
        let d = haversine_m(48.3400, 7.8700, 48.3490, 7.8700);
        assert!((d - 1000.8).abs() < 5.0, "{d}");
        assert_eq!(bearing(48.0, 7.0, 49.0, 7.0), "north");
        assert_eq!(spoken_distance(432.0), "430 m");
        assert_eq!(spoken_distance(2500.0), "2.5 km");
        assert_eq!(weather_code_text(63), "rain");
        assert_eq!(minutes_text(1500.0), "25 minutes");
    }

    #[test]
    fn location_questions_go_to_the_right_tool() {
        assert_eq!(upfront("How is the weather for riding my motorcycle right now and later today?").map(|t| t.0), Some("check_ride_and_commute_status"));
        assert_eq!(upfront("When do I need to leave for school tomorrow?").unwrap().1["day"], "tomorrow");
        assert_eq!(upfront("Will it rain later?").map(|t| t.0), Some("get_precise_location_weather"));
        assert_eq!(upfront("What's the weather in Berlin?"), None, "a named city goes to the web search");
        assert_eq!(upfront("Where is the nearest gas station?").map(|t| t.0), Some("find_nearby_places_or_route"));
        assert_eq!(upfront("Open Spotify"), None);
        assert_eq!(upfront("What's my CPU temperature?"), None);
        assert_eq!(upfront("How far is the moon?"), None);
        assert_eq!(poi_filter("nächste Tankstelle").map(|p| p.1), Some("fuel"));
        assert!(ROUTE.captures("how far is it to Freiburg").is_some());
    }

    #[test]
    fn geofence_fires_once_on_arrival() {
        let engine = LocationEngine::new();
        let graph = ContextGraph::new();
        let (tx, mut rx) = broadcast::channel(8);
        let cfg = LocationSettings { home_lat: Some(48.0), home_lon: Some(7.0), ..LocationSettings::default() };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let sample = |lat: f64| PhoneSample { latitude: lat, longitude: 7.0, accuracy: Some(10.0), altitude: None, speed: None, heading: None, };
            // Fixes come with an address already cached so no lookup is spawned.
            for lat in [48.02, 48.0005, 48.0004] {
                engine.inner.addresses.lock().unwrap_or_else(|e| e.into_inner()).insert(((lat * 1000.0_f64).round() as i64, 7000), "test".into());
                engine.update_from_phone(sample(lat), &cfg, &graph, &tx);
            }
        });
        let mut arrivals = 0;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, StarkEvent::Arrived { .. }) {
                arrivals += 1;
            }
        }
        assert_eq!(arrivals, 1);
        assert!(graph.location().is_some());
    }
}
