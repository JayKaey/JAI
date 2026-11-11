//! Live data around Sir, with or without the Earth open (C1): the hub of the satellites, the
//! trains, the departures board, the traffic, the weather of other days and the rain nowcast.
//!
//! * **Where Sir is** is asked in ONE place, `here()`, so a more precise "locate me" can take
//!   over there alone.
//! * **No tools of their own** (the prompt has no room left): the spoken answers are
//!   `open_jai_view` view `live` — `passes` ("when can I see the ISS?"), `traffic` ("how's
//!   the traffic on the A5?"), `weather_on` ("what was the weather in Berlin on 3 May 2020?"),
//!   `rain_soon` ("will it rain in the next hour?"), `alerts` / `set_alert` (the rain and ISS
//!   heads-ups); the Earth's layers and the departures board are its views `earth` and
//!   `departures` (`earth_layers`, `transit`).
//! * **Voice**: `voice_command` turns Sir's words into those calls (one hook in `intents`).
//! * **The watcher**: a quiet note about a quarter of an hour before rain reaches Sir (on by
//!   default, spoken only when he asked for it and proactive alerts are on), and, when he turned
//!   it on, a word five minutes before a bright ISS pass.
//!
//! Everything is keyless; under `cfg(test)` and in the verifier's dry runs nothing is fetched
//! (`offline`) and the answers come from fixed demo data.

use crate::sat_orbit::{self, Observer, Pass};
use crate::tools::{str_arg, ToolContext};
use chrono::{Local, NaiveDate, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

// --------------------------------------------------------------------------- settings ----

/// `config.json` → `live_layers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveLayerSettings {
    /// A quiet note about a quarter of an hour before rain reaches Sir's location.
    pub rain_heads_up: bool,
    /// The rain note also said aloud (only while proactive alerts are on).
    pub rain_heads_up_spoken: bool,
    /// A word five minutes before a bright ISS pass over Sir.
    pub iss_heads_up: bool,
}

impl Default for LiveLayerSettings {
    fn default() -> Self {
        Self { rain_heads_up: true, rain_heads_up_spoken: false, iss_heads_up: false }
    }
}

/// Nothing is fetched: the tests and the verifier's dry runs.
pub fn offline() -> bool {
    cfg!(test) || crate::live_self_tester::running() || crate::live_self_tester::dry()
}

// ------------------------------------------------------------------------------- HTTP ----

const USER_AGENT: &str = "JAI/3 desktop assistant";

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(12)).connect_timeout(Duration::from_secs(5)).build().unwrap_or_else(|_| reqwest::Client::new())
});

/// A GET's JSON (12 s at most).
pub async fn get_json(url: &str) -> Result<Value, String> {
    get_json_within(url, Duration::from_secs(12)).await
}

pub async fn get_json_within(url: &str, timeout: Duration) -> Result<Value, String> {
    if offline() {
        return Err("offline".into());
    }
    let response = HTTP.get(url).timeout(timeout).send().await.map_err(|e| if e.is_timeout() { "the service took too long to answer".to_string() } else { "the service is out of reach".to_string() })?;
    let status = response.status();
    if !status.is_success() {
        debug!("[LIVE] {url} → HTTP {status}");
        return Err(format!("the service answered HTTP {}", status.as_u16()));
    }
    let body = response.bytes().await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("unreadable answer: {e}"))
}

// --------------------------------------------------------------------------- geometry ----

/// Great-circle distance in km.
pub fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2) + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().clamp(0.0, 1.0).asin()
}

/// The point a fraction `t` of the way from `a` to `b` along the great circle.
pub fn great_circle(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let d = km(a, b) / 6371.0;
    if d < 1e-9 {
        return a;
    }
    let (sa, sb) = (((1.0 - t) * d).sin() / d.sin(), (t * d).sin() / d.sin());
    let x = sa * la1.cos() * lo1.cos() + sb * la2.cos() * lo2.cos();
    let y = sa * la1.cos() * lo1.sin() + sb * la2.cos() * lo2.sin();
    let z = sa * la1.sin() + sb * la2.sin();
    (z.atan2((x * x + y * y).sqrt()).to_degrees(), y.atan2(x).to_degrees())
}

// ----------------------------------------------------------------------------- places ----

/// A place to answer for.
#[derive(Debug, Clone, PartialEq)]
pub struct Spot {
    pub lat: f64,
    pub lon: f64,
    /// "Berlin"; "your area" for Sir's own position.
    pub name: String,
    pub is_here: bool,
}

/// The tools' context, for `here()` (set at start-up).
static TOOLS: OnceLock<ToolContext> = OnceLock::new();

/// Where the demo and the tests put Sir.
pub const DEMO_HERE: (f64, f64) = (48.7758, 9.1829);

/// WHERE SIR IS: the one place every live layer asks (the location engine's phone GPS → home →
/// IP today; a more precise "locate me" replaces it here).
pub async fn here() -> Result<Spot, String> {
    if offline() {
        return Ok(Spot { lat: DEMO_HERE.0, lon: DEMO_HERE.1, name: "your area".into(), is_here: true });
    }
    let tools = TOOLS.get().ok_or("I don't know where you are yet, Sir.")?;
    let fix = tools.stark.location.current(tools).await?;
    Ok(Spot { lat: fix.lat, lon: fix.lon, name: "your area".into(), is_here: true })
}

/// A few places for the demo and when the geocoder is out of reach.
fn gazetteer(name: &str) -> Option<Spot> {
    const PLACES: [(&str, f64, f64); 14] = [
        ("stuttgart", 48.7758, 9.1829),
        ("karlsruhe", 49.0069, 8.4037),
        ("berlin", 52.52, 13.405),
        ("munich", 48.1351, 11.582),
        ("münchen", 48.1351, 11.582),
        ("frankfurt", 50.1109, 8.6821),
        ("hamburg", 53.5511, 9.9937),
        ("cologne", 50.9375, 6.9603),
        ("köln", 50.9375, 6.9603),
        ("freiburg", 47.999, 7.8421),
        ("london", 51.5072, -0.1276),
        ("paris", 48.8566, 2.3522),
        ("helsinki", 60.1699, 24.9384),
        ("zurich", 47.3769, 8.5417),
    ];
    let n = name.trim().to_lowercase();
    PLACES.iter().find(|(p, ..)| n == *p || n.starts_with(&format!("{p} "))).map(|(p, lat, lon)| Spot { lat: *lat, lon: *lon, name: capitalised(p), is_here: false })
}

fn capitalised(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A named place (Open-Meteo's geocoder; the small list above when it is out of reach).
pub async fn place(name: &str) -> Result<Spot, String> {
    if offline() {
        return gazetteer(name).ok_or_else(|| format!("I couldn't find '{name}', Sir."));
    }
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json", urlencoding::encode(name.trim()));
    match get_json(&url).await {
        Ok(v) => {
            let r = &v["results"][0];
            match (r["latitude"].as_f64(), r["longitude"].as_f64()) {
                (Some(lat), Some(lon)) => Ok(Spot { lat, lon, name: r["name"].as_str().unwrap_or(name).to_string(), is_here: false }),
                _ => gazetteer(name).ok_or_else(|| format!("I couldn't find '{name}', Sir.")),
            }
        }
        Err(e) => gazetteer(name).ok_or(e),
    }
}

/// The place named in a tool's arguments, or Sir's own.
pub async fn spot(args: &Value) -> Result<Spot, String> {
    match str_arg(args, &["place", "location", "city"]).map(str::trim).filter(|p| !p.is_empty() && !is_here_word(p)) {
        Some(name) => place(name).await,
        None => here().await,
    }
}

fn is_here_word(p: &str) -> bool {
    matches!(p.to_lowercase().as_str(), "here" | "hier" | "me" | "my area" | "my location" | "home" | "zuhause" | "zu hause" | "my town" | "us" | "current location")
}

static AREA_NAMES: LazyLock<Mutex<HashMap<(i32, i32), String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The town at a point ("Stuttgart"), for sentences about Sir's own area (cached per ~10 km).
pub async fn area_name(lat: f64, lon: f64) -> Option<String> {
    if offline() {
        return Some("Stuttgart".into());
    }
    let key = ((lat * 10.0).round() as i32, (lon * 10.0).round() as i32);
    if let Some(name) = crate::crash_guard::lock(&AREA_NAMES).get(&key) {
        return Some(name.clone());
    }
    let url = format!("https://api.bigdatacloud.net/data/reverse-geocode-client?latitude={lat:.3}&longitude={lon:.3}&localityLanguage=en");
    let v = get_json(&url).await.ok()?;
    let name = [&v["city"], &v["locality"], &v["principalSubdivision"]].iter().filter_map(|x| x.as_str()).map(str::trim).find(|s| !s.is_empty())?.to_string();
    crate::crash_guard::lock(&AREA_NAMES).insert(key, name.clone());
    Some(name)
}

/// What is under a point, for "Hubble is over …": a country, else an ocean or sea.
pub async fn over_what(lat: f64, lon: f64) -> Option<String> {
    if offline() {
        return None;
    }
    let url = format!("https://api.bigdatacloud.net/data/reverse-geocode-client?latitude={lat:.3}&longitude={lon:.3}&localityLanguage=en");
    let v = get_json_within(&url, Duration::from_secs(5)).await.ok()?;
    if let Some(country) = v["countryName"].as_str().map(str::trim).filter(|c| !c.is_empty()) {
        let country = country.trim_end_matches(" (the)");
        return Some(if ["United States", "United Kingdom", "Netherlands", "Philippines", "Czech Republic", "United Arab Emirates", "Democratic Republic of the Congo"].iter().any(|c| country.starts_with(c)) { format!("the {country}") } else { country.to_string() });
    }
    v["localityInfo"]["informative"].as_array()?.iter().filter_map(|i| i["name"].as_str()).find(|n| {
        let l = n.to_lowercase();
        l.contains("ocean") || l.contains(" sea") || l.contains("gulf") || l.contains("bay")
    }).map(|n| if n.to_lowercase().starts_with("the ") { n.to_string() } else { format!("the {n}") })
}

// ------------------------------------------------------------------------------ tools ----

// No tools of their own (the prompt has no room): everything is `open_jai_view`, the globe's
// layers as Earth actions, the board as view `departures`, and the spoken answers as view `live`.

/// `open_jai_view {"view": "live", "action": …}`: an answer, no window. The actions that only
/// read (`hyper_cortex`'s table) and the one that changes a setting.
pub const READ_ACTIONS: [&str; 5] = ["passes", "traffic", "weather_on", "rain_soon", "alerts"];
pub const SET_ALERT: &str = "set_alert";

/// Runs view `live`: the block text and a "Spoken:" line. A failure is said in his words too (it
/// is phrased for Sir already: "The Autobahn service didn't answer for the A8, Sir.").
pub async fn run_view(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let _ = TOOLS.set(tools.clone());
    let action = str_arg(args, &["action"]).unwrap_or("").trim().to_lowercase();
    let result = match action.as_str() {
        "passes" => passes_tool(args).await,
        "traffic" => traffic_tool(args).await,
        "weather_on" => weather_tool(args).await,
        "rain_soon" => rain_tool(args).await,
        "alerts" | SET_ALERT => alerts_tool(tools, args),
        other => return Err(format!("'{other}' is not one of the live answers (passes, traffic, weather_on, rain_soon, alerts).")),
    };
    Ok(match result {
        Ok((display, spoken)) => crate::features::spoken(display, spoken),
        Err(e) => {
            let line = if e.ends_with("Sir.") || e.ends_with("Sir?") { e } else { format!("{}, Sir.", e.trim_end_matches('.')) };
            crate::features::spoken(line.clone(), line)
        }
    })
}

fn num(args: &Value, key: &str) -> Option<f64> {
    match args.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

// ---- satellite passes

/// "today", "tonight", "tomorrow", "on Friday" for a local time.
fn day_words(at: chrono::DateTime<Local>, now: chrono::DateTime<Local>) -> String {
    let days = (at.date_naive() - now.date_naive()).num_days();
    let evening = at.format("%H").to_string().parse::<u32>().unwrap_or(12) >= 17;
    match days {
        0 if evening => "tonight".into(),
        0 => "today".into(),
        1 if evening => "tomorrow evening".into(),
        1 if at.format("%H").to_string().parse::<u32>().unwrap_or(12) < 7 => "early tomorrow".into(),
        1 => "tomorrow".into(),
        _ => format!("on {}", at.format("%A")),
    }
}

fn local(unix: f64) -> chrono::DateTime<Local> {
    chrono::DateTime::<Utc>::from_timestamp(unix as i64, 0).unwrap_or_default().with_timezone(&Local)
}

/// (the block, the line JAI says) for a satellite's passes.
pub fn passes_answer(sat: &str, passes: &[Pass], place: &str, now_unix: f64, hours: f64) -> (String, String) {
    let mut display = format!("Visible passes · {sat} · {place}");
    for p in passes.iter().take(8) {
        display.push_str(&format!(
            "\n{}  {}–{}  max {:.0}° in the {}  {} → {}  {} min",
            local(p.start).format("%a %d.%m."),
            local(p.start).format("%H:%M"),
            local(p.end).format("%H:%M"),
            p.max_el,
            sat_orbit::compass(p.max_az),
            sat_orbit::compass(p.start_az),
            sat_orbit::compass(p.end_az),
            (p.duration_secs() / 60.0).round().max(1.0)
        ));
    }
    let days = if hours >= 47.0 { format!("{:.0} days", (hours / 24.0).round()) } else { format!("{hours:.0} hours") };
    let Some(first) = passes.first() else {
        display.push_str("\nNo visible pass");
        let spoken = format!("{sat} won't be visible from {place} in the next {days}, Sir: its passes are all in daylight or too low.");
        return (display, spoken);
    };
    let now = local(now_unix);
    let at = local(first.start);
    let mins = ((first.duration_secs()) / 60.0).round().max(1.0);
    let mut spoken = format!(
        "You can see {sat} {} at {}, Sir: it rises in the {}, climbs to {:.0} degrees in the {} and sets in the {} after about {mins:.0} minute{}.",
        day_words(at, now),
        at.format("%H:%M"),
        sat_orbit::compass_word(first.start_az),
        first.max_el,
        sat_orbit::compass_word(first.max_az),
        sat_orbit::compass_word(first.end_az),
        if mins == 1.0 { "" } else { "s" }
    );
    // A much better one later is worth a mention.
    if let Some(best) = passes.iter().skip(1).filter(|p| p.max_el >= first.max_el + 20.0).max_by(|a, b| a.max_el.total_cmp(&b.max_el)) {
        spoken.push_str(&format!(" The best one is {} at {}, {:.0} degrees up.", day_words(local(best.start), now), local(best.start).format("%H:%M"), best.max_el));
    }
    (display, spoken)
}

async fn passes_tool(args: &Value) -> Result<(String, String), String> {
    let name = str_arg(args, &["satellite", "name", "object"]).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("ISS").to_string();
    let hours = num(args, "hours").unwrap_or(72.0).clamp(6.0, 240.0);
    let spot = spot(args).await?;
    let elements = crate::earth_layers::elements_for(&name).await?;
    let label = crate::earth_layers::display_name(&elements);
    let model = sat_orbit::Sgp4::new(&elements)?;
    let observer = Observer { lat: spot.lat, lon: spot.lon, alt_km: 0.3 };
    let from = if offline() { elements.epoch_unix } else { sat_orbit::now_unix() };
    let passes = tokio::task::spawn_blocking(move || sat_orbit::visible_passes(&model, &observer, from, hours)).await.map_err(|e| e.to_string())?;
    let place = if spot.is_here { area_name(spot.lat, spot.lon).await.unwrap_or_else(|| "your area".into()) } else { spot.name.clone() };
    Ok(passes_answer(&label, &passes, &place, from, hours))
}

// ---- traffic

async fn traffic_tool(args: &Value) -> Result<(String, String), String> {
    if let Some(road) = str_arg(args, &["road", "motorway", "autobahn", "name"]).and_then(|r| crate::traffic::normalize_road(r).or_else(|| crate::traffic::normalize_road(&format!("a{}", r.trim().trim_start_matches(['A', 'a']))))) {
        let list = crate::traffic::road_incidents(&road, true).await.map_err(|e| format!("The Autobahn service didn't answer for the {road}: {e}"))?;
        return Ok(crate::traffic::road_answer(&road, &list));
    }
    // (A `name` that is no motorway is the place.)
    let spot = match (str_arg(args, &["place", "location", "city"]), str_arg(args, &["name"])) {
        (None, Some(name)) => self::spot(&json!({ "place": name })).await?,
        _ => self::spot(args).await?,
    };
    let radius = num(args, "radius_km").unwrap_or(crate::traffic::NEAR_KM).clamp(2.0, 200.0);
    let jams_only = crate::tools::bool_arg(args, "jams_only").unwrap_or(true);
    let label = if spot.is_here { "you".to_string() } else { spot.name.clone() };
    let all = if crate::traffic::in_london(spot.lat, spot.lon) {
        crate::traffic::tfl().await?
    } else if crate::traffic::in_germany(spot.lat, spot.lon) {
        crate::traffic::all_incidents().await?
    } else {
        return Err("I only have live traffic for the German motorways and London's roads, Sir.".into());
    };
    let found = crate::traffic::near(&all, spot.lat, spot.lon, radius);
    Ok(crate::traffic::area_answer(&label, &found, radius, jams_only))
}

// ---- weather of other days

fn iso_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

async fn weather_tool(args: &Value) -> Result<(String, String), String> {
    use crate::weather_history as wh;
    let today = Local::now().date_naive();
    let when = match (str_arg(args, &["from", "start_date"]).and_then(iso_day), str_arg(args, &["to", "end_date"]).and_then(iso_day)) {
        (Some(from), Some(to)) if to >= from => Some(wh::When::Range { from, to, label: format!("{} to {}", from.format("%-d %B %Y"), to.format("%-d %B %Y")) }),
        _ => str_arg(args, &["date", "day", "when"]).and_then(|d| iso_day(d).map(wh::When::Day).or_else(|| wh::parse_when(&d.to_lowercase(), today))),
    };
    let when = when.ok_or("Which day, Sir?")?;
    let (from, to) = when.span();
    wh::check_span(from, to, today)?;
    let spot = spot(args).await?;
    let name = if spot.is_here { area_name(spot.lat, spot.lon).await.unwrap_or_else(|| "your area".into()) } else { spot.name.clone() };
    let days = if offline() { wh::demo_days(from, to) } else { wh::parse_daily(&get_json(&wh::daily_url(spot.lat, spot.lon, from, to, today)).await?) };
    if days.is_empty() {
        return Err("The weather archive had nothing for that, Sir.".into());
    }
    let focus = str_arg(args, &["focus"]).map(str::to_lowercase);
    Ok(wh::answer(&days, &when, &name, today, focus.as_deref().filter(|f| matches!(*f, "cold" | "warm" | "rain"))))
}

// ---- the rain nowcast

async fn nowcast(spot: &Spot) -> Result<(Vec<crate::weather_history::Quarter>, chrono::NaiveDateTime), String> {
    use crate::weather_history as wh;
    if offline() {
        let now = Local::now().naive_local();
        return Ok((wh::demo_nowcast(now), now));
    }
    let v = get_json(&wh::nowcast_url(spot.lat, spot.lon)).await?;
    Ok((wh::parse_nowcast(&v), wh::local_now(&v, Utc::now())))
}

async fn rain_tool(args: &Value) -> Result<(String, String), String> {
    let spot = spot(args).await?;
    let minutes = num(args, "minutes").unwrap_or(60.0).clamp(15.0, 150.0) as i64;
    let (quarters, now) = nowcast(&spot).await?;
    Ok(crate::weather_history::nowcast_answer(&quarters, now, &spot.name, minutes))
}

// ---- the heads-ups

fn alerts_tool(tools: &ToolContext, args: &Value) -> Result<(String, String), String> {
    // ("alerts" only reads; "set_alert" with "set": rain_on … changes.)
    let action = if args["action"] == SET_ALERT { str_arg(args, &["set"]).unwrap_or("status").to_lowercase() } else { "status".to_string() };
    let change = |f: fn(&mut LiveLayerSettings)| {
        tools.config.update(|c| f(&mut c.live_layers));
    };
    let spoken = match action.as_str() {
        "rain_on" => {
            change(|s| s.rain_heads_up = true);
            "I'll let you know shortly before it rains, Sir."
        }
        "rain_off" => {
            change(|s| {
                s.rain_heads_up = false;
                s.rain_heads_up_spoken = false;
            });
            "No more rain notes, Sir."
        }
        "rain_spoken_on" => {
            change(|s| {
                s.rain_heads_up = true;
                s.rain_heads_up_spoken = true;
            });
            "I'll tell you out loud before it rains, Sir."
        }
        "rain_spoken_off" => {
            change(|s| s.rain_heads_up_spoken = false);
            "The rain notes stay quiet again, Sir."
        }
        "iss_on" => {
            change(|s| s.iss_heads_up = true);
            "I'll tell you five minutes before a bright pass of the space station, Sir."
        }
        "iss_off" => {
            change(|s| s.iss_heads_up = false);
            "No more space station alerts, Sir."
        }
        _ => "",
    };
    let s = tools.config.read(|c| c.live_layers.clone());
    let on_off = |b: bool| if b { "on" } else { "off" };
    let display = format!(
        "Live heads-ups\nRain note: {}{}\nISS pass: {}",
        on_off(s.rain_heads_up),
        if s.rain_heads_up_spoken { " (spoken)" } else { "" },
        on_off(s.iss_heads_up)
    );
    let spoken = if spoken.is_empty() {
        format!(
            "The rain note is {}{}, and the space station alert is {}, Sir.",
            on_off(s.rain_heads_up),
            if s.rain_heads_up && s.rain_heads_up_spoken { " and spoken" } else { "" },
            on_off(s.iss_heads_up)
        )
    } else {
        spoken.to_string()
    };
    Ok((display, spoken))
}

// ------------------------------------------------------------------------------ voice ----

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid live-layers regex")
}

/// The satellite a sentence names, for passes and "where is …": (name to look up, is it the ISS).
fn satellite_named(s: &str) -> Option<(String, bool)> {
    let padded = format!(" {s} ");
    let word = |w: &str| padded.contains(&format!(" {w} "));
    if ["iss", "i.s.s", "international space station", "space station", "raumstation", "internationale raumstation"].iter().any(|w| word(w)) && !word("chinese") && !word("chinesische") {
        return Some(("ISS".into(), true));
    }
    if word("hubble") || word("hst") {
        return Some(("Hubble".into(), false));
    }
    if ["tiangong", "tianhe", "css", "chinese space station", "chinesische raumstation"].iter().any(|w| word(w)) {
        return Some(("Tiangong".into(), false));
    }
    // "NOAA 20", "Sentinel-2A", "Meteosat 11", "Starlink-1234": a known family with a number.
    static FAMILY: LazyLock<Regex> = LazyLock::new(|| re(r"\b((?:noaa|goes|meteosat|sentinel|landsat|metop|himawari|starlink|oneweb|iridium|cosmos|fengyun|terra|aqua|suomi|gps|galileo|envisat|jason|cryosat|swarm|grace)[\s-]?[0-9][0-9a-z-]*)\b"));
    FAMILY.captures(s).map(|c| (c[1].to_uppercase(), false))
}

/// The part of a sentence after one of `leads` ("departures from Karlsruhe Hbf" → "karlsruhe hbf").
fn after_lead(s: &str, leads: &[&str]) -> Option<String> {
    let padded = format!(" {s}");
    leads
        .iter()
        .filter_map(|l| padded.rfind(&format!(" {l} ")).map(|i| padded[i + l.len() + 2..].trim().to_string()))
        .filter(|rest| !rest.is_empty())
        .min_by_key(|rest| rest.len())
}

/// "on the earth", "on the map", "right now": tails that name no place.
fn strip_tails(s: &str) -> String {
    static TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"\s+(?:on|auf)\s+(?:the|my|der|dem|die)\s+(?:earth|globe|map|world\s+map|erde|globus|karte|weltkarte)$|\s+(?:right\s+now|at\s+the\s+moment|currently|gerade|jetzt|im\s+moment|today|heute|please|bitte)$"));
    let mut out = s.trim().to_string();
    for _ in 0..3 {
        let next = TAIL.replace(&out, "").trim().to_string();
        if next == out {
            break;
        }
        out = next;
    }
    out
}

/// A place in Sir's words, or None for "me" / "here".
fn place_words(rest: &str) -> Option<String> {
    let p = strip_tails(rest);
    let p = p.trim_start_matches("the ").trim_start_matches("der ").trim_start_matches("die ").trim_start_matches("dem ").trim();
    let p = p.trim_end_matches(['?', '.', '!']).trim();
    if p.is_empty() || is_here_word(p) || matches!(p, "mich" | "mir" | "uns" | "my house" | "my place" | "where i am" | "in der nähe" | "der nähe") || p.split_whitespace().count() > 5 {
        return None;
    }
    Some(capitalised(p))
}

const SHOW_VERBS: &[&str] = &["show", "show me", "display", "turn on", "put up", "bring up", "track", "plot", "map", "zeig", "zeig mir", "zeige", "zeige mir", "blende", "mark"];

fn starts_with_any(s: &str, leads: &[&str]) -> bool {
    leads.iter().any(|l| s == *l || s.starts_with(&format!("{l} ")))
}

/// Sir's words as one of the live calls: always `open_jai_view` (view `earth`, `departures`
/// or `live`). `text` is his sentence as said.
pub fn voice_command(text: &str) -> Option<(&'static str, Value)> {
    let (what, args) = voice_call(text)?;
    let action = match what {
        "open_jai_view" => return Some((what, args)),
        "get_satellite_passes" => "passes",
        "check_traffic" => "traffic",
        "get_weather_for_date" => "weather_on",
        "check_rain_nowcast" => "rain_soon",
        _ => SET_ALERT,
    };
    let mut call = json!({ "view": "live", "action": action });
    for (key, value) in args.as_object().into_iter().flatten() {
        // (A heads-up's switch goes in "set".)
        call[if key == "action" { "set" } else { key.as_str() }] = value.clone();
    }
    Some(("open_jai_view", call))
}

/// What Sir asked for, by what it does: (passes / traffic / weather / rain / heads-up, or the
/// view call itself, arguments).
fn voice_call(text: &str) -> Option<(&'static str, Value)> {
    let s = crate::intents::normalize_phrase(text);
    if s.is_empty() || s.split_whitespace().count() > 22 {
        return None;
    }
    let s = s.replace("what s ", "whats ").replace("where s ", "wheres ").replace("when s ", "whens ").replace("how s ", "hows ");
    let s = s.trim_start_matches("hey ").trim().to_string();
    let padded = format!(" {s} ");
    let has = |words: &[&str]| words.iter().any(|w| s.contains(w));
    let word = |words: &[&str]| words.iter().any(|w| padded.contains(&format!(" {w} ")));
    let earth = |action: &str| json!({ "view": "earth", "action": action });

    // ---- the heads-ups ("warn me before it rains", "tell me when the ISS flies over").
    let rainy = has(&["rain", "regen", "regnet", "regnen", "shower", "schauer", "niesel", "drizzle"]);
    let alert_words = has(&["warn me", "tell me before", "let me know before", "alert me", "heads-up", "heads up", "notify me", "warne mich", "bescheid bevor", "benachrichtige", "rain alert", "rain warning", "regenwarnung", "regen-warnung", "iss alert", "pass alert", "tell me when the iss", "tell me when the space station", "let me know when the iss", "sag mir wenn die iss", "sag mir bescheid wenn die iss"]);
    let off = word(&["stop", "off", "disable", "aus", "deactivate", "no", "keine", "nicht"]) || has(&["dont", "do not", "no more", "turn off", "switch off", "nicht mehr", "keine mehr"]);
    if alert_words {
        if satellite_named(&s).is_some_and(|(_, iss)| iss) {
            return Some(("live_alerts", json!({ "action": if off { "iss_off" } else { "iss_on" } })));
        }
        if rainy {
            let spoken = has(&["out loud", "aloud", "say it", "speak", "laut", "ansagen", "spoken"]);
            let action = match (off, spoken) {
                (true, true) => "rain_spoken_off",
                (true, false) => "rain_off",
                (false, true) => "rain_spoken_on",
                (false, false) => "rain_on",
            };
            return Some(("live_alerts", json!({ "action": action })));
        }
    }

    // ---- satellites.
    let sat_word = word(&["satellites", "satelliten", "sats"]) || has(&["satellites", "satelliten"]);
    let imagery = has(&["satellite view", "satellite mode", "satellite imagery", "satellite image", "satellite map", "satellite photo", "satellite picture", "satellitenansicht", "satellitenbild", "satellitenmodus"]);
    let phone_link = word(&["phone", "handy", "mobile", "qr", "pair", "pairing", "link", "tunnel", "app", "smartphone"]);
    // "When can I see the ISS?", "Wann sehe ich die ISS?", "next ISS pass".
    let when = starts_with_any(&s, &["when", "whens", "wann", "what time", "um wie viel uhr", "um wieviel uhr"]) || has(&["next pass", "next visible pass", "visible passes", "überflug", "ueberflug", "next flyover", "next fly-over"]);
    let seeing = has(&["see", "visible", "fly over", "flies over", "fly by", "flyover", "fly-over", "pass", "overhead", "sehe", "sichtbar", "überfl", "ueberfl", "fliegt", "spot", "watch", "beobacht"]);
    if when && seeing && !imagery {
        if let Some((name, _)) = satellite_named(&s) {
            return Some(("get_satellite_passes", json!({ "satellite": name })));
        }
    }
    // "What satellite is above me?", "welche Satelliten sind über mir?".
    let above_me = has(&["above me", "over me", "above us", "over us", "overhead", "over my head", "above my head", "above here", "over here", "über mir", "ueber mir", "über uns", "ueber uns", "über meinem kopf", "über dem haus", "über mich"]);
    if above_me && (sat_word || has(&["satellite", "satellit"])) && !when && !imagery {
        return Some(("open_jai_view", earth("show_overhead")));
    }
    // "Show me all satellites", "show me Starlink", "zeig mir die GPS-Satelliten".
    if !imagery && !phone_link {
        if let Some(group) = crate::earth_layers::group_in(&s) {
            // Only the constellations stand alone ("show my GPS location" is no satellite).
            let alone = matches!(group, "starlink" | "oneweb");
            let shows = starts_with_any(&s, SHOW_VERBS) || has(&["on the earth", "on the globe", "on the map", "auf der erde", "auf dem globus", "auf der karte"]) || starts_with_any(&s, &["where are", "wo sind", "all", "alle"]);
            if (sat_word || alone || has(&["satellit"])) && shows {
                return Some(("open_jai_view", json!({ "view": "earth", "action": "show_satellites", "group": group })));
            }
        }
    }
    // "Where is Hubble?", "find Tiangong", "wo ist Hubble?" (the ISS keeps its own layer).
    static WHERE_IS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:where\s+is|wheres|where\s+s|where\s+the\s+hell\s+is|wo\s+ist|wo\s+befindet\s+sich|find|locate|track|follow|show\s+me|show|zeig\s+mir|zeige\s+mir|zeig|verfolge)\s+(.+)$"));
    if let Some(c) = WHERE_IS.captures(&s) {
        let object = strip_tails(&c[1]);
        if let Some((name, iss)) = satellite_named(&object) {
            let explicit = has(&["satellite", "satellit", "telescope", "teleskop", "station"]) || ["hubble", "tiangong", "tianhe", "css"].iter().any(|w| object.contains(w)) || name.chars().any(|ch| ch.is_ascii_digit());
            if !iss && explicit && !imagery && !phone_link {
                return Some(("open_jai_view", json!({ "view": "earth", "action": "find_satellite", "name": name })));
            }
        }
    }

    // ---- layers off while the Earth is up ("hide the satellites", "remove the traffic").
    static HIDE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:hide|remove|turn\s+off|disable|switch\s+off|get\s+rid\s+of|clear|blende|entferne|schalte)\s+(?:the\s+|all\s+(?:the\s+)?|die\s+|den\s+|alle\s+)?(satellites|satelliten|sats|starlink|trains|züge|zuege|bahnen|traffic|verkehr|staus|jams|traffic\s+jams)(?:\s+(?:layer|layers|markers|overlay|ebene))?(?:\s+aus)?$")
    });
    if let Some(c) = HIDE.captures(&s) {
        if crate::earth::is_open() {
            let layer = match &c[1] {
                "satellites" | "satelliten" | "sats" | "starlink" => "satellites",
                "trains" | "züge" | "zuege" | "bahnen" => "trains",
                _ => "traffic",
            };
            return Some(("open_jai_view", json!({ "view": "earth", "action": "hide_layer", "layer": layer })));
        }
    }

    // ---- trains.
    let trains_word = word(&["trains", "train", "züge", "zuege", "zug", "bahn", "bahnen", "s-bahn", "u-bahn", "tram", "trams", "railway", "railways"]);
    // "Where is that train?", "wo ist mein Zug?" → on the Earth.
    if trains_word && (starts_with_any(&s, &["where is", "wheres", "where s", "wo ist", "wo bleibt", "wo fährt", "wo faehrt"]) || has(&["that train on", "my train on", "the train on the", "den zug auf", "show me that train", "show that train", "show me my train", "zeig mir den zug", "zeig mir meinen zug"])) && !word(&["trains", "züge", "zuege", "bahnen"]) && !has(&["station", "bahnhof", "hbf", "platform", "gleis"]) {
        return Some(("open_jai_view", earth("show_train")));
    }
    // The departures board.
    let departures_word = has(&["departures", "departure board", "departure times", "abfahrten", "abfahrtstafel", "abfahrtszeiten", "abfahrtsplan", "abfahrtsmonitor"]);
    let next_train = has(&["next train", "my train", "next tram", "next s-bahn", "next u-bahn", "next departure", "nächste bahn", "nächster zug", "nächste zug", "naechste bahn", "naechster zug", "meine bahn", "mein zug", "nächste s-bahn", "nächste abfahrt", "next connection", "nächste verbindung"]);
    if (departures_word && !has(&["flight", "flug", "airport", "flughafen"])) || (next_train && (when || has(&["next", "nächst", "naechst", "leave", "fährt", "faehrt", "kommt"]))) {
        let mut args = json!({ "view": "departures", "action": "show" });
        if let Some(station) = after_lead(&s, &["from", "at", "for", "in", "ab", "von", "vom", "am", "für", "fuer", "of"]).and_then(|r| place_words(&r)) {
            args["station"] = json!(station);
        }
        return Some(("open_jai_view", args));
    }
    // "Show me the trains around Stuttgart", "live trains", "zeig mir die Züge in Helsinki".
    if word(&["trains", "züge", "zuege", "bahnen", "railway traffic", "zugverkehr"]) && (starts_with_any(&s, SHOW_VERBS) || has(&["live trains", "trains on the", "züge auf der"])) {
        let mut args = earth("show_trains");
        if let Some(place) = after_lead(&s, &["around", "near", "in", "over", "at", "um", "bei", "über", "ueber", "nahe"]).and_then(|r| place_words(&r)) {
            args["place"] = json!(place);
        }
        return Some(("open_jai_view", args));
    }

    // ---- traffic.
    let traffic_word = has(&["traffic", "verkehr", "stau", "staus", "jam", "jams", "congestion", "accident", "accidents", "unfall", "unfälle", "unfaelle", "closure", "closures", "sperrung", "gesperrt", "roadworks", "baustelle", "baustellen"]);
    let not_roads = has(&["network", "netzwerk", "wifi", "wlan", "router", "website", "server", "internet", "datenverkehr", "bandwidth", "bandbreite", "upload", "download", "port ", "traffic light", "ampel", "data traffic", "web traffic"]);
    if traffic_word && !not_roads {
        let road = crate::traffic::normalize_road(&s);
        if starts_with_any(&s, SHOW_VERBS) {
            let mut args = earth("show_traffic");
            if let Some(road) = &road {
                args["road"] = json!(road);
            } else if let Some(place) = after_lead(&s, &["around", "near", "in", "over", "at", "um", "bei", "in der nähe von"]).and_then(|r| place_words(&r)) {
                args["place"] = json!(place);
            }
            return Some(("open_jai_view", args));
        }
        if let Some(road) = road {
            return Some(("check_traffic", json!({ "road": road })));
        }
        let asks = starts_with_any(&s, &["how", "hows", "hows the", "is there", "are there", "any", "anything", "what", "whats", "gibt es", "gibts", "wie", "wie ist", "ist", "sind", "check", "prüf", "pruef", "traffic", "stau", "staus", "verkehr"]);
        if asks {
            let mut args = json!({ "jams_only": true });
            if let Some(place) = after_lead(&s, &["around", "near", "in", "at", "um", "bei", "nahe", "in der nähe von", "rund um"]).and_then(|r| place_words(&r)) {
                args["place"] = json!(place);
            }
            return Some(("check_traffic", args));
        }
    }
    // "How's the A5?", "wie sieht's auf der A8 aus?".
    if let Some(road) = crate::traffic::normalize_road(&s) {
        if starts_with_any(&s, &["hows the", "how is the", "hows it looking on the", "how does the", "is the", "wie siehts auf der", "wie sieht es auf der", "wie ist die", "ist die"]) && (s.ends_with(&road.to_lowercase()) || has(&["looking", "clear", "frei", "aus", "free"])) {
            return Some(("check_traffic", json!({ "road": road })));
        }
    }

    // ---- the rain nowcast ("will it rain in the next hour?", "regnet es gleich?").
    let soon = has(&["next hour", "next half hour", "next half an hour", "next 30 minutes", "next thirty minutes", "next 15 minutes", "next fifteen minutes", "next few minutes", "next 45 minutes", "next two hours", "next 2 hours", "next 90 minutes", "next minutes", "soon", "any minute", "shortly", "in a bit", "in a moment", "within the hour", "this hour", "gleich", "bald", "nächsten stunde", "naechsten stunde", "nächste stunde", "nächsten minuten", "naechsten minuten", "nächsten zwei stunden", "in kürze", "in kuerze", "demnächst", "jeden moment", "halben stunde"]);
    let starts_or_stops = has(&["start raining", "start to rain", "rain start", "rain begin", "begin to rain", "begin raining", "rain stop", "stop raining", "rain end", "rain let up", "anfangen zu regnen", "an zu regnen", "fängt es an", "regen auf", "aufhören zu regnen", "hört es auf", "hoert es auf", "rain arrive", "rain get here", "rain coming", "regen kommt", "kommt regen", "kommt der regen"]);
    if rainy && (soon || starts_or_stops) && !has(&["tomorrow", "morgen ", "next week", "weekend", "wochenende"]) {
        let mut args = json!({});
        static MINUTES: LazyLock<Regex> = LazyLock::new(|| re(r"next\s+(\d{1,3})\s+(?:minutes|mins|minuten)"));
        let minutes = if let Some(c) = MINUTES.captures(&s) {
            c[1].parse::<i64>().unwrap_or(60)
        } else if has(&["half hour", "half an hour", "halben stunde"]) {
            30
        } else if has(&["two hours", "2 hours", "zwei stunden", "90 minutes"]) {
            120
        } else {
            60
        };
        args["minutes"] = json!(minutes);
        // The time words go first ("… in Berlin in the next 30 minutes" → "… in Berlin").
        static TIME_TAIL: LazyLock<Regex> = LazyLock::new(|| {
            re(r"\s+(?:in|within|during|over|for)\s+(?:the\s+)?(?:next|coming)\b.*$|\s+in\s+(?:den|der)\s+(?:nächsten|naechsten|kommenden)\b.*$|\s+(?:soon|shortly|gleich|bald|in\s+a\s+bit|in\s+a\s+moment|any\s+minute|in\s+kürze|in\s+kuerze|demnächst)\b.*$")
        });
        let base = TIME_TAIL.replace(&strip_tails(&s), "").to_string();
        if let Some(place) = after_lead(&base, &["in", "at", "over", "for", "around", "für", "fuer", "bei", "über", "ueber"]).and_then(|r| place_words(&r)) {
            args["place"] = json!(place);
        }
        return Some(("check_rain_nowcast", args));
    }

    // ---- the weather of other days.
    weather_for_date(&s)
}

/// "What was the weather in Berlin on 3 May 2020?", "how cold was it last January here?",
/// "wie war das Wetter gestern?": past days, or a named place's day further ahead.
fn weather_for_date(s: &str) -> Option<(&'static str, Value)> {
    use crate::weather_history as wh;
    let has = |words: &[&str]| words.iter().any(|w| s.contains(w));
    if !has(&["weather", "wetter", "temperature", "temperatur", "how cold", "how warm", "how hot", "wie kalt", "wie warm", "wie heiß", "wie heiss", "rain", "regen", "regnete", "geregnet", "snow", "schnee", "geschneit", "sunny", "sonnig", "windy", "windig", "frost", "degrees", "grad"]) {
        return None;
    }
    if has(&["cpu", "gpu", "pc ", "computer", "rechner", "processor", "prozessor", "graphics card", "grafikkarte", "room", "zimmer", "server", "laptop", "fridge", "kühlschrank", "oven", "ofen"]) {
        return None;
    }
    let today = Local::now().date_naive();
    let when = wh::parse_when(s, today)?;
    let (from, to) = when.span();
    // The place: what is left once the date words are gone ("… in Berlin on" → "Berlin").
    static RELATIVE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:the\s+day\s+before\s+yesterday|day\s+after\s+tomorrow|yesterday|vorgestern|gestern|übermorgen|uebermorgen|tomorrow|today|heute|morgen|last\s+week|past\s+week|letzte\s+woche|letzten\s+woche|vergangene\s+woche|last\s+month|letzten\s+monat|vergangenen\s+monat|last\s+year|letztes\s+jahr|vergangenes\s+jahr)\b"));
    let rest = RELATIVE.replace_all(&wh::without_dates(s), " ").to_string();
    let mut words: Vec<&str> = rest.split_whitespace().collect();
    while words.last().is_some_and(|w| matches!(*w, "on" | "am" | "at" | "like" | "then" | "damals" | "there" | "so" | "here" | "hier" | "the" | "der" | "die" | "das" | "im" | "in" | "during" | "it" | "es" | "sir" | "of" | "for" | "be" | "been" | "aus" | "um" | "von")) {
        words.pop();
    }
    let rest = words.join(" ");
    let place = after_lead(&rest, &["in", "at", "for", "für", "fuer", "bei", "around", "um", "over", "über"]).and_then(|p| place_words(&p)).filter(|p| {
        let l = p.to_lowercase();
        !l.contains("weather") && !l.contains("wetter") && wh::parse_when(&l, today).is_none()
    });
    let past = to < today;
    let ahead = from > today + chrono::Duration::days(2);
    if !(past || ahead || (from >= today && place.is_some())) {
        return None;
    }
    let mut args = match &when {
        wh::When::Day(d) => json!({ "date": d.format("%Y-%m-%d").to_string() }),
        wh::When::Range { from, to, .. } => json!({ "from": from.format("%Y-%m-%d").to_string(), "to": to.format("%Y-%m-%d").to_string(), "date": when_label(&when) }),
    };
    if let Some(place) = place {
        args["place"] = json!(place);
    }
    if let Some(focus) = wh::focus_of(s).filter(|_| !has(&["weather", "wetter"]) || has(&["how cold", "how warm", "wie kalt", "wie warm", "rain", "regen"])) {
        args["focus"] = json!(focus);
    }
    Some(("get_weather_for_date", args))
}

fn when_label(when: &crate::weather_history::When) -> String {
    match when {
        crate::weather_history::When::Range { label, .. } => label.clone(),
        crate::weather_history::When::Day(d) => d.to_string(),
    }
}

// ---------------------------------------------------------------------------- watcher ----

/// The rain and ISS heads-ups. Off in tests and while the live self-test runs.
pub fn start(app: crate::app::Jai, stop: CancellationToken) {
    let _ = TOOLS.set(app.tools().clone());
    if cfg!(test) {
        return;
    }
    tokio::spawn(async move {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut rain_checked: Option<Instant> = None;
        let mut rain_said: Option<Instant> = None;
        let mut iss_checked: Option<Instant> = None;
        let mut iss_passes: Vec<Pass> = Vec::new();
        let mut iss_said: Vec<i64> = Vec::new();
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            if offline() {
                continue;
            }
            let (settings, proactive) = app.config().read(|c| (c.live_layers.clone(), c.proactive_alerts));
            // Rain: every ten minutes, at most one note in three hours.
            if settings.rain_heads_up && rain_checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(600)) && rain_said.is_none_or(|t| t.elapsed() >= Duration::from_secs(3 * 3600)) {
                rain_checked = Some(Instant::now());
                if let Ok(spot) = here().await {
                    if let Ok((quarters, now)) = nowcast(&spot).await {
                        if let Some((minutes, mm)) = crate::weather_history::rain_heads_up(&quarters, now) {
                            rain_said = Some(Instant::now());
                            let strength = if mm >= 1.0 { "heavy rain" } else if mm >= 0.25 { "rain" } else { "light rain" };
                            let line = format!("Heads-up, Sir: {strength} in about {minutes} minutes.");
                            info!("[LIVE] {line}");
                            if settings.rain_heads_up_spoken && proactive {
                                crate::announcements::say(line);
                            } else {
                                app.notice(line);
                            }
                        }
                    }
                }
            }
            // The ISS: the next twelve hours' passes every half hour; a word five minutes
            // before a bright one (high in a dark sky).
            if settings.iss_heads_up {
                if iss_checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(1800)) {
                    iss_checked = Some(Instant::now());
                    if let (Ok(spot), Ok(elements)) = (here().await, crate::earth_layers::elements_for("ISS").await) {
                        if let Ok(model) = sat_orbit::Sgp4::new(&elements) {
                            let observer = Observer { lat: spot.lat, lon: spot.lon, alt_km: 0.3 };
                            let from = sat_orbit::now_unix();
                            if let Ok(passes) = tokio::task::spawn_blocking(move || sat_orbit::visible_passes(&model, &observer, from, 12.0)).await {
                                iss_passes = passes;
                            }
                        }
                    }
                }
                let now = sat_orbit::now_unix();
                if let Some(p) = iss_passes.iter().find(|p| p.max_el >= 25.0 && (p.start - now) > 150.0 && (p.start - now) <= 390.0 && !iss_said.contains(&(p.start as i64))) {
                    iss_said.push(p.start as i64);
                    iss_said.retain(|t| (*t as f64) > now - 86_400.0);
                    let line = format!(
                        "The space station passes over in about {:.0} minutes, Sir: it rises in the {} and climbs to {:.0} degrees.",
                        ((p.start - now) / 60.0).round(),
                        sat_orbit::compass_word(p.start_az),
                        p.max_el
                    );
                    if proactive {
                        crate::announcements::say(line);
                    } else {
                        app.notice(line);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a sentence asks for, by what it does (the view call it becomes: `voice_command`).
    fn said(text: &str) -> Option<(&'static str, Value)> {
        voice_call(text)
    }

    #[test]
    fn every_call_is_a_view_call() {
        assert_eq!(voice_command("When can I see the ISS?"), Some(("open_jai_view", json!({"view": "live", "action": "passes", "satellite": "ISS"}))));
        assert_eq!(voice_command("Stau auf der A8?"), Some(("open_jai_view", json!({"view": "live", "action": "traffic", "road": "A8"}))));
        assert_eq!(voice_command("Will it rain in the next hour?"), Some(("open_jai_view", json!({"view": "live", "action": "rain_soon", "minutes": 60}))));
        assert_eq!(voice_command("Warn me before it rains."), Some(("open_jai_view", json!({"view": "live", "action": "set_alert", "set": "rain_on"}))));
        assert_eq!(voice_command("Show me Starlink."), Some(("open_jai_view", json!({"view": "earth", "action": "show_satellites", "group": "starlink"}))));
        // What only reads is known to: a question may run it.
        for action in READ_ACTIONS {
            assert!(crate::hyper_cortex::is_read_only("open_jai_view", &json!({"view": "live", "action": action})), "{action}");
        }
        assert!(!crate::hyper_cortex::is_read_only("open_jai_view", &json!({"view": "live", "action": SET_ALERT, "set": "rain_off"})));
    }

    fn tool(text: &str) -> Option<&'static str> {
        said(text).map(|(t, _)| t)
    }

    #[test]
    fn satellites_are_asked_for_by_name_and_group() {
        assert_eq!(said("Show me all satellites."), Some(("open_jai_view", json!({"view": "earth", "action": "show_satellites", "group": "active"}))));
        assert_eq!(said("Show me Starlink."), Some(("open_jai_view", json!({"view": "earth", "action": "show_satellites", "group": "starlink"}))));
        assert_eq!(said("Zeig mir die GPS Satelliten"), Some(("open_jai_view", json!({"view": "earth", "action": "show_satellites", "group": "gps"}))));
        assert_eq!(said("Where is Hubble?"), Some(("open_jai_view", json!({"view": "earth", "action": "find_satellite", "name": "Hubble"}))));
        assert_eq!(said("Wo ist das Hubble Teleskop gerade?"), Some(("open_jai_view", json!({"view": "earth", "action": "find_satellite", "name": "Hubble"}))));
        assert_eq!(said("What satellite is above me?"), Some(("open_jai_view", json!({"view": "earth", "action": "show_overhead"}))));
        assert_eq!(said("Welche Satelliten sind gerade über mir?"), Some(("open_jai_view", json!({"view": "earth", "action": "show_overhead"}))));
        // The ISS keeps the Earth's own layer; the imagery and the phone link are not satellites.
        assert_eq!(said("Where is the ISS?"), None);
        assert_eq!(said("Switch to satellite view."), None);
        assert_eq!(said("Show me the satellite link for my phone."), None);
    }

    #[test]
    fn passes_are_asked_for_in_both_languages() {
        for text in ["When can I see the ISS?", "Wann sehe ich die ISS?", "When is the space station visible?", "When does the ISS fly over?", "Wann kann ich die Raumstation sehen?", "When's the next ISS pass?"] {
            assert_eq!(said(text), Some(("get_satellite_passes", json!({"satellite": "ISS"}))), "{text}");
        }
        assert_eq!(said("When can I see Hubble?"), Some(("get_satellite_passes", json!({"satellite": "Hubble"}))));
        assert_eq!(said("Tell me when the ISS flies over."), Some(("live_alerts", json!({"action": "iss_on"}))));
    }

    #[test]
    fn trains_and_departures() {
        assert_eq!(said("When's my next train?"), Some(("open_jai_view", json!({"view": "departures", "action": "show"}))));
        assert_eq!(said("Wann fährt meine Bahn?"), Some(("open_jai_view", json!({"view": "departures", "action": "show"}))));
        assert_eq!(said("Departures from Karlsruhe Hbf."), Some(("open_jai_view", json!({"view": "departures", "action": "show", "station": "Karlsruhe Hbf"}))));
        assert_eq!(said("Show me the departures at Stuttgart Hauptbahnhof."), Some(("open_jai_view", json!({"view": "departures", "action": "show", "station": "Stuttgart Hauptbahnhof"}))));
        assert_eq!(said("Abfahrten in Ulm"), Some(("open_jai_view", json!({"view": "departures", "action": "show", "station": "Ulm"}))));
        assert_eq!(said("When is the next train from Freiburg?"), Some(("open_jai_view", json!({"view": "departures", "action": "show", "station": "Freiburg"}))));
        assert_eq!(said("Show me the trains around Stuttgart."), Some(("open_jai_view", json!({"view": "earth", "action": "show_trains", "place": "Stuttgart"}))));
        assert_eq!(said("Zeig mir die Züge in Helsinki"), Some(("open_jai_view", json!({"view": "earth", "action": "show_trains", "place": "Helsinki"}))));
        assert_eq!(said("Where is that train?"), Some(("open_jai_view", json!({"view": "earth", "action": "show_train"}))));
        assert_eq!(said("Wo ist mein Zug?"), Some(("open_jai_view", json!({"view": "earth", "action": "show_train"}))));
        assert_eq!(tool("Show me the flight departures."), None);
    }

    #[test]
    fn traffic_in_words_and_on_the_map() {
        assert_eq!(said("How's the traffic on the A5?"), Some(("check_traffic", json!({"road": "A5"}))));
        assert_eq!(said("Stau auf der A8?"), Some(("check_traffic", json!({"road": "A8"}))));
        assert_eq!(said("Any traffic jams near me?"), Some(("check_traffic", json!({"jams_only": true}))));
        assert_eq!(said("Gibt es Staus?"), Some(("check_traffic", json!({"jams_only": true}))));
        assert_eq!(said("How is the traffic around Stuttgart?"), Some(("check_traffic", json!({"jams_only": true, "place": "Stuttgart"}))));
        assert_eq!(said("Show me the traffic on the A8."), Some(("open_jai_view", json!({"view": "earth", "action": "show_traffic", "road": "A8"}))));
        assert_eq!(said("Show me the live traffic."), Some(("open_jai_view", json!({"view": "earth", "action": "show_traffic"}))));
        // Not the roads.
        assert_eq!(tool("How much traffic is on my network?"), None);
        assert_eq!(tool("Set a 5 minute timer."), None);
    }

    #[test]
    fn rain_soon_and_other_days_weather() {
        assert_eq!(said("Will it rain in the next hour?"), Some(("check_rain_nowcast", json!({"minutes": 60}))));
        assert_eq!(said("Regnet es gleich?"), Some(("check_rain_nowcast", json!({"minutes": 60}))));
        assert_eq!(said("Will it rain in Berlin in the next 30 minutes?"), Some(("check_rain_nowcast", json!({"minutes": 30, "place": "Berlin"}))));
        assert_eq!(said("When will it start raining?"), Some(("check_rain_nowcast", json!({"minutes": 60}))));
        assert_eq!(said("What was the weather in Berlin on 3 May 2020?"), Some(("get_weather_for_date", json!({"date": "2020-05-03", "place": "Berlin"}))));
        assert_eq!(said("Wie war das Wetter am 3. Mai 2020 in Berlin?"), Some(("get_weather_for_date", json!({"date": "2020-05-03", "place": "Berlin"}))));
        let (name, args) = said("How cold was it last January here?").unwrap();
        assert_eq!((name, args["focus"].as_str(), args.get("place")), ("get_weather_for_date", Some("cold"), None));
        assert!(args["from"].as_str().unwrap().ends_with("-01-01"), "{args}");
        let (name, args) = said("Wie war das Wetter gestern?").unwrap();
        assert_eq!((name, args.get("place")), ("get_weather_for_date", None));
        // Today's and tomorrow's weather here stay with the location engine; the CPU is not weather.
        assert_eq!(tool("What's the weather like?"), None);
        assert_eq!(tool("What's the weather tomorrow?"), None);
        assert_eq!(tool("How hot was my CPU yesterday?"), None);
    }

    #[test]
    fn heads_ups_are_switched_by_voice() {
        assert_eq!(said("Warn me before it rains."), Some(("live_alerts", json!({"action": "rain_on"}))));
        assert_eq!(said("Stop the rain alerts."), Some(("live_alerts", json!({"action": "rain_off"}))));
        assert_eq!(said("Warn me out loud before it rains."), Some(("live_alerts", json!({"action": "rain_spoken_on"}))));
    }

    #[test]
    fn every_phrase_reaches_its_tool_through_the_whole_router() {
        let ctx = crate::intent_engine::ReflexContext::default();
        for (text, view, action) in [
            ("Show me all satellites.", "earth", "show_satellites"),
            ("When can I see the ISS?", "live", "passes"),
            ("Wann sehe ich die ISS?", "live", "passes"),
            ("When's my next train?", "departures", "show"),
            ("Departures from Karlsruhe Hbf.", "departures", "show"),
            ("How's the traffic on the A5?", "live", "traffic"),
            ("Any traffic jams near me?", "live", "traffic"),
            ("Stau auf der A8?", "live", "traffic"),
            ("Will it rain in the next hour?", "live", "rain_soon"),
            ("What was the weather in Berlin on 3 May 2020?", "live", "weather_on"),
            ("How cold was it last January here?", "live", "weather_on"),
            ("Show me the trains around Stuttgart.", "earth", "show_trains"),
            ("Where is Hubble?", "earth", "find_satellite"),
        ] {
            let got = crate::compound_planner::resolve_one(text, &ctx);
            let ok = got.as_ref().is_some_and(|(t, a)| t == "open_jai_view" && a["view"] == view && a["action"] == action);
            assert!(ok, "{text}: {got:?}");
        }
    }

    #[test]
    fn settings_default_and_read_partially() {
        let s = LiveLayerSettings::default();
        assert!(s.rain_heads_up && !s.rain_heads_up_spoken && !s.iss_heads_up);
        let partial: LiveLayerSettings = serde_json::from_str(r#"{"iss_heads_up": true}"#).unwrap();
        assert!(partial.rain_heads_up && partial.iss_heads_up);
    }

    #[test]
    fn geometry_and_places() {
        let d = km((48.7758, 9.1829), (49.0069, 8.4037));
        assert!((d - 62.0).abs() < 2.0, "{d}");
        let mid = great_circle((0.0, 0.0), (0.0, 90.0), 0.5);
        assert!(mid.0.abs() < 1e-9 && (mid.1 - 45.0).abs() < 1e-9);
        assert_eq!(gazetteer("Zurich").map(|s| s.name), Some("Zurich".to_string()));
        assert_eq!(place_words("stuttgart on the map"), Some("Stuttgart".into()));
        assert_eq!(place_words("me"), None);
    }

    #[test]
    fn the_tools_answer_offline() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (display, spoken) = rt.block_on(passes_tool(&json!({"satellite": "ISS"}))).unwrap();
        assert!(display.starts_with("Visible passes · the ISS · Stuttgart"), "{display}");
        assert!(spoken.contains("Sir"), "{spoken}");
        let (_, spoken) = rt.block_on(traffic_tool(&json!({"road": "a 8"}))).unwrap();
        assert!(spoken.starts_with("On the A8, Sir"), "{spoken}");
        let (_, spoken) = rt.block_on(traffic_tool(&json!({}))).unwrap();
        assert!(spoken.starts_with("Near you, Sir"), "{spoken}");
        let (_, spoken) = rt.block_on(weather_tool(&json!({"date": "2020-05-03", "place": "Berlin"}))).unwrap();
        assert!(spoken.contains("Berlin had"), "{spoken}");
        let (display, _) = rt.block_on(rain_tool(&json!({}))).unwrap();
        assert!(display.starts_with("Rain nowcast · your area"), "{display}");
        assert!(rt.block_on(weather_tool(&json!({"date": "1900-01-01"}))).is_err());
    }

    #[test]
    fn pass_answers_read_naturally() {
        let now = 1_794_200_000.0;
        let pass = Pass { start: now + 3600.0, start_az: 250.0, start_el: 10.0, max_at: now + 3780.0, max_az: 180.0, max_el: 67.4, end: now + 3960.0, end_az: 100.0, end_el: 10.0 };
        let (display, spoken) = passes_answer("the ISS", &[pass], "Stuttgart", now, 72.0);
        assert!(display.contains("max 67°"), "{display}");
        assert!(spoken.starts_with("You can see the ISS "), "{spoken}");
        assert!(spoken.contains("rises in the west-south-west") || spoken.contains("rises in the west"), "{spoken}");
        assert!(spoken.ends_with("after about 6 minutes."), "{spoken}");
        let (_, none) = passes_answer("Hubble", &[], "Stuttgart", now, 72.0);
        assert_eq!(none, "Hubble won't be visible from Stuttgart in the next 3 days, Sir: its passes are all in daylight or too low.");
    }
}
