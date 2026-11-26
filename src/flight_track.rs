//! Following one flight on the Earth (Sir, 2026-11-22: "track a flight that goes from Frankfurt
//! to New York"): the aircraft near the route from adsb.lol's live feed, each callsign's route
//! from its standing data, then the one that flies it followed every few seconds. The feed comes
//! from receivers on the ground, so a plane far out over an ocean is out of its reach.

use serde_json::Value;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

/// One aircraft in the live feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Plane {
    pub hex: String,
    /// "DLH400" (empty when it sends none).
    pub callsign: String,
    pub lat: f64,
    pub lon: f64,
    /// Feet; None on the ground.
    pub alt_ft: Option<f64>,
    /// Knots over the ground.
    pub speed_kt: f64,
    /// Degrees from north.
    pub track: f64,
    /// Seconds since a receiver last heard its position (the feed's `seen_pos`).
    pub age: f64,
}

/// Where a callsign flies: its airports' codes ("FRA-JFK") and their (place, lat, lon).
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub codes: String,
    pub airports: Vec<(String, f64, f64)>,
}

const FEED: &str = "https://api.adsb.lol/v2";
const TIMEOUT: Duration = Duration::from_secs(10);

pub fn point_url(lat: f64, lon: f64, nm: u32) -> String {
    format!("{FEED}/point/{lat:.3}/{lon:.3}/{nm}")
}

pub fn hex_url(hex: &str) -> String {
    format!("{FEED}/hex/{hex}")
}

pub fn callsign_url(callsign: &str) -> String {
    format!("{FEED}/callsign/{callsign}")
}

pub fn route_url(callsign: &str) -> String {
    format!("https://vrs-standing-data.adsb.lol/routes/{}/{callsign}.json", callsign.get(..2).unwrap_or(callsign))
}

pub fn parse_planes(v: &Value) -> Vec<Plane> {
    let planes = v["ac"].as_array().into_iter().flatten();
    planes
        .filter_map(|a| {
            Some(Plane {
                hex: a["hex"].as_str()?.to_string(),
                callsign: a["flight"].as_str().unwrap_or_default().trim().to_uppercase(),
                lat: a["lat"].as_f64()?,
                lon: a["lon"].as_f64()?,
                // ("alt_baro" is the word "ground" on the ground.)
                alt_ft: a["alt_baro"].as_f64(),
                speed_kt: a["gs"].as_f64().unwrap_or(0.0),
                track: a["track"].as_f64().unwrap_or(0.0),
                age: a["seen_pos"].as_f64().unwrap_or(0.0),
            })
        })
        .collect()
}

pub fn parse_route(v: &Value) -> Option<Route> {
    let airports: Vec<(String, f64, f64)> = v["_airports"].as_array()?.iter().filter_map(|a| Some((a["location"].as_str().or(a["name"].as_str())?.to_string(), a["lat"].as_f64()?, a["lon"].as_f64()?))).collect();
    let codes = v["_airport_codes_iata"].as_str().or(v["airport_codes"].as_str()).unwrap_or_default().to_string();
    (airports.len() >= 2).then_some(Route { codes, airports })
}

/// The compass bearing from `a` to `b` (degrees from north), both (lat, lon).
pub fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (p1, p2, dl) = (a.0.to_radians(), b.0.to_radians(), (b.1 - a.1).to_radians());
    let (y, x) = (dl.sin() * p2.cos(), p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos());
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// The aircraft in the air with a callsign that head for `to`, the best aimed first, each once.
pub fn heading_for(planes: &[Plane], to: (f64, f64)) -> Vec<Plane> {
    let mut seen = HashSet::new();
    let mut found: Vec<(f64, Plane)> = planes
        .iter()
        .filter(|p| !p.callsign.is_empty() && p.alt_ft.is_some_and(|ft| ft > 3_000.0) && seen.insert(p.hex.clone()))
        .filter_map(|p| {
            let off = (p.track - bearing((p.lat, p.lon), to)).rem_euclid(360.0);
            let off = off.min(360.0 - off);
            (off < 25.0).then(|| (off, p.clone()))
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, p)| p).collect()
}

/// Whether a route leaves from near `from` and ends near `to` (its airports, 150 km).
pub fn flies(route: &Route, from: (f64, f64), to: (f64, f64)) -> bool {
    let near = |a: &(String, f64, f64), b: (f64, f64)| crate::earth::haversine_km((a.1, a.2), b) < 150.0;
    route.airports.first().is_some_and(|a| near(a, from)) && route.airports.last().is_some_and(|a| near(a, to))
}

/// "LH 400" → "DLH400": the airline's ICAO code, as the feed knows the callsign.
pub fn icao_callsign(said: &str) -> String {
    const AIRLINES: [(&str, &str); 18] = [
        ("LH", "DLH"), ("BA", "BAW"), ("AF", "AFR"), ("KL", "KLM"), ("UA", "UAL"), ("AA", "AAL"), ("DL", "DAL"), ("EK", "UAE"), ("LX", "SWR"),
        ("OS", "AUA"), ("EW", "EWG"), ("FR", "RYR"), ("TK", "THY"), ("QR", "QTR"), ("SQ", "SIA"), ("AC", "ACA"), ("IB", "IBE"), ("SK", "SAS"),
    ];
    let s: String = said.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_uppercase();
    let digits = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    match AIRLINES.iter().find(|(iata, _)| *iata == &s[..digits]) {
        Some((_, icao)) => format!("{icao}{}", &s[digits..]),
        None => s,
    }
}

/// "DLH400" as it is said: "DLH 400".
pub fn spoken(callsign: &str) -> String {
    let (airline, number) = callsign.split_at(callsign.find(|c: char| c.is_ascii_digit()).unwrap_or(callsign.len()));
    format!("{airline} {number}").trim().to_string()
}

/// Why the feed could not be asked, as it is said (the error itself goes to the log).
fn feed_down(e: impl std::fmt::Display) -> String {
    tracing::warn!("[FLIGHT] adsb.lol: {e}");
    "the live flight feed isn't answering".into()
}

/// The aircraft in the air now that flies from `from` to `to`, with its route; else why not,
/// as it is said ("none of the 40 aircraft along that route is heading that way").
pub async fn find(from: (f64, f64), to: (f64, f64)) -> Result<(Plane, Route), String> {
    let spots: Vec<String> = [0.03, 0.15, 0.3, 0.5, 0.7, 0.85, 0.97].iter().map(|t| crate::earth::great_circle(from, to, *t)).map(|(lat, lon)| point_url(lat, lon, 250)).collect();
    let feeds = futures_util::future::join_all(spots.iter().map(|url| crate::earth::get_json(url, TIMEOUT))).await;
    if let Some(Err(e)) = feeds.first().filter(|_| feeds.iter().all(Result::is_err)) {
        return Err(feed_down(e));
    }
    let planes: Vec<Plane> = feeds.iter().flatten().flat_map(parse_planes).collect();
    let near = planes.iter().map(|p| &p.hex).collect::<HashSet<_>>().len();
    if near == 0 {
        return Err("no receiver hears a single aircraft along that route; far out over an ocean they're beyond their reach".into());
    }
    let candidates: Vec<Plane> = heading_for(&planes, to).into_iter().take(30).collect();
    if candidates.is_empty() {
        return Err(format!("none of the {near} aircraft along that route is heading that way"));
    }
    let urls: Vec<String> = candidates.iter().map(|p| route_url(&p.callsign)).collect();
    let routes = futures_util::future::join_all(urls.iter().map(|url| crate::earth::get_json(url, TIMEOUT))).await;
    if let Some(Err(e)) = routes.first().filter(|_| routes.iter().all(Result::is_err)) {
        tracing::warn!("[FLIGHT] routes: {e}");
        return Err(format!("{} aircraft are heading that way, but the route database isn't answering, so I can't tell where they fly", candidates.len()));
    }
    let heading = candidates.len();
    candidates
        .into_iter()
        .zip(routes)
        .find_map(|(plane, route)| {
            let route = parse_route(route.as_ref().ok()?)?;
            flies(&route, from, to).then_some((plane, route))
        })
        .ok_or_else(|| format!("{heading} aircraft are heading that way, but none of them flies that route right now"))
}

/// The aircraft flying as `callsign` now, with its route when it is known; else why not.
pub async fn by_callsign(callsign: &str) -> Result<(Plane, Option<Route>), String> {
    let v = crate::earth::get_json(&callsign_url(callsign), TIMEOUT).await.map_err(feed_down)?;
    let plane = parse_planes(&v).into_iter().next().ok_or_else(|| format!("{} isn't in the air within the receivers' reach: not up yet, already down, or far out over an ocean", spoken(callsign)))?;
    let route = crate::earth::get_json(&route_url(callsign), TIMEOUT).await.ok().as_ref().and_then(parse_route);
    Ok((plane, route))
}

/// How long a plane is flown on from its last fix while no new one comes (dead reckoning, as
/// a radar screen coasts a track); then it waits where it got to.
pub const COAST_SECS: f64 = 30.0;

/// Where `plane` is `secs` after its fix: flown on along its track at its ground speed.
pub fn ahead(plane: &Plane, secs: f64) -> (f64, f64) {
    let km = plane.speed_kt * 1.852 * secs.clamp(0.0, COAST_SECS) / 3600.0;
    crate::earth::destination((plane.lat, plane.lon), plane.track, km)
}

/// What JAI says when the followed plane's signal is gone for a minute: why, by how high it was.
pub fn lost_line(plane: &Plane) -> String {
    let why = match plane.alt_ft {
        Some(ft) if ft < 5_000.0 => "it was low, coming in to land, where the receivers lose it behind the ground",
        None => "it was on the ground, where few receivers hear it",
        Some(_) => "no receiver has heard it for a minute; far out over an ocean it's beyond their reach",
    };
    format!("I've lost {}, Sir: {why}.", spoken(&plane.callsign))
}

static FOLLOWING: Mutex<Option<String>> = Mutex::new(None);

/// The aircraft (its hex id) being followed.
pub fn following() -> Option<String> {
    FOLLOWING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Stops following; whether a flight was followed.
pub fn stop() -> bool {
    FOLLOWING.lock().unwrap_or_else(|e| e.into_inner()).take().is_some()
}

/// Seconds between two fixes of the followed plane; between them the Earth flies it on (`ahead`).
const FIX_SECS: u64 = 2;

/// Follows `plane` on the Earth, a fix every two seconds, until another flight, a place or
/// "stop tracking" takes over, the Earth closes, or a minute passes without its signal (then
/// he says why). A landing is said too.
pub fn follow(plane: &Plane, label: String) {
    let hex = plane.hex.clone();
    *FOLLOWING.lock().unwrap_or_else(|e| e.into_inner()) = Some(hex.clone());
    let mut last = plane.clone();
    tokio::spawn(async move {
        let mut misses = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(FIX_SECS)).await;
            if following().as_deref() != Some(hex.as_str()) || !crate::earth::is_open() {
                break;
            }
            let seen = crate::earth::get_json(&hex_url(&hex), TIMEOUT).await.ok().and_then(|v| parse_planes(&v).into_iter().next());
            match seen {
                Some(plane) => {
                    misses = 0;
                    if last.alt_ft.is_some() && plane.alt_ft.is_none() {
                        crate::announcements::say(format!("{} has landed, Sir.", spoken(&plane.callsign)));
                    }
                    crate::earth::show_plane(&plane, &label, true, None);
                    last = plane;
                }
                None if misses * FIX_SECS >= 60 => {
                    if stop() {
                        crate::announcements::say(lost_line(&last));
                    }
                    break;
                }
                None => misses += 1,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The feed's and the standing data's own shapes (as adsb.lol answered on 2026-11-22),
    /// the plane that heads the right way picked, its route checked against both cities.
    #[test]
    fn a_flight_on_a_route_is_found_in_the_feed() {
        let feed = json!({"ac": [
            {"hex": "3c6444", "flight": "DLH400  ", "lat": 53.1, "lon": -9.0, "alt_baro": 37000, "gs": 480.0, "track": 285.0},
            {"hex": "34640e", "flight": "IBE0282 ", "lat": 56.9, "lon": -8.8, "alt_baro": 41000, "gs": 482.8, "track": 173.8},
            {"hex": "aaaaaa", "flight": "GROUND1 ", "lat": 53.4, "lon": -6.2, "alt_baro": "ground", "gs": 3.0, "track": 290.0},
            {"hex": "bbbbbb", "lat": 53.0, "lon": -8.0, "alt_baro": 30000, "track": 285.0},
            {"hex": "3c6444", "flight": "DLH400  ", "lat": 53.1, "lon": -9.0, "alt_baro": 37000, "gs": 480.0, "track": 285.0}
        ]});
        let planes = parse_planes(&feed);
        assert_eq!(planes.len(), 5);
        assert_eq!((planes[0].callsign.as_str(), planes[2].alt_ft), ("DLH400", None));
        let (frankfurt, new_york) = ((50.11, 8.68), (40.71, -74.01));
        let heading: Vec<String> = heading_for(&planes, new_york).into_iter().map(|p| p.callsign).collect();
        assert_eq!(heading, ["DLH400"], "southbound, on the ground, no callsign and twice are left out");
        let route = parse_route(&json!({
            "callsign": "DLH400", "_airport_codes_iata": "FRA-JFK",
            "_airports": [
                {"name": "Frankfurt-am-Main International Airport", "location": "Frankfurt-am-Main", "lat": 50.026402, "lon": 8.54313},
                {"name": "John F Kennedy International Airport", "location": "New York", "lat": 40.639801, "lon": -73.7789}
            ]
        }))
        .unwrap();
        assert_eq!((route.codes.as_str(), route.airports[1].0.as_str()), ("FRA-JFK", "New York"));
        assert!(flies(&route, frankfurt, new_york));
        assert!(!flies(&route, new_york, frankfurt), "the other way round is another flight");
        assert!(!flies(&route, (48.14, 11.58), new_york), "Munich is not Frankfurt");
        assert!((bearing(frankfurt, new_york) - 292.0).abs() < 3.0, "{}", bearing(frankfurt, new_york));
        assert_eq!(route_url("DLH400"), "https://vrs-standing-data.adsb.lol/routes/DL/DLH400.json");
        assert_eq!(icao_callsign("LH 400"), "DLH400");
        assert_eq!(icao_callsign("dlh400"), "DLH400");
        assert_eq!(icao_callsign("ba117"), "BAW117");
        use crate::earth::{parse_action, Action};
        let track = |args: Value| parse_action(&args);
        assert_eq!(
            track(json!({"action": "track_flight", "from": "Frankfurt", "to": "New York"})),
            Ok(Action::Track { from: Some("Frankfurt".into()), to: Some("New York".into()), flight: None })
        );
        assert!(track(json!({"action": "track_flight", "from": "Frankfurt"})).is_err(), "half a route");
        assert_eq!(track(json!({"action": "stop_tracking"})), Ok(Action::StopTrack));
        assert!(crate::earth::describes_a_place("the capital of France") && crate::earth::describes_a_place("Hauptstadt Spaniens"));
        assert!(!crate::earth::describes_a_place("Paris") && !crate::earth::describes_a_place("the capital letter key"));
    }

    /// Sir, 2026-11-26: "he should follow the flights every move and not just update every now
    /// and then", and say why when he loses one: between two fixes the plane is flown on along
    /// its track at its speed, for so long.
    #[test]
    fn a_followed_plane_is_flown_on_between_fixes() {
        let p = parse_planes(&json!({"ac": [{"hex": "3c6444", "flight": "DLH400  ", "lat": 50.0, "lon": 8.0, "alt_baro": 37000, "gs": 480.0, "track": 90.0, "seen_pos": 1.5}]})).remove(0);
        assert_eq!(p.age, 1.5);
        let (lat, lon) = ahead(&p, 10.0);
        let km = crate::earth::haversine_km((p.lat, p.lon), (lat, lon));
        assert!((km - 480.0 * 1.852 * 10.0 / 3600.0).abs() < 0.01, "{km} km in ten seconds");
        assert!(lon > p.lon && (lat - p.lat).abs() < 0.01, "east: ({lat}, {lon})");
        let still = ahead(&p, 0.0);
        assert!((still.0 - p.lat).abs() < 1e-9 && (still.1 - p.lon).abs() < 1e-9);
        assert_eq!(ahead(&p, 600.0), ahead(&p, COAST_SECS), "it coasts half a minute, then waits");
        assert_eq!(spoken("DLH400"), "DLH 400");
        assert_eq!(lost_line(&p), "I've lost DLH 400, Sir: no receiver has heard it for a minute; far out over an ocean it's beyond their reach.");
        assert!(lost_line(&Plane { alt_ft: Some(2_000.0), ..p.clone() }).contains("coming in to land"));
    }
}
