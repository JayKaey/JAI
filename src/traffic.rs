//! Live road traffic without a key: Germany's motorways from the Autobahn GmbH's own API
//! (warnings, closures and roadworks per motorway: `verkehr.autobahn.de/o/autobahn`) and
//! London's road disruptions from TfL. "How's the traffic on the A5?", "any traffic jams near
//! me?", "Stau auf der A8?" are answered in words even without the Earth open; with its traffic
//! layer on, the same incidents are drawn on the globe (`earth_layers`).
//!
//! The parsers and the sentences are pure (tested at the end); the fetches go through
//! `live_layers::get_json` and are cached for five minutes per motorway, so neither a question
//! nor the map ever asks the servers for much.

use crate::crash_guard::lock;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub const AUTOBAHN: &str = "https://verkehr.autobahn.de/o/autobahn";
pub const TFL_URL: &str = "https://api.tfl.gov.uk/Road/all/Disruption";
/// How long one motorway's incidents count as fresh.
pub const FRESH: Duration = Duration::from_secs(5 * 60);
/// How far around Sir "near me" reaches.
pub const NEAR_KM: f64 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Accident,
    Jam,
    Closure,
    Hazard,
    Roadworks,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accident => "ACCIDENT",
            Self::Jam => "JAM",
            Self::Closure => "CLOSURE",
            Self::Hazard => "WARNING",
            Self::Roadworks => "ROADWORKS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Autobahn,
    Tfl,
}

/// One thing on the road.
#[derive(Debug, Clone, PartialEq)]
pub struct Incident {
    pub id: String,
    /// "A5", "A99", or TfL's road ("A1008").
    pub road: String,
    pub kind: Kind,
    /// "Reiskirchen - Reinhardshain", or TfL's location.
    pub section: String,
    /// "Frankfurt -> Kassel", "Both directions".
    pub direction: String,
    /// The event lines as reported ("Fahrbahnschäden", "Unfall", or TfL's comment).
    pub details: Vec<String>,
    pub lat: f64,
    pub lon: f64,
    /// The stretch it covers, (lat, lon), thinned.
    pub line: Vec<(f64, f64)>,
    pub delay_min: Option<u32>,
    pub length_km: Option<f64>,
    pub source: Source,
    /// Only a slip road is closed.
    pub exit_only: bool,
    /// When it began, as reported.
    pub since: String,
}

impl Incident {
    /// "a 4 km jam", "an accident", "road damage" (English, for Jarvis's sentences).
    pub fn what(&self) -> String {
        match self.kind {
            Kind::Accident => "an accident".into(),
            Kind::Jam => match self.length_km {
                Some(km) if km >= 0.5 => format!("a {} km jam", trim_km(km)),
                _ if self.details.iter().any(|d| d.to_lowercase().contains("stockend") || d.to_lowercase().contains("slow")) => "slow traffic".into(),
                _ => "a traffic jam".into(),
            },
            Kind::Closure if self.exit_only => "a closed slip road".into(),
            Kind::Closure => "a closure".into(),
            Kind::Roadworks => "roadworks".into(),
            Kind::Hazard => self.details.iter().find_map(|d| translate(d)).unwrap_or_else(|| "a warning".into()),
        }
    }

    /// "between Reiskirchen and Reinhardshain towards Kassel".
    pub fn whereabouts(&self) -> String {
        let mut out = String::new();
        let section = self.section.trim();
        if let Some((a, b)) = section.split_once(" - ") {
            out.push_str(&format!("between {} and {}", a.trim(), b.trim()));
        } else if !section.is_empty() {
            out.push_str(&format!("at {section}"));
        }
        if let Some((_, to)) = self.direction.split_once("->") {
            let to = to.trim();
            if !to.is_empty() {
                out.push_str(&format!("{}towards {to}", if out.is_empty() { "" } else { " " }));
            }
        }
        out
    }

    /// One line for the block and the Earth's hover card.
    pub fn line_text(&self) -> String {
        let mut s = format!("{} {}", self.kind.label(), self.road);
        if !self.section.is_empty() {
            s.push_str(&format!(" · {}", self.section));
        }
        if !self.direction.trim().is_empty() {
            s.push_str(&format!(" ({})", self.direction.trim()));
        }
        if let Some(d) = self.details.first() {
            s.push_str(&format!(" — {d}"));
        }
        if let Some(m) = self.delay_min {
            s.push_str(&format!(" · +{m} min"));
        }
        s
    }
}

fn trim_km(km: f64) -> String {
    if km < 10.0 && km.fract().abs() > 0.05 {
        format!("{km:.1}")
    } else {
        format!("{km:.0}")
    }
}

/// The common German event words in English ("Fahrbahnschäden" → "road damage").
pub fn translate(de: &str) -> Option<String> {
    let s = de.trim().trim_start_matches('-').trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    const WORDS: &[(&str, &str)] = &[
        ("falschfahrer", "a wrong-way driver"),
        ("geisterfahrer", "a wrong-way driver"),
        ("unfall", "an accident"),
        ("vollsperrung", "a full closure"),
        ("stockender verkehr", "slow traffic"),
        ("zähfließender verkehr", "slow traffic"),
        ("stau", "a traffic jam"),
        ("fahrbahnschäden", "road damage"),
        ("fahrbahnschaden", "road damage"),
        ("gegenstände auf der fahrbahn", "debris on the road"),
        ("gegenstand auf der fahrbahn", "debris on the road"),
        ("liegengebliebenes fahrzeug", "a broken-down vehicle"),
        ("pannenfahrzeug", "a broken-down vehicle"),
        ("glätte", "ice on the road"),
        ("glatteis", "black ice"),
        ("schnee", "snow on the road"),
        ("nebel", "fog"),
        ("personen auf der fahrbahn", "people on the road"),
        ("tiere auf der fahrbahn", "animals on the road"),
        ("fahrstreifen gesperrt", "a lane closed"),
        ("fahrstreifen blockiert", "a lane blocked"),
        ("baustelle", "roadworks"),
        ("bauarbeiten", "roadworks"),
        ("brückenarbeiten", "bridge works"),
        ("mäharbeiten", "mowing works"),
        ("reinigungsarbeiten", "cleaning works"),
        ("gefahr", "a hazard"),
    ];
    WORDS.iter().find(|(de, _)| s.contains(de)).map(|(_, en)| en.to_string())
}

// ---------------------------------------------------------------------------- parsers ----

static LENGTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:länge:?\s*([\d.,]+)\s*km|([\d.,]+)\s*km\s+(?:stau|stockender))").expect("valid length regex"));
static ROAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:a|autobahn|bab)\s*-?\s*(\d{1,3})([ab])?\b").expect("valid road regex"));

/// "a 5", "A-8", "autobahn 81", "A64a" → "A5", "A8", "A81", "A64a".
pub fn normalize_road(s: &str) -> Option<String> {
    let c = ROAD.captures(s)?;
    let n: u32 = c[1].parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(format!("A{n}{}", c.get(2).map_or(String::new(), |m| m.as_str().to_lowercase())))
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    }
}

/// Every other point of a long line, so a motorway's worth stays light (at most `max`).
fn thin(points: Vec<(f64, f64)>, max: usize) -> Vec<(f64, f64)> {
    if points.len() <= max || max < 2 {
        return points;
    }
    let step = (points.len() - 1) as f64 / (max - 1) as f64;
    (0..max).map(|i| points[((i as f64 * step).round() as usize).min(points.len() - 1)]).collect()
}

/// One Autobahn API answer (`{"warning": [...]}`, `{"closure": [...]}`, `{"roadworks": [...]}`)
/// for `road`. Incidents only planned for later (`future`) are left out.
pub fn parse_autobahn(road: &str, v: &Value) -> Vec<Incident> {
    let Some(obj) = v.as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for items in obj.values().filter_map(Value::as_array) {
        for it in items {
            if it["future"].as_bool() == Some(true) {
                continue;
            }
            let (Some(lat), Some(lon)) = (number(&it["coordinate"]["lat"]), number(&it["coordinate"]["long"])) else { continue };
            let display = it["display_type"].as_str().unwrap_or_default();
            let title = it["title"].as_str().unwrap_or_default().trim();
            let (road_here, section) = match title.split_once('|') {
                Some((r, s)) => (r.trim().to_string(), s.trim().to_string()),
                None => (road.to_string(), title.to_string()),
            };
            let road_here = normalize_road(&road_here).unwrap_or_else(|| road.to_string());
            let description: Vec<String> = it["description"].as_array().map(|d| d.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let details: Vec<String> = description.iter().filter_map(|l| l.trim().strip_prefix("- ").map(|d| d.trim().to_string())).filter(|d| !d.is_empty()).collect();
            let text = format!("{} {}", description.join(" "), title).to_lowercase();
            let blocked = it["isBlocked"].as_str() == Some("true") || it["isBlocked"].as_bool() == Some(true);
            let jam = it.get("abnormalTrafficType").is_some_and(|t| !t.is_null()) || ["stau", "stockender verkehr", "zähfließend"].iter().any(|w| text.contains(w));
            let kind = if text.contains("unfall") {
                Kind::Accident
            } else if jam {
                Kind::Jam
            } else if display.starts_with("CLOSURE") || blocked || text.contains("vollsperrung") {
                Kind::Closure
            } else if display.contains("ROADWORKS") {
                Kind::Roadworks
            } else {
                Kind::Hazard
            };
            let length_km = LENGTH.captures(&description.join(" ")).and_then(|c| c.get(1).or(c.get(2))).and_then(|m| m.as_str().replace(',', ".").parse().ok());
            let line = it["geometry"]["coordinates"]
                .as_array()
                .map(|pts| pts.iter().filter_map(|p| Some((p.get(1)?.as_f64()?, p.get(0)?.as_f64()?))).collect())
                .unwrap_or_default();
            out.push(Incident {
                id: it["identifier"].as_str().unwrap_or_default().to_string(),
                road: road_here,
                kind,
                section,
                direction: it["subtitle"].as_str().unwrap_or_default().trim().to_string(),
                details,
                lat,
                lon,
                line: thin(line, 48),
                delay_min: number(&it["delayTimeValue"]).map(|m| m.round().max(0.0) as u32),
                length_km,
                source: Source::Autobahn,
                exit_only: display == "CLOSURE_ENTRY_EXIT",
                since: it["startTimestamp"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    out
}

/// TfL's road disruptions (London). "Minimal" works are left out.
pub fn parse_tfl(v: &Value) -> Vec<Incident> {
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|it| {
            let severity = it["severity"].as_str().unwrap_or_default();
            if matches!(severity, "Minimal" | "No impact") {
                return None;
            }
            let c = it["geography"]["coordinates"].as_array()?;
            let (lon, lat) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
            let category = it["category"].as_str().unwrap_or_default().to_lowercase();
            let comments = it["comments"].as_str().unwrap_or_default().trim().to_string();
            let lower = comments.to_lowercase();
            let kind = if category.contains("collision") || lower.contains("collision") || lower.contains("accident") {
                Kind::Accident
            } else if it["hasClosures"].as_bool() == Some(true) || lower.contains("closed") {
                Kind::Closure
            } else if category.contains("works") {
                Kind::Roadworks
            } else if lower.contains("congestion") || lower.contains("queue") || lower.contains("slow traffic") {
                Kind::Jam
            } else {
                Kind::Hazard
            };
            let location = it["location"].as_str().unwrap_or_default().trim().to_string();
            let road = location.strip_prefix('[').and_then(|l| l.split_once(']')).map(|(r, _)| r.to_string()).unwrap_or_else(|| "London".into());
            Some(Incident {
                id: it["id"].as_str().unwrap_or_default().to_string(),
                road,
                kind,
                section: location,
                direction: String::new(),
                details: [comments, it["currentUpdate"].as_str().unwrap_or_default().trim().to_string()].into_iter().filter(|s| !s.is_empty()).collect(),
                lat,
                lon,
                line: Vec::new(),
                delay_min: None,
                length_km: None,
                source: Source::Tfl,
                exit_only: false,
                since: it["startDateTime"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

pub fn parse_roads(v: &Value) -> Vec<String> {
    v["roads"].as_array().map(|r| r.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

// --------------------------------------------------------------------------- answers ----

/// Where traffic can be asked about at all.
pub fn in_germany(lat: f64, lon: f64) -> bool {
    (47.2..=55.1).contains(&lat) && (5.8..=15.1).contains(&lon)
}

pub fn in_london(lat: f64, lon: f64) -> bool {
    (51.25..=51.75).contains(&lat) && (-0.55..=0.35).contains(&lon)
}

fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        // ("…towards Heidelberg, costing about 3 minutes" has its own comma: semicolons between.)
        [init @ .., last] if parts.iter().any(|p| p.contains(',')) => format!("{}; and {last}", init.join("; ")),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn spoken_one(i: &Incident, with_road: bool) -> String {
    let mut s = i.what();
    if with_road {
        s.push_str(&format!(" on the {}", i.road));
    }
    let at = i.whereabouts();
    if !at.is_empty() {
        s.push(' ');
        s.push_str(&at);
    }
    if let Some(m) = i.delay_min.filter(|m| *m > 0) {
        s.push_str(&format!(", costing about {m} minutes"));
    }
    s
}

/// "How's the traffic on the A5?": (the block, the line Jarvis says).
pub fn road_answer(road: &str, incidents: &[Incident]) -> (String, String) {
    let mut serious: Vec<&Incident> = incidents.iter().filter(|i| i.kind != Kind::Roadworks).collect();
    serious.sort_by_key(|i| (i.kind, i.exit_only));
    let works = incidents.len() - serious.len();
    let mut display = format!("Traffic · {road}");
    if serious.is_empty() {
        display.push_str("\nNo jams, accidents or closures reported");
    }
    for i in &serious {
        display.push_str(&format!("\n• {}", i.line_text()));
    }
    if works > 0 {
        display.push_str(&format!("\n{works} roadworks"));
    }
    display.push_str("\nSource: Autobahn GmbH");
    let works_tail = match works {
        0 => String::new(),
        1 => ", just one set of roadworks".into(),
        n => format!(", just {n} roadworks"),
    };
    let spoken = if serious.is_empty() {
        format!("The {road} is clear, Sir: no jams, accidents or closures reported{works_tail}.")
    } else {
        let main: Vec<String> = serious.iter().filter(|i| !i.exit_only).take(3).map(|i| spoken_one(i, false)).collect();
        let main = if main.is_empty() { serious.iter().take(2).map(|i| spoken_one(i, false)).collect() } else { main };
        let rest = serious.len() - main.len();
        let more = if rest > 0 { format!(", plus {rest} more report{}", if rest == 1 { "" } else { "s" }) } else { String::new() };
        format!("On the {road}, Sir: {}{more}.", join_and(&main))
    };
    (display, spoken)
}

/// "Any traffic jams near me?": the serious ones within `radius_km` of a point, nearest first.
pub fn near(incidents: &[Incident], lat: f64, lon: f64, radius_km: f64) -> Vec<(Incident, f64)> {
    let mut out: Vec<(Incident, f64)> = incidents
        .iter()
        .filter(|i| i.kind != Kind::Roadworks && !i.exit_only)
        .filter_map(|i| {
            let d = std::iter::once((i.lat, i.lon)).chain(i.line.iter().copied()).map(|p| crate::live_layers::km((lat, lon), p)).fold(f64::MAX, f64::min);
            (d <= radius_km).then(|| (i.clone(), d))
        })
        .collect();
    out.sort_by(|a, b| (a.0.kind, a.1 as i64).cmp(&(b.0.kind, b.1 as i64)));
    out
}

pub fn area_answer(place: &str, found: &[(Incident, f64)], radius_km: f64, jams_only: bool) -> (String, String) {
    let shown: Vec<&(Incident, f64)> = found.iter().filter(|(i, _)| !jams_only || matches!(i.kind, Kind::Jam | Kind::Accident | Kind::Closure)).collect();
    let mut display = format!("Traffic within {radius_km:.0} km of {place}");
    if shown.is_empty() {
        display.push_str("\nNothing reported");
    }
    for (i, d) in &shown {
        display.push_str(&format!("\n• {} · {d:.0} km", i.line_text()));
    }
    let spoken = if shown.is_empty() {
        format!("Nothing on the roads within {radius_km:.0} kilometres of {place}, Sir: no jams, accidents or closures reported.")
    } else {
        let lines: Vec<String> = shown.iter().take(3).map(|(i, _)| spoken_one(i, true)).collect();
        let rest = shown.len().saturating_sub(3);
        let more = if rest > 0 { format!(", and {rest} more") } else { String::new() };
        format!("Near {place}, Sir: {}{more}.", join_and(&lines))
    };
    (display, spoken)
}

// ---------------------------------------------------------------------------- fetches ----

struct Cache {
    roads: Option<(Instant, Vec<String>)>,
    by_road: HashMap<String, (Instant, Vec<Incident>)>,
    tfl: Option<(Instant, Vec<Incident>)>,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache { roads: None, by_road: HashMap::new(), tfl: None }));

/// The motorways the API knows (a day's cache).
pub async fn roads() -> Result<Vec<String>, String> {
    if crate::live_layers::offline() {
        return Ok(["A1", "A3", "A5", "A6", "A7", "A8", "A81", "A9", "A99"].map(String::from).to_vec());
    }
    if let Some((at, list)) = &lock(&CACHE).roads {
        if at.elapsed() < Duration::from_secs(86_400) {
            return Ok(list.clone());
        }
    }
    let list = parse_roads(&crate::live_layers::get_json(&format!("{AUTOBAHN}/")).await?);
    if list.is_empty() {
        return Err("the motorway list came back empty".into());
    }
    lock(&CACHE).roads = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// One motorway's warnings and closures (and its roadworks when `works`), from the cache while
/// fresh.
pub async fn road_incidents(road: &str, works: bool) -> Result<Vec<Incident>, String> {
    if crate::live_layers::offline() {
        return Ok(demo_incidents().into_iter().filter(|i| i.road == road && i.source == Source::Autobahn).collect());
    }
    let key = format!("{road}{}", if works { "+works" } else { "" });
    if let Some((at, list)) = lock(&CACHE).by_road.get(&key) {
        if at.elapsed() < FRESH {
            return Ok(list.clone());
        }
    }
    let mut services = vec!["warning", "closure"];
    if works {
        services.push("roadworks");
    }
    let mut out = Vec::new();
    for service in services {
        let v = crate::live_layers::get_json(&format!("{AUTOBAHN}/{}/services/{service}", urlencoding::encode(road))).await?;
        out.extend(parse_autobahn(road, &v));
    }
    // (A closure may be listed under both its motorway and the one it leads to.)
    let mut seen = std::collections::HashSet::new();
    out.retain(|i| seen.insert(i.id.clone()));
    let mut cache = lock(&CACHE);
    cache.by_road.insert(key, (Instant::now(), out.clone()));
    if works {
        cache.by_road.insert(road.to_string(), (Instant::now(), out.iter().filter(|i| i.kind != Kind::Roadworks).cloned().collect()));
    }
    Ok(out)
}

/// London's disruptions (five minutes' cache).
pub async fn tfl() -> Result<Vec<Incident>, String> {
    if crate::live_layers::offline() {
        return Ok(demo_incidents().into_iter().filter(|i| i.source == Source::Tfl).collect());
    }
    if let Some((at, list)) = &lock(&CACHE).tfl {
        if at.elapsed() < FRESH {
            return Ok(list.clone());
        }
    }
    let list = parse_tfl(&crate::live_layers::get_json(TFL_URL).await?);
    lock(&CACHE).tfl = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// Every motorway's warnings and closures, eight at a time (cached, so a second question
/// within five minutes costs nothing).
pub async fn all_incidents() -> Result<Vec<Incident>, String> {
    use futures_util::stream::{self, StreamExt};
    let roads = roads().await?;
    let results: Vec<Result<Vec<Incident>, String>> = stream::iter(roads.into_iter().map(|r| async move { road_incidents(&r, false).await })).buffer_unordered(8).collect().await;
    let mut out = Vec::new();
    let mut failed = 0;
    for r in results {
        match r {
            Ok(list) => out.extend(list),
            Err(_) => failed += 1,
        }
    }
    if out.is_empty() && failed > 0 {
        return Err("the Autobahn service didn't answer".into());
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|i| seen.insert(i.id.clone()));
    Ok(out)
}

/// What the map shows: everything cached right now (no fetch).
pub fn cached() -> Vec<Incident> {
    if crate::live_layers::offline() {
        return demo_incidents();
    }
    let cache = lock(&CACHE);
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Incident> = cache.by_road.values().flat_map(|(_, l)| l.iter().cloned()).chain(cache.tfl.iter().flat_map(|(_, l)| l.iter().cloned())).collect();
    out.retain(|i| seen.insert(i.id.clone()));
    out
}

/// The motorway the map should refresh next: the one never fetched or fetched longest ago,
/// if that was more than `older_than` ago.
pub fn stalest(roads: &[String], older_than: Duration) -> Option<String> {
    let cache = lock(&CACHE);
    roads
        .iter()
        .map(|r| (r, cache.by_road.get(r).map(|(at, _)| at.elapsed())))
        .filter(|(_, age)| age.map_or(true, |a| a >= older_than))
        .max_by_key(|(_, age)| age.map_or(Duration::MAX, |a| a))
        .map(|(r, _)| r.clone())
}

/// The cached motorway list (no fetch).
pub fn cached_roads() -> Option<Vec<String>> {
    lock(&CACHE).roads.as_ref().map(|(_, r)| r.clone())
}

/// When London's list was fetched last.
pub fn tfl_age() -> Option<Duration> {
    lock(&CACHE).tfl.as_ref().map(|(at, _)| at.elapsed())
}

/// A few incidents for the demo and the tests (made up, on real motorways).
pub fn demo_incidents() -> Vec<Incident> {
    let mk = |id: &str, road: &str, kind: Kind, section: &str, direction: &str, detail: &str, lat: f64, lon: f64, delay: Option<u32>, km: Option<f64>| Incident {
        id: id.into(),
        road: road.into(),
        kind,
        section: section.into(),
        direction: direction.into(),
        details: vec![detail.into()],
        lat,
        lon,
        line: vec![(lat, lon), (lat + 0.02, lon + 0.03)],
        delay_min: delay,
        length_km: km,
        source: if road == "A1008" { Source::Tfl } else { Source::Autobahn },
        exit_only: false,
        since: String::new(),
    };
    vec![
        mk("demo-a8-jam", "A8", Kind::Jam, "Leonberg - Stuttgart-Möhringen", "Karlsruhe -> München", "Stau", 48.744, 9.06, Some(12), Some(4.0)),
        mk("demo-a8-works", "A8", Kind::Roadworks, "Pforzheim-Ost - Heimsheim", "Karlsruhe -> München", "Baustelle", 48.86, 8.79, None, None),
        mk("demo-a5-damage", "A5", Kind::Hazard, "Bruchsal - Kronau", "Karlsruhe -> Frankfurt", "Fahrbahnschäden", 49.17, 8.6, None, None),
        mk("demo-a81-accident", "A81", Kind::Accident, "Böblingen-Hulb - Sindelfingen-Ost", "Singen -> Stuttgart", "Unfall", 48.69, 9.03, Some(8), None),
        mk("demo-tfl", "A1008", Kind::Closure, "[A1008] Cambridge Park", "", "Road closed for resurfacing", 51.574, 0.023, None, None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn warning_a5() -> Value {
        json!({"warning":[{"identifier":"NLW_2026_002476","icon":"101","isBlocked":"false","future":false,"extent":"50.60863551516282,8.824087312243885,50.617450093604084,8.880933520701147","point":"50.60863551516282,8.824087312243885","display_type":"WARNING","subtitle":" Frankfurt -> Kassel","title":"A5 | Reiskirchen - Reinhardshain","startTimestamp":"2026-07-29T12:18:00+02:00","coordinate":{"lat":50.60863551516282,"long":8.824087312243885},"description":["Beginn: 29.07.26 um 12:18 Uhr","","Ereignismeldung:","- Fahrbahnschäden","","A5: Frankfurt -> Kassel, zwischen 1.7 km hinter AS Reiskirchen und 1.2 km vor Reinhardshain"," ","Ereignisnummer: NLW_2026_002476"],"geometry":{"type":"LineString","coordinates":[[8.824087312,50.608635515],[8.85,50.612],[8.880933521,50.617450094]]}}]})
    }

    #[test]
    fn autobahn_warnings_and_closures_are_read() {
        let found = parse_autobahn("A5", &warning_a5());
        assert_eq!(found.len(), 1);
        let i = &found[0];
        assert_eq!((i.road.as_str(), i.kind, i.section.as_str(), i.direction.as_str()), ("A5", Kind::Hazard, "Reiskirchen - Reinhardshain", "Frankfurt -> Kassel"));
        assert_eq!(i.details, vec!["Fahrbahnschäden".to_string()]);
        assert_eq!(i.line.len(), 3);
        assert!((i.line[0].0 - 50.6086).abs() < 1e-3 && (i.line[0].1 - 8.8241).abs() < 1e-3);
        assert_eq!(i.what(), "road damage");
        assert_eq!(i.whereabouts(), "between Reiskirchen and Reinhardshain towards Kassel");

        let jam = json!({"warning":[{"identifier":"j1","isBlocked":"false","future":false,"display_type":"WARNING","subtitle":" Karlsruhe -> München","title":"A8 | Leonberg - Stuttgart-Degerloch","coordinate":{"lat":"48.75","long":"9.1"},"delayTimeValue":"14","abnormalTrafficType":"STATIONARY_TRAFFIC","description":["Ereignismeldung:","- Stau","Länge: 3,5 km"]}]});
        let j = &parse_autobahn("A8", &jam)[0];
        assert_eq!((j.kind, j.delay_min, j.length_km), (Kind::Jam, Some(14), Some(3.5)));
        assert_eq!(j.what(), "a 3.5 km jam");

        let closure = json!({"closure":[
            {"identifier":"c1","isBlocked":"false","future":true,"display_type":"CLOSURE_ENTRY_EXIT","title":"A99 Ersatzneubau","subtitle":"x","coordinate":{"lat":48.0,"long":11.6},"description":[]},
            {"identifier":"c2","isBlocked":"true","future":false,"display_type":"CLOSURE","title":"A8 | Merklingen - Ulm-West","subtitle":" Stuttgart -> München","coordinate":{"lat":48.5,"long":9.8},"description":["- Vollsperrung"]}
        ]});
        let c = parse_autobahn("A8", &closure);
        assert_eq!(c.len(), 1, "the planned one is left out");
        assert_eq!((c[0].kind, c[0].what()), (Kind::Closure, "a closure".to_string()));
    }

    #[test]
    fn tfl_disruptions_are_read_without_the_minimal_ones() {
        let v = json!([
            {"id":"TIMS-1","severity":"Minimal","category":"Works","comments":"Temporary lights.","geography":{"type":"Point","coordinates":[0.02,51.57]},"location":"[A1008] CAMBRIDGE PARK (E11 ) (Redbridge)"},
            {"id":"TIMS-2","severity":"Serious","category":"Collisions","comments":"Collision on the approach.","currentUpdate":"Two lanes closed.","geography":{"type":"Point","coordinates":[-0.12,51.5]},"location":"[A4] Strand (WC2) (Westminster)","hasClosures":true}
        ]);
        let found = parse_tfl(&v);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].road.as_str(), found[0].kind), ("A4", Kind::Accident));
        assert!(in_london(found[0].lat, found[0].lon));
    }

    #[test]
    fn roads_are_named_the_way_the_api_does() {
        assert_eq!(normalize_road("how is the traffic on the a 5").as_deref(), Some("A5"));
        assert_eq!(normalize_road("stau auf der a8").as_deref(), Some("A8"));
        assert_eq!(normalize_road("autobahn 81").as_deref(), Some("A81"));
        assert_eq!(normalize_road("the A-99").as_deref(), Some("A99"));
        assert_eq!(normalize_road("A64a").as_deref(), Some("A64a"));
        assert_eq!(normalize_road("traffic near me"), None);
        assert_eq!(parse_roads(&json!({"roads": ["A1", "A2", " "]})), vec!["A1".to_string(), "A2".to_string()]);
    }

    #[test]
    fn answers_are_spoken_plainly() {
        let demo = demo_incidents();
        let a8: Vec<Incident> = demo.iter().filter(|i| i.road == "A8").cloned().collect();
        let (display, spoken) = road_answer("A8", &a8);
        assert!(display.contains("JAM A8"), "{display}");
        assert_eq!(spoken, "On the A8, Sir: a 4 km jam between Leonberg and Stuttgart-Möhringen towards München, costing about 12 minutes.");
        let (_, clear) = road_answer("A7", &[]);
        assert_eq!(clear, "The A7 is clear, Sir: no jams, accidents or closures reported.");
        // Near Stuttgart: the jam and the accident, not the roadworks or what is far away.
        let found = near(&demo, 48.7758, 9.1829, NEAR_KM);
        assert_eq!(found.iter().map(|(i, _)| i.id.as_str()).collect::<Vec<_>>(), vec!["demo-a81-accident", "demo-a8-jam"]);
        let (_, spoken) = area_answer("Stuttgart", &found, NEAR_KM, true);
        assert!(spoken.starts_with("Near Stuttgart, Sir: an accident on the A81 between Böblingen-Hulb and Sindelfingen-Ost towards Stuttgart, costing about 8 minutes; and a 4 km jam on the A8"), "{spoken}");
        assert_eq!(translate("- Gegenstände auf der Fahrbahn").as_deref(), Some("debris on the road"));
        assert!(in_germany(48.78, 9.18) && !in_germany(51.5, -0.12));
    }

    #[test]
    fn lines_are_thinned() {
        let pts: Vec<(f64, f64)> = (0..500).map(|i| (i as f64, 0.0)).collect();
        let thin = thin(pts, 48);
        assert_eq!(thin.len(), 48);
        assert_eq!((thin[0].0, thin[47].0), (0.0, 499.0));
    }
}
