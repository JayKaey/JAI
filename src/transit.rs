//! Trains: the live DEPARTURES board ("when's my next train?", "departures from Karlsruhe Hbf",
//! "Wann fährt meine Bahn?") and the moving trains on the Earth.
//!
//! Germany (and most of Europe) comes from Transitous, the community-run MOTIS journey planner
//! fed by the open timetable and real-time feeds (DELFI, the regional transport associations,
//! …): stops, live departures with delays, platforms and cancellations, the trips themselves and
//! the trains between their stops for a map box. Finland comes from Digitraffic's live GPS
//! positions. No key, no account. (The db.transport.rest mirror is gone and bahn.de refuses
//! programs; Transitous is the open replacement.)
//!
//! The parsers, the polyline decoder and the sentences are pure (tested at the end). The board's
//! state lives here; `gui_departures.rs` draws it and refreshes it every 30 s while it is open.

use crate::crash_guard::lock;
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

pub const TRANSITOUS: &str = "https://api.transitous.org/api/v1";
pub const FINLAND_URL: &str = "https://rata.digitraffic.fi/api/v1/train-locations/latest";
/// How often the open board asks again.
pub const REFRESH: Duration = Duration::from_secs(30);

/// Trains Sir would call trains (Stuttgart's S-Bahn comes as METRO).
pub const TRAIN_MODES: &[&str] = &["HIGHSPEED_RAIL", "LONG_DISTANCE", "NIGHT_RAIL", "REGIONAL_FAST_RAIL", "REGIONAL_RAIL", "RAIL", "SUBURBAN", "METRO"];
/// Everything on rails (the Stadtbahn comes as SUBWAY).
pub const RAIL_MODES: &[&str] = &["HIGHSPEED_RAIL", "LONG_DISTANCE", "NIGHT_RAIL", "REGIONAL_FAST_RAIL", "REGIONAL_RAIL", "RAIL", "SUBURBAN", "METRO", "SUBWAY", "TRAM"];

pub fn is_train(mode: &str) -> bool {
    TRAIN_MODES.contains(&mode)
}

pub fn is_rail(mode: &str) -> bool {
    RAIL_MODES.contains(&mode)
}

/// "S-Bahn", "regional", "tram" for the board.
pub fn mode_label(mode: &str) -> &'static str {
    match mode {
        "HIGHSPEED_RAIL" => "high-speed",
        "LONG_DISTANCE" => "long-distance",
        "NIGHT_RAIL" => "night train",
        "REGIONAL_FAST_RAIL" | "REGIONAL_RAIL" | "RAIL" => "regional",
        "SUBURBAN" | "METRO" => "S-Bahn",
        "SUBWAY" => "U-Bahn",
        "TRAM" => "tram",
        "BUS" => "bus",
        "COACH" => "coach",
        "FERRY" => "ferry",
        _ => "",
    }
}

// ------------------------------------------------------------------------------ stops ----

#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub modes: Vec<String>,
    pub importance: f64,
}

impl Stop {
    pub fn has_trains(&self) -> bool {
        self.modes.iter().any(|m| is_train(m))
    }
}

/// The geocoder's and the map's stops (`/geocode`, `/reverse-geocode`, `/map/stops`). A
/// platform is asked for through its station (`parentId`).
pub fn parse_stops(v: &Value) -> Vec<Stop> {
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter(|s| s["type"].as_str().is_none_or(|t| t == "STOP"))
        .filter_map(|s| {
            let id = s["parentId"].as_str().or(s["id"].as_str()).or(s["stopId"].as_str())?.to_string();
            Some(Stop {
                id,
                name: s["name"].as_str()?.trim().to_string(),
                lat: s["lat"].as_f64()?,
                lon: s["lon"].as_f64()?,
                modes: s["modes"].as_array().map(|m| m.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                importance: s["importance"].as_f64().unwrap_or(0.0),
            })
        })
        .collect()
}

/// The station Sir means by "my train" near a point: the most important train station within a
/// kilometre of the nearest one (so the main station wins over a halt next door).
pub fn home_station(stops: &[Stop], lat: f64, lon: f64) -> Option<Stop> {
    let with_trains: Vec<&Stop> = stops.iter().filter(|s| s.has_trains()).collect();
    let pool: Vec<&Stop> = if with_trains.is_empty() { stops.iter().filter(|s| s.modes.iter().any(|m| is_rail(m))).collect() } else { with_trains };
    let pool: Vec<&Stop> = if pool.is_empty() { stops.iter().collect() } else { pool };
    let dist = |s: &Stop| crate::live_layers::km((lat, lon), (s.lat, s.lon));
    let nearest = pool.iter().map(|s| dist(s)).fold(f64::MAX, f64::min);
    pool.into_iter().filter(|s| dist(s) <= nearest + 1.0).max_by(|a, b| a.importance.total_cmp(&b.importance)).cloned()
}

/// A named station from the geocoder: the first with trains among the first five, else the first.
pub fn named_station(stops: &[Stop]) -> Option<Stop> {
    stops.iter().take(5).find(|s| s.has_trains()).or(stops.first()).cloned()
}

// ------------------------------------------------------------------------- departures ----

#[derive(Debug, Clone, PartialEq)]
pub struct Departure {
    pub line: String,
    pub mode: String,
    pub headsign: String,
    pub planned: DateTime<Utc>,
    pub expected: DateTime<Utc>,
    /// The time comes from a live feed.
    pub realtime: bool,
    pub track: Option<String>,
    pub planned_track: Option<String>,
    pub cancelled: bool,
    pub trip_id: String,
    pub colour: Option<[u8; 3]>,
    pub agency: String,
}

impl Departure {
    pub fn delay_min(&self) -> i64 {
        (self.expected - self.planned).num_minutes()
    }

    pub fn platform_changed(&self) -> bool {
        matches!((&self.track, &self.planned_track), (Some(t), Some(p)) if t != p)
    }

    /// "cancelled", "+5", "on time", "" (no live data).
    pub fn status(&self) -> String {
        if self.cancelled {
            "cancelled".into()
        } else if !self.realtime {
            String::new()
        } else if self.delay_min() >= 1 {
            format!("+{}", self.delay_min())
        } else {
            "on time".into()
        }
    }
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|t| t.with_timezone(&Utc))
}

fn hex_colour(v: &Value) -> Option<[u8; 3]> {
    let s = v.as_str()?.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

/// "S11 Neureut Kirchfeld" with line "S11" → "Neureut Kirchfeld".
fn clean_headsign(line: &str, headsign: &str) -> String {
    let h = headsign.trim();
    h.strip_prefix(line).map(str::trim).filter(|r| !r.is_empty()).unwrap_or(h).to_string()
}

/// `/stoptimes`: the departures in the order they leave.
pub fn parse_stoptimes(v: &Value) -> Vec<Departure> {
    let Some(items) = v["stopTimes"].as_array() else { return Vec::new() };
    let mut out: Vec<Departure> = items
        .iter()
        .filter_map(|s| {
            let p = &s["place"];
            let planned = time(&p["scheduledDeparture"]).or_else(|| time(&p["departure"]))?;
            let expected = time(&p["departure"]).unwrap_or(planned);
            let line = [&s["displayName"], &s["routeShortName"], &s["tripShortName"]].iter().filter_map(|x| x.as_str()).map(str::trim).find(|x| !x.is_empty()).unwrap_or("").to_string();
            let mode = s["mode"].as_str().unwrap_or_default().to_string();
            let line = if line.is_empty() { mode_label(&mode).to_string() } else { line };
            Some(Departure {
                headsign: clean_headsign(&line, s["headsign"].as_str().unwrap_or_default()),
                line,
                mode,
                planned,
                expected,
                realtime: s["realTime"].as_bool().unwrap_or(false),
                track: p["track"].as_str().map(str::to_string).filter(|t| !t.is_empty()),
                planned_track: p["scheduledTrack"].as_str().map(str::to_string).filter(|t| !t.is_empty()),
                cancelled: [&p["cancelled"], &s["cancelled"], &s["tripCancelled"]].iter().any(|c| c.as_bool() == Some(true)),
                trip_id: s["tripId"].as_str().unwrap_or_default().to_string(),
                colour: hex_colour(&s["routeColor"]),
                agency: s["agencyName"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect();
    out.sort_by_key(|d| d.planned);
    out
}

/// What the board lists: trains and trams first; buses only where nothing runs on rails.
pub fn board_rows(all: &[Departure]) -> Vec<Departure> {
    let rail: Vec<Departure> = all.iter().filter(|d| is_rail(&d.mode)).cloned().collect();
    if rail.len() >= 3 {
        rail
    } else {
        all.to_vec()
    }
}

/// The departure "my next train" means: the next train still running, else the next on rails,
/// else the next of anything. (index into `rows`).
pub fn next_train(rows: &[Departure], now: DateTime<Utc>) -> Option<usize> {
    let upcoming = |d: &Departure| d.expected >= now - chrono::Duration::seconds(30) && !d.cancelled;
    rows.iter().position(|d| upcoming(d) && is_train(&d.mode)).or_else(|| rows.iter().position(|d| upcoming(d) && is_rail(&d.mode))).or_else(|| rows.iter().position(upcoming))
}

fn clock(t: DateTime<Utc>, tz: &FixedOffset) -> String {
    t.with_timezone(tz).format("%H:%M").to_string()
}

/// The local offset now.
pub fn local_offset() -> FixedOffset {
    *chrono::Local::now().offset()
}

fn platform_words(d: &Departure) -> String {
    match (&d.track, &d.planned_track) {
        (Some(t), Some(p)) if t != p => format!(" from platform {t} instead of {p}"),
        (Some(t), _) => format!(" from platform {t}"),
        _ => String::new(),
    }
}

fn status_words(d: &Departure) -> String {
    if !d.realtime {
        return String::new();
    }
    match d.delay_min() {
        m if m >= 2 => format!(", running {m} minutes late"),
        1 => ", a minute late".into(),
        _ => ", on time".into(),
    }
}

fn kind_word(mode: &str) -> &'static str {
    match mode {
        "TRAM" => "tram",
        "SUBWAY" => "U-Bahn",
        "BUS" => "bus",
        "COACH" => "coach",
        "FERRY" => "ferry",
        _ => "train",
    }
}

/// "The next train from Stuttgart Hbf is the S2 to Schorndorf at 23:38, in 4 minutes, from
/// platform 102, running 3 minutes late, Sir."
pub fn next_answer(stop: &str, rows: &[Departure], now: DateTime<Utc>, tz: &FixedOffset) -> String {
    let Some(i) = next_train(rows, now) else {
        return format!("Nothing more is leaving {stop} at the moment, Sir.");
    };
    let d = &rows[i];
    let mins = (d.expected - now).num_minutes().max(0);
    let when = match mins {
        0 => ", leaving now".to_string(),
        1 => ", in a minute".to_string(),
        m if m < 90 => format!(", in {m} minutes"),
        _ => String::new(),
    };
    let mut line = format!("The next {} from {stop} is the {} to {} at {}{when}{}{}", kind_word(&d.mode), said_line(&d.line), d.headsign, clock(d.planned, tz), platform_words(d), status_words(d));
    // A cancelled one before it is worth knowing.
    if let Some(c) = rows[..i].iter().rev().find(|c| c.cancelled && is_train(&c.mode) && c.planned >= now - chrono::Duration::minutes(2)) {
        line.push_str(&format!("; the {} to {} at {} is cancelled", said_line(&c.line), c.headsign, clock(c.planned, tz)));
    }
    line.push_str(", Sir.");
    line
}

/// "RB26 (17109)" → "RB26": the trip number stays on the board, out of his sentence.
fn said_line(line: &str) -> &str {
    match line.rsplit_once(" (") {
        Some((name, num)) if !name.trim().is_empty() && num.trim_end_matches(')').chars().all(|c| c.is_ascii_digit()) => name.trim(),
        _ => line,
    }
}

/// The board for the chat: one line per departure.
pub fn board_text(stop: &str, rows: &[Departure], tz: &FixedOffset) -> String {
    let mut out = format!("Departures · {stop}");
    for d in rows.iter().take(12) {
        let status = d.status();
        out.push_str(&format!(
            "\n{}  {:<8} {}{}{}",
            clock(d.planned, tz),
            d.line,
            d.headsign,
            d.track.as_ref().map(|t| format!(" · Pl. {t}")).unwrap_or_default(),
            if status.is_empty() { String::new() } else { format!(" · {status}") }
        ));
    }
    if rows.is_empty() {
        out.push_str("\nNo departures in the next hours");
    }
    out.push_str("\nSource: Transitous (open timetable and real-time data)");
    out
}

// --------------------------------------------------------------------- trains on a map ----

/// Google's encoded polyline at `precision` decimals → (lat, lon).
pub fn decode_polyline(s: &str, precision: u32) -> Vec<(f64, f64)> {
    let bytes = s.as_bytes();
    let scale = 10f64.powi(precision as i32);
    let (mut i, mut lat, mut lon) = (0usize, 0i64, 0i64);
    let mut out = Vec::new();
    'points: while i < bytes.len() {
        let mut pair = [0i64; 2];
        for value in &mut pair {
            let (mut shift, mut result) = (0u32, 0i64);
            loop {
                let Some(&b) = bytes.get(i) else { break 'points };
                i += 1;
                let b = b as i64 - 63;
                result |= (b & 0x1f) << shift;
                shift += 5;
                if b < 0x20 || shift > 60 {
                    break;
                }
            }
            *value = if result & 1 == 1 { !(result >> 1) } else { result >> 1 };
        }
        lat += pair[0];
        lon += pair[1];
        out.push((lat as f64 / scale, lon as f64 / scale));
    }
    out
}

/// One train between two stops (`/map/trips`).
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub trip_id: String,
    pub line: String,
    pub mode: String,
    pub from: String,
    pub to: String,
    pub departs: DateTime<Utc>,
    pub arrives: DateTime<Utc>,
    pub path: Vec<(f64, f64)>,
    pub colour: Option<[u8; 3]>,
    pub realtime: bool,
}

impl Segment {
    /// Where it is at `t` (by time along the path's length), if it is on this stretch then.
    pub fn at(&self, t: DateTime<Utc>) -> Option<(f64, f64)> {
        if t < self.departs || t > self.arrives || self.path.is_empty() {
            return None;
        }
        let total = (self.arrives - self.departs).num_milliseconds().max(1) as f64;
        let f = ((t - self.departs).num_milliseconds() as f64 / total).clamp(0.0, 1.0);
        Some(along(&self.path, f))
    }
}

/// The point a fraction `f` of the way along a line.
pub fn along(path: &[(f64, f64)], f: f64) -> (f64, f64) {
    if path.len() < 2 {
        return path.first().copied().unwrap_or((0.0, 0.0));
    }
    let lengths: Vec<f64> = path.windows(2).map(|w| crate::live_layers::km(w[0], w[1])).collect();
    let total: f64 = lengths.iter().sum();
    if total <= 0.0 {
        return path[0];
    }
    let mut left = f.clamp(0.0, 1.0) * total;
    for (k, len) in lengths.iter().enumerate() {
        if left <= *len || k == lengths.len() - 1 {
            let t = if *len > 0.0 { (left / len).clamp(0.0, 1.0) } else { 0.0 };
            let (a, b) = (path[k], path[k + 1]);
            return (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        }
        left -= len;
    }
    *path.last().unwrap_or(&path[0])
}

/// `/map/trips`: the rail segments (buses, coaches and planes are left out).
pub fn parse_map_trips(v: &Value, precision: u32) -> Vec<Segment> {
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|s| {
            let mode = s["mode"].as_str().unwrap_or_default();
            if !is_rail(mode) {
                return None;
            }
            let trip = s["trips"].as_array()?.first()?;
            let mut path = decode_polyline(s["polyline"].as_str().unwrap_or_default(), precision);
            if path.len() > 400 {
                let step = path.len().div_ceil(400);
                let last = *path.last()?;
                path = path.into_iter().step_by(step).collect();
                path.push(last);
            }
            if path.is_empty() {
                path = vec![(s["from"]["lat"].as_f64()?, s["from"]["lon"].as_f64()?), (s["to"]["lat"].as_f64()?, s["to"]["lon"].as_f64()?)];
            }
            Some(Segment {
                trip_id: trip["tripId"].as_str().unwrap_or_default().to_string(),
                line: trip["routeShortName"].as_str().or(trip["displayName"].as_str()).unwrap_or(mode_label(mode)).trim().to_string(),
                mode: mode.to_string(),
                from: s["from"]["name"].as_str().unwrap_or_default().to_string(),
                to: s["to"]["name"].as_str().unwrap_or_default().to_string(),
                departs: time(&s["departure"])?,
                arrives: time(&s["arrival"])?,
                path,
                colour: hex_colour(&s["routeColor"]),
                realtime: s["realTime"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

/// The URL for the trains in a box over the next few minutes.
pub fn map_trips_url(south: f64, west: f64, north: f64, east: f64, zoom: u32, from: DateTime<Utc>, minutes: i64) -> String {
    let fmt = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    format!("{TRANSITOUS}/map/trips?zoom={zoom}&min={south:.4},{west:.4}&max={north:.4},{east:.4}&startTime={}&endTime={}&precision=4", fmt(from), fmt(from + chrono::Duration::minutes(minutes)))
}

/// One Finnish train from Digitraffic.
#[derive(Debug, Clone, PartialEq)]
pub struct FiTrain {
    pub number: i64,
    pub lat: f64,
    pub lon: f64,
    pub speed_kmh: f64,
    pub at: Option<DateTime<Utc>>,
}

pub fn parse_fi(v: &Value) -> Vec<FiTrain> {
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|t| {
            let c = t["location"]["coordinates"].as_array()?;
            Some(FiTrain { number: t["trainNumber"].as_i64()?, lon: c.first()?.as_f64()?, lat: c.get(1)?.as_f64()?, speed_kmh: t["speed"].as_f64().unwrap_or(0.0), at: time(&t["timestamp"]) })
        })
        .collect()
}

// ------------------------------------------------------------------ "where is that train" ----

/// A stop of a trip with its times.
#[derive(Debug, Clone, PartialEq)]
pub struct TripStop {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub arrives: Option<DateTime<Utc>>,
    pub departs: Option<DateTime<Utc>>,
}

/// `/trip`: the stops in order (the first leg's from, its intermediate stops, its to).
pub fn parse_trip(v: &Value) -> Vec<TripStop> {
    let Some(leg) = v["legs"].as_array().and_then(|l| l.first()) else { return Vec::new() };
    let stop = |p: &Value| -> Option<TripStop> {
        Some(TripStop { name: p["name"].as_str()?.to_string(), lat: p["lat"].as_f64()?, lon: p["lon"].as_f64()?, arrives: time(&p["arrival"]), departs: time(&p["departure"]) })
    };
    let mut out: Vec<TripStop> = stop(&leg["from"]).into_iter().collect();
    if let Some(mid) = leg["intermediateStops"].as_array() {
        out.extend(mid.iter().filter_map(stop));
    }
    out.extend(stop(&leg["to"]));
    out
}

/// Where a train is on its trip at `t`: (lat, lon, words: "between Pasing and Augsburg",
/// "at Ulm Hbf", "not left München Hbf yet", "arrived in Berlin").
pub fn trip_position(stops: &[TripStop], t: DateTime<Utc>) -> Option<(f64, f64, String)> {
    let first = stops.first()?;
    let last = stops.last()?;
    if first.departs.is_some_and(|d| t < d) {
        return Some((first.lat, first.lon, format!("hasn't left {} yet", first.name)));
    }
    for (k, s) in stops.iter().enumerate() {
        if let (Some(a), Some(d)) = (s.arrives, s.departs) {
            if t >= a && t <= d {
                return Some((s.lat, s.lon, format!("is at {}", s.name)));
            }
        }
        if let Some(next) = stops.get(k + 1) {
            if let (Some(d), Some(a)) = (s.departs, next.arrives) {
                if t >= d && t <= a {
                    let f = (t - d).num_milliseconds() as f64 / (a - d).num_milliseconds().max(1) as f64;
                    let (lat, lon) = crate::live_layers::great_circle((s.lat, s.lon), (next.lat, next.lon), f.clamp(0.0, 1.0));
                    return Some((lat, lon, format!("is between {} and {}", s.name, next.name)));
                }
            }
        }
    }
    Some((last.lat, last.lon, format!("has arrived in {}", last.name)))
}

// ------------------------------------------------------------------------- the board ----

#[derive(Debug, Clone, Default)]
pub struct Board {
    pub stop: Option<Stop>,
    /// What Sir named ("Karlsruhe Hbf"), or None for the station near him.
    pub asked: Option<String>,
    pub rows: Vec<Departure>,
    pub fetched: Option<Instant>,
    /// "23:06:12".
    pub updated: String,
    pub error: Option<String>,
    pub loading: bool,
    /// The row clicked (or the one JAI named): "where is that train?" means it.
    pub selected: Option<usize>,
    pub demo: bool,
}

impl Board {
    pub fn selected_row(&self) -> Option<&Departure> {
        self.selected.and_then(|i| self.rows.get(i))
    }
}

static BOARD: LazyLock<Mutex<Board>> = LazyLock::new(|| Mutex::new(Board::default()));
#[cfg(not(test))]
static OPEN: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
thread_local!(static OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
static FETCHING: AtomicBool = AtomicBool::new(false);

pub fn board() -> MutexGuard<'static, Board> {
    lock(&BOARD)
}

fn set_open(on: bool) {
    #[cfg(not(test))]
    OPEN.store(on, Ordering::Release);
    #[cfg(test)]
    OPEN.with(|o| o.set(on));
}

pub fn is_open() -> bool {
    #[cfg(not(test))]
    {
        OPEN.load(Ordering::Acquire)
    }
    #[cfg(test)]
    {
        OPEN.with(|o| o.get())
    }
}

pub fn close() {
    set_open(false);
}

/// Picks a row (a click on the board).
pub fn select(i: usize) {
    let mut b = board();
    if i < b.rows.len() {
        b.selected = Some(i);
    }
}

/// The trip "that train" means: the selected row, else the next train on the board.
pub fn focused() -> Option<Departure> {
    let b = board();
    b.selected_row().cloned().or_else(|| next_train(&b.rows, Utc::now()).and_then(|i| b.rows.get(i).cloned()))
}

/// Refreshes the open board every 30 s (called by the block each frame).
pub fn schedule(rt: &Handle) {
    if !is_open() {
        return;
    }
    let due = {
        let b = board();
        !b.demo && b.stop.is_some() && b.fetched.is_none_or(|t| t.elapsed() >= REFRESH)
    };
    if due && !FETCHING.swap(true, Ordering::AcqRel) {
        rt.spawn(async {
            let stop = board().stop.clone();
            if let Some(stop) = stop {
                let result = departures(&stop.id).await;
                store(stop, result);
            }
            FETCHING.store(false, Ordering::Release);
            crate::gui::refresh();
        });
    }
}

/// Fetches the board again now ("refresh").
pub fn refresh_now() {
    board().fetched = None;
}

fn store(stop: Stop, result: Result<Vec<Departure>, String>) {
    let mut b = board();
    // (Sir asked for another station meanwhile.)
    if b.stop.as_ref().is_some_and(|s| s.id != stop.id) {
        return;
    }
    let keep = b.selected_row().map(|d| d.trip_id.clone());
    match result {
        Ok(all) => {
            b.rows = board_rows(&all);
            b.error = None;
            b.selected = keep.and_then(|id| b.rows.iter().position(|d| d.trip_id == id));
        }
        Err(e) => b.error = Some(e),
    }
    b.stop = Some(stop);
    b.fetched = Some(Instant::now());
    b.updated = chrono::Local::now().format("%H:%M:%S").to_string();
    b.loading = false;
}

// ---------------------------------------------------------------------------- fetches ----

async fn departures(stop_id: &str) -> Result<Vec<Departure>, String> {
    let url = format!("{TRANSITOUS}/stoptimes?stopId={}&n=40", urlencoding::encode(stop_id));
    Ok(parse_stoptimes(&crate::live_layers::get_json(&url).await?))
}

/// A station by name (biased towards Sir when he is known).
pub async fn find_station(name: &str, near: Option<(f64, f64)>) -> Result<Stop, String> {
    let bias = near.map(|(lat, lon)| format!("&place={lat:.4},{lon:.4}")).unwrap_or_default();
    let url = format!("{TRANSITOUS}/geocode?text={}&type=STOP{bias}", urlencoding::encode(name.trim()));
    named_station(&parse_stops(&crate::live_layers::get_json(&url).await?)).ok_or_else(|| format!("I couldn't find a station called '{name}', Sir."))
}

/// The station near a point.
pub async fn station_near(lat: f64, lon: f64) -> Result<Stop, String> {
    for half in [0.03, 0.08] {
        let url = format!("{TRANSITOUS}/map/stops?min={:.4},{:.4}&max={:.4},{:.4}", lat - half, lon - half * 1.5, lat + half, lon + half * 1.5);
        let stops = parse_stops(&crate::live_layers::get_json(&url).await?);
        if let Some(s) = home_station(&stops, lat, lon).filter(|s| s.has_trains() || half > 0.05) {
            return Ok(s);
        }
    }
    Err("I couldn't find a station near you, Sir.".into())
}

/// The trains in a box (rail only).
pub async fn trains_in(south: f64, west: f64, north: f64, east: f64, zoom: u32) -> Result<Vec<Segment>, String> {
    let url = map_trips_url(south, west, north, east, zoom, Utc::now(), 4);
    Ok(parse_map_trips(&crate::live_layers::get_json(&url).await?, 4))
}

pub async fn finland() -> Result<Vec<FiTrain>, String> {
    Ok(parse_fi(&crate::live_layers::get_json(FINLAND_URL).await?))
}

/// Where a trip's train is now.
pub async fn locate_trip(trip_id: &str) -> Result<(f64, f64, String), String> {
    let url = format!("{TRANSITOUS}/trip?tripId={}", urlencoding::encode(trip_id));
    let stops = parse_trip(&crate::live_layers::get_json(&url).await?);
    trip_position(&stops, Utc::now()).ok_or_else(|| "That trip has no stops I can place, Sir.".to_string())
}

// ----------------------------------------------------------------------------- the tool ----

/// `open_jai_view` with view `departures`: opens the board for `station` (or the station
/// near Sir), waits for the first answer, and says the next train.
pub async fn run(_ctx: &crate::tools::ToolContext, args: &Value) -> Result<String, String> {
    open_board(args).await
}

/// Opens (or refreshes) the board; the tool's body, also used by the Earth's "that train".
pub async fn open_board(args: &Value) -> Result<String, String> {
    let action = crate::tools::str_arg(args, &["action"]).unwrap_or("show").to_lowercase();
    if action == "refresh" {
        refresh_now();
        set_open(true);
        crate::gui::refresh();
        return Ok("Refreshing the departures\nSpoken: Refreshing the board, Sir.".into());
    }
    let asked = crate::tools::str_arg(args, &["station", "name", "place", "stop", "from"]).map(str::trim).filter(|s| !s.is_empty() && !matches!(s.to_lowercase().as_str(), "here" | "hier" | "me" | "my station")).map(str::to_string);
    let tz = local_offset();
    let now = Utc::now();
    if crate::live_layers::offline() {
        let stop = demo_stop(asked.as_deref());
        let rows = demo_departures(now);
        let spoken = next_answer(&stop.name, &rows, now, &tz);
        let display = board_text(&stop.name, &rows, &tz);
        {
            let mut b = board();
            *b = Board { selected: next_train(&rows, now), stop: Some(stop), asked, rows, fetched: Some(Instant::now()), updated: chrono::Local::now().format("%H:%M:%S").to_string(), demo: true, ..Board::default() };
        }
        set_open(true);
        crate::gui::refresh();
        return Ok(crate::features::spoken(display, spoken));
    }
    // Open at once with "searching…"; the answer follows.
    {
        let mut b = board();
        if b.asked != asked || b.stop.is_none() {
            *b = Board { asked: asked.clone(), loading: true, ..Board::default() };
        }
    }
    set_open(true);
    crate::gui::refresh();
    let here = crate::live_layers::here().await.ok();
    let stop = match &asked {
        Some(name) => find_station(name, here.as_ref().map(|h| (h.lat, h.lon))).await,
        None => match &here {
            Some(h) => station_near(h.lat, h.lon).await,
            None => Err("I don't know where you are, Sir. Name a station, or tell me your home address.".into()),
        },
    };
    let stop = match stop {
        Ok(s) => s,
        Err(e) => {
            let mut b = board();
            b.loading = false;
            b.error = Some(e.clone());
            return Err(e);
        }
    };
    board().stop = Some(stop.clone());
    let result = departures(&stop.id).await;
    let failed = result.as_ref().err().cloned();
    store(stop.clone(), result);
    if let Some(e) = failed {
        return Err(format!("The departures for {} didn't come through: {e}", stop.name));
    }
    let (spoken, display) = {
        let mut b = board();
        let next = next_train(&b.rows, now);
        b.selected = next;
        (next_answer(&stop.name, &b.rows, now, &tz), board_text(&stop.name, &b.rows, &tz))
    };
    crate::hud_dock::describe_block("departures", &display);
    crate::gui::refresh();
    Ok(crate::features::spoken(display, spoken))
}

// ------------------------------------------------------------------------------- demo ----

fn demo_stop(asked: Option<&str>) -> Stop {
    Stop {
        id: "demo".into(),
        name: asked.map(str::to_string).unwrap_or_else(|| "Stuttgart Hbf".into()),
        lat: 48.7848,
        lon: 9.1832,
        modes: vec!["REGIONAL_RAIL".into(), "METRO".into()],
        importance: 0.01,
    }
}

/// A plausible board for the demo and the tests (made-up times around `now`).
pub fn demo_departures(now: DateTime<Utc>) -> Vec<Departure> {
    let at = |m: i64| now + chrono::Duration::minutes(m);
    let row = |line: &str, mode: &str, to: &str, m: i64, late: i64, track: &str, cancelled: bool| Departure {
        line: line.into(),
        mode: mode.into(),
        headsign: to.into(),
        planned: at(m),
        expected: at(m + late),
        realtime: true,
        track: Some(track.into()),
        planned_track: Some(track.into()),
        cancelled,
        trip_id: format!("demo-{line}-{m}"),
        colour: None,
        agency: "Demo".into(),
    };
    vec![
        row("S2", "METRO", "Schorndorf", 3, 2, "102", false),
        row("RE 5", "REGIONAL_RAIL", "Friedrichshafen", 6, 0, "9", true),
        row("S4", "METRO", "Marbach (N)", 7, 0, "102", false),
        row("U6", "SUBWAY", "Gerlingen", 9, 0, "3", false),
        row("ICE 594", "HIGHSPEED_RAIL", "Berlin Hbf", 14, 6, "16", false),
        row("RB 17a", "REGIONAL_RAIL", "Pforzheim Hbf", 21, 0, "4", false),
    ]
}

/// The tool's arguments for "show me that train on the Earth".
pub fn show_train_args() -> Value {
    json!({ "view": "earth", "action": "show_train" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn polylines_decode_at_both_precisions() {
        // Google's own example at precision 5.
        let pts = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 5);
        assert_eq!(pts.len(), 3);
        assert!((pts[0].0 - 38.5).abs() < 1e-9 && (pts[0].1 + 120.2).abs() < 1e-9);
        assert!((pts[2].0 - 43.252).abs() < 1e-9 && (pts[2].1 + 126.453).abs() < 1e-9);
        // The same digits read at precision 4 are ten times larger.
        let p4 = decode_polyline("_p~iF~ps|U", 4);
        assert!((p4[0].0 - 385.0).abs() < 1e-9);
        // Truncated input stops cleanly.
        assert_eq!(decode_polyline("_p~iF~ps", 5).len(), 0);
    }

    #[test]
    fn departures_are_read_with_delays_platforms_and_cancellations() {
        let v = json!({"stopTimes": [
            {"place": {"name": "Karlsruhe Hbf", "departure": "2026-11-08T23:24:00Z", "scheduledDeparture": "2026-11-08T23:21:00Z", "track": "3", "scheduledTrack": "2", "cancelled": false},
             "mode": "REGIONAL_RAIL", "realTime": true, "headsign": "S11 Neureut Kirchfeld", "routeShortName": "S11", "displayName": "S11", "tripId": "t-s11", "routeColor": "00975f", "agencyName": "AVG"},
            {"place": {"name": "Karlsruhe Hbf", "departure": "2026-11-08T23:05:00Z", "scheduledDeparture": "2026-11-08T23:05:00Z", "cancelled": false},
             "mode": "COACH", "realTime": true, "headsign": "Zagreb (Bus Station)", "displayName": "FlixBus N880", "tripId": "t-flix"},
            {"place": {"name": "Karlsruhe Hbf", "scheduledDeparture": "2026-11-08T23:10:00Z"},
             "mode": "HIGHSPEED_RAIL", "realTime": false, "headsign": "Basel SBB", "displayName": "ICE 71", "tripId": "t-ice", "tripCancelled": true}
        ]});
        let d = parse_stoptimes(&v);
        assert_eq!(d.iter().map(|x| x.line.as_str()).collect::<Vec<_>>(), vec!["FlixBus N880", "ICE 71", "S11"]);
        let s11 = &d[2];
        assert_eq!((s11.headsign.as_str(), s11.delay_min(), s11.platform_changed(), s11.colour), ("Neureut Kirchfeld", 3, true, Some([0x00, 0x97, 0x5f])));
        assert_eq!(s11.status(), "+3");
        assert!(d[1].cancelled);
        assert_eq!(d[1].status(), "cancelled");
        let tz = FixedOffset::east_opt(3600).unwrap();
        let now = utc("2026-11-08T23:00:00Z");
        assert_eq!(next_train(&d, now), Some(2));
        assert_eq!(
            next_answer("Karlsruhe Hbf", &d, now, &tz),
            "The next train from Karlsruhe Hbf is the S11 to Neureut Kirchfeld at 00:21, in 24 minutes from platform 3 instead of 2, running 3 minutes late; the ICE 71 to Basel SBB at 00:10 is cancelled, Sir."
        );
        assert!(board_text("Karlsruhe Hbf", &d, &tz).contains("00:21  S11      Neureut Kirchfeld · Pl. 3 · +3"));
    }

    #[test]
    fn the_board_prefers_rails_and_the_demo_has_a_next_train() {
        let now = utc("2026-11-08T23:00:00Z");
        let rows = demo_departures(now);
        assert_eq!(board_rows(&rows).len(), rows.len());
        let tz = FixedOffset::east_opt(3600).unwrap();
        let spoken = next_answer("Stuttgart Hbf", &rows, now, &tz);
        assert!(spoken.starts_with("The next train from Stuttgart Hbf is the S2 to Schorndorf at 00:03, in 5 minutes from platform 102, running 2 minutes late"), "{spoken}");
        let mut buses = rows.clone();
        for d in &mut buses {
            d.mode = "BUS".into();
        }
        assert_eq!(board_rows(&buses).len(), rows.len());
        assert!(next_answer("Halt", &buses, now, &tz).starts_with("The next bus"));
        assert_eq!(next_answer("Halt", &[], now, &tz), "Nothing more is leaving Halt at the moment, Sir.");
        // The trip number stays on the board.
        assert_eq!(said_line("RB26 (17109)"), "RB26");
        assert_eq!(said_line("ICE 71"), "ICE 71");
        assert_eq!(said_line("S2 (Flughafen)"), "S2 (Flughafen)");
    }

    #[test]
    fn stations_are_chosen_sensibly() {
        let v = json!([
            {"type": "STOP", "name": "Ulm Hbf", "id": "de-DELFI_de:08421:1_G", "lat": 48.3994, "lon": 9.9829, "modes": ["HIGHSPEED_RAIL", "LONG_DISTANCE", "REGIONAL_RAIL", "BUS"], "importance": 0.0023},
            {"type": "STOP", "name": "Ulm ZOB", "id": "eu-flixbus_x", "lat": 48.3991, "lon": 9.9832, "modes": ["LONG_DISTANCE", "BUS"], "importance": 0.0001},
            {"type": "ADDRESS", "name": "Ulmer Straße 1", "id": "a", "lat": 48.0, "lon": 9.9}
        ]);
        let stops = parse_stops(&v);
        assert_eq!(stops.len(), 2);
        assert_eq!(named_station(&stops).unwrap().name, "Ulm Hbf");
        // The map's platforms ask through their station; the main station beats a halt nearby.
        let map = json!([
            {"name": "Heidehofstraße", "stopId": "de-DELFI_de:08111:6122:3:4", "lat": 48.7792, "lon": 9.1901, "modes": ["BUS"], "importance": 0.0007},
            {"name": "Stuttgart Hbf", "stopId": "de-DELFI_de:08111:6115:1:1", "parentId": "de-DELFI_de:08111:6115", "lat": 48.7848, "lon": 9.1832, "modes": ["HIGHSPEED_RAIL", "REGIONAL_RAIL"], "importance": 0.0145},
            {"name": "Stadtmitte", "stopId": "de-DELFI_de:08111:6056:1:1", "lat": 48.7765, "lon": 9.1729, "modes": ["METRO"], "importance": 0.004}
        ]);
        let stops = parse_stops(&map);
        let home = home_station(&stops, 48.7795, 9.1785).unwrap();
        assert_eq!((home.name.as_str(), home.id.as_str()), ("Stuttgart Hbf", "de-DELFI_de:08111:6115"));
    }

    #[test]
    fn trains_move_along_their_stretch_and_their_trip() {
        let v = json!([
            {"trips": [{"tripId": "t1", "routeShortName": "ICE 698"}], "mode": "HIGHSPEED_RAIL", "from": {"name": "Ulm Hbf", "lat": 48.0, "lon": 9.0}, "to": {"name": "Stuttgart Hbf", "lat": 48.0, "lon": 10.0},
             "departure": "2026-11-08T23:00:00Z", "arrival": "2026-11-08T23:10:00Z", "realTime": true, "polyline": ""},
            {"trips": [{"tripId": "b1", "routeShortName": "42"}], "mode": "BUS", "from": {"name": "a", "lat": 1.0, "lon": 1.0}, "to": {"name": "b", "lat": 2.0, "lon": 2.0},
             "departure": "2026-11-08T23:00:00Z", "arrival": "2026-11-08T23:10:00Z", "polyline": ""}
        ]);
        let segs = parse_map_trips(&v, 4);
        assert_eq!(segs.len(), 1, "no buses on the map");
        let (lat, lon) = segs[0].at(utc("2026-11-08T23:05:00Z")).unwrap();
        assert!((lat - 48.0).abs() < 1e-9 && (lon - 9.5).abs() < 1e-6);
        assert_eq!(segs[0].at(utc("2026-11-08T23:11:00Z")), None);
        assert!(map_trips_url(48.7, 9.1, 48.85, 9.3, 12, utc("2026-11-08T23:40:00Z"), 4).ends_with("startTime=2026-11-08T23:40:00Z&endTime=2026-11-08T23:44:00Z&precision=4"));

        let trip = json!({"legs": [{"from": {"name": "München Hbf", "lat": 48.14, "lon": 11.56, "departure": "2026-11-08T20:53:00Z"},
            "intermediateStops": [{"name": "Pasing", "lat": 48.15, "lon": 11.46, "arrival": "2026-11-08T20:59:00Z", "departure": "2026-11-08T21:00:00Z"}],
            "to": {"name": "Augsburg Hbf", "lat": 48.37, "lon": 10.89, "arrival": "2026-11-08T21:30:00Z"}}]});
        let stops = parse_trip(&trip);
        assert_eq!(stops.len(), 3);
        assert_eq!(trip_position(&stops, utc("2026-11-08T20:40:00Z")).unwrap().2, "hasn't left München Hbf yet");
        assert_eq!(trip_position(&stops, utc("2026-11-08T20:59:30Z")).unwrap().2, "is at Pasing");
        let (lat, _, words) = trip_position(&stops, utc("2026-11-08T21:15:00Z")).unwrap();
        assert_eq!(words, "is between Pasing and Augsburg Hbf");
        assert!(lat > 48.15 && lat < 48.37);
        assert_eq!(trip_position(&stops, utc("2026-11-08T22:00:00Z")).unwrap().2, "has arrived in Augsburg Hbf");
    }

    #[test]
    fn finnish_trains_are_read() {
        let v = json!([{"accuracy": 0, "isGpsLocation": true, "location": {"type": "Point", "coordinates": [27.454185, 63.931725]}, "speed": 126, "trainNumber": 73, "departureDate": "2026-11-08", "timestamp": "2026-11-08T23:05:55.000Z"}]);
        let t = parse_fi(&v);
        assert_eq!((t[0].number, t[0].lat, t[0].lon, t[0].speed_kmh), (73, 63.931725, 27.454185, 126.0));
        assert!(t[0].at.is_some());
    }
}
