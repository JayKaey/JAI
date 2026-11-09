//! "What's happening in the world?" / "give me a world tour": the Earth flies from event to event
//! (the week's strongest earthquake, the active storms, the largest group of wildfires) with a
//! caption card and one short line each, then back home. "Stop the tour" ends it.
//!
//! The plan and the stepping are pure (and tested here); `earth` runs it in its frame step and
//! hands the lines to `announcements` (never in dry runs or tests).

use crate::earth::{self, Camera, EventKind, GeoEvent, Quake};

/// How long the camera rests at a stop (one short line is said meanwhile).
pub const DWELL_SECS: f64 = 8.0;
pub const MAX_STORMS: usize = 2;
const QUAKE_ALT_KM: f64 = 2_800.0;
const STORM_ALT_KM: f64 = 4_600.0;
const FIRES_ALT_KM: f64 = 2_300.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    Quake,
    Storm,
    Fires,
}

impl StopKind {
    pub fn label(self) -> &'static str {
        match self {
            StopKind::Quake => "EARTHQUAKE",
            StopKind::Storm => "STORM",
            StopKind::Fires => "WILDFIRES",
        }
    }
}

/// One stop: where the camera goes, the caption card and the line Jarvis says there.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub kind: StopKind,
    pub cam: Camera,
    pub title: String,
    pub caption: String,
    pub line: String,
}

/// The sea a storm is over, roughly ("the North Atlantic").
pub fn ocean_of(lat: f64, lon: f64) -> &'static str {
    let lon = earth::wrap_lon(lon);
    if lat >= 0.0 {
        if (-100.0..=-10.0).contains(&lon) || ((-10.0..=0.0).contains(&lon) && lat > 35.0) {
            "the North Atlantic"
        } else if (30.0..=100.0).contains(&lon) && lat < 30.0 {
            "the northern Indian Ocean"
        } else if lon > 100.0 || lon < -100.0 {
            if lon > 100.0 { "the western Pacific" } else { "the eastern Pacific" }
        } else {
            "open water"
        }
    } else if (-70.0..=20.0).contains(&lon) {
        "the South Atlantic"
    } else if (20.0..=120.0).contains(&lon) {
        "the southern Indian Ocean"
    } else {
        "the South Pacific"
    }
}

/// The densest group of wildfires: the 4-degree cell with the most, its middle and its fires.
pub fn fire_cluster(events: &[GeoEvent]) -> Option<((f64, f64), Vec<&GeoEvent>)> {
    let fires: Vec<&GeoEvent> = events.iter().filter(|e| e.kind == EventKind::Fire).collect();
    let mut cells: std::collections::HashMap<(i32, i32), Vec<&GeoEvent>> = std::collections::HashMap::new();
    for f in &fires {
        cells.entry(((f.lat / 4.0).floor() as i32, (earth::wrap_lon(f.lon) / 4.0).floor() as i32)).or_default().push(f);
    }
    let (_, group) = cells.into_iter().max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(&a.0)))?;
    let n = group.len() as f64;
    let centre = (group.iter().map(|f| f.lat).sum::<f64>() / n, group.iter().map(|f| f.lon).sum::<f64>() / n);
    Some((centre, group))
}

/// Storms to visit: hurricanes, typhoons and cyclones before the weaker ones.
fn storms(events: &[GeoEvent]) -> Vec<&GeoEvent> {
    let strong = |e: &GeoEvent| {
        let t = e.title.to_lowercase();
        ["hurricane", "typhoon", "cyclone"].iter().any(|w| t.contains(w))
    };
    let mut list: Vec<&GeoEvent> = events.iter().filter(|e| e.kind == EventKind::Storm).collect();
    list.sort_by_key(|e| !strong(e));
    list.truncate(MAX_STORMS);
    list
}

/// The tour through what is loaded: the strongest quake, the storms, the largest fire group.
pub fn plan(quakes: &[Quake], events: &[GeoEvent], now_ms: i64) -> Vec<Stop> {
    let mut stops = Vec::new();
    if let Some(q) = quakes.iter().max_by(|a, b| a.mag.total_cmp(&b.mag)) {
        let near = earth::near_phrase(&q.place);
        let when = earth::spoken_ago((now_ms - q.time_ms) / 1000);
        stops.push(Stop {
            kind: StopKind::Quake,
            cam: Camera { lat: q.lat, lon: q.lon, alt: QUAKE_ALT_KM },
            title: format!("M{:.1}", q.mag),
            caption: if q.place.is_empty() { earth::coords_label(q.lat, q.lon) } else { q.place.clone() },
            line: format!("The week's strongest earthquake: a magnitude {:.1}{near}, {when}.", q.mag),
        });
    }
    for s in storms(events) {
        let name = if s.title.trim().is_empty() { s.label.clone() } else { s.title.trim().to_string() };
        stops.push(Stop {
            kind: StopKind::Storm,
            cam: Camera { lat: s.lat, lon: s.lon, alt: STORM_ALT_KM },
            title: if s.label.is_empty() { name.to_uppercase() } else { s.label.clone() },
            caption: name.clone(),
            line: format!("{name}, over {}.", ocean_of(s.lat, s.lon)),
        });
    }
    if let Some(((lat, lon), group)) = fire_cluster(events) {
        let first = group[0].title.trim();
        let line = match group.len() {
            1 => format!("A wildfire is burning here: {first}."),
            n => format!("{} wildfires are burning in this region, among them {first}.", earth::count_word(n)),
        };
        stops.push(Stop { kind: StopKind::Fires, cam: Camera { lat, lon, alt: FIRES_ALT_KM }, title: format!("{} FIRES", group.len()), caption: first.to_string(), line });
    }
    stops
}

/// What Jarvis says as the tour starts (the first stop's line with it).
pub fn opening(stops: &[Stop]) -> String {
    match stops.first() {
        Some(first) => format!("Here's the world right now, Sir. {}", first.line),
        None => "The world is quiet right now, Sir: no major earthquakes, storms or wildfires are being tracked.".to_string(),
    }
}

pub const CLOSING: &str = "And that's the world, Sir.";

/// What the tour wants this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum TourStep {
    Wait,
    FlyTo(Camera),
    /// Arrived at a stop after the first: its line.
    Say(String),
    /// Over: back home.
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tour {
    pub stops: Vec<Stop>,
    pub index: usize,
    /// egui time of the arrival at the current stop.
    pub arrived: Option<f64>,
    started: bool,
}

impl Tour {
    pub fn new(stops: Vec<Stop>) -> Self {
        Self { stops, index: 0, arrived: None, started: false }
    }

    /// The stop the camera is at or flying to.
    pub fn current(&self) -> Option<&Stop> {
        self.stops.get(self.index)
    }

    /// One frame; `moving`: the camera is still on its way.
    pub fn step(&mut self, now: f64, moving: bool) -> TourStep {
        if !self.started {
            self.started = true;
            return match self.current() {
                Some(stop) => TourStep::FlyTo(stop.cam),
                None => TourStep::Done,
            };
        }
        if moving {
            return TourStep::Wait;
        }
        match self.arrived {
            None => {
                self.arrived = Some(now);
                // (The first stop's line was the answer to the request.)
                match self.current() {
                    Some(stop) if self.index > 0 => TourStep::Say(stop.line.clone()),
                    _ => TourStep::Wait,
                }
            }
            Some(at) if now - at >= DWELL_SECS => {
                self.index += 1;
                self.arrived = None;
                match self.current() {
                    Some(stop) => TourStep::FlyTo(stop.cam),
                    None => TourStep::Done,
                }
            }
            Some(_) => TourStep::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, title: &str, label: &str, lat: f64, lon: f64) -> GeoEvent {
        GeoEvent { id: title.into(), kind, title: title.into(), label: label.into(), lat, lon }
    }

    #[test]
    fn the_plan_visits_the_strongest_quake_the_storms_and_the_biggest_fire_group() {
        let now = 1_790_000_000_000i64;
        let q = |mag: f64, place: &str, lat: f64, lon: f64| Quake { id: place.into(), mag, place: place.into(), time_ms: now - 5 * 3_600_000, lat, lon, depth_km: 10.0 };
        let quakes = [q(4.8, "Somewhere", 0.0, 0.0), q(6.1, "61 km W of Pisco, Peru", -13.8, -76.8)];
        let events = [
            event(EventKind::Storm, "Tropical Storm Polo", "POLO-26", 15.2, -101.0),
            event(EventKind::Storm, "Hurricane Fay", "FAY-26", 33.0, -52.0),
            event(EventKind::Storm, "Tropical Depression Ten", "TEN-26", 12.0, 140.0),
            event(EventKind::Fire, "Lone Fire, Spain", "", 40.0, -4.0),
            event(EventKind::Fire, "Prairie Fire 1, Oklahoma", "", 34.6, -98.4),
            event(EventKind::Fire, "Prairie Fire 2, Oklahoma", "", 35.1, -97.2),
        ];
        let stops = plan(&quakes, &events, now);
        assert_eq!(stops.iter().map(|s| s.kind).collect::<Vec<_>>(), vec![StopKind::Quake, StopKind::Storm, StopKind::Storm, StopKind::Fires]);
        assert_eq!(stops[0].line, "The week's strongest earthquake: a magnitude 6.1 near Pisco, Peru, 5 hours ago.");
        assert_eq!(stops[1].line, "Hurricane Fay, over the North Atlantic.", "a hurricane before a tropical storm");
        assert_eq!(stops[2].line, "Tropical Storm Polo, over the eastern Pacific.");
        assert_eq!(stops[3].line, "Two wildfires are burning in this region, among them Prairie Fire 1, Oklahoma.");
        assert!((stops[3].cam.lat - 34.85).abs() < 1e-9);
        assert!(opening(&stops).starts_with("Here's the world right now, Sir. The week's strongest earthquake"));
        assert!(opening(&[]).contains("quiet"));
        assert_eq!(ocean_of(15.0, 88.0), "the northern Indian Ocean");
        assert_eq!(ocean_of(-20.0, 160.0), "the South Pacific");
        assert_eq!(ocean_of(20.0, 130.0), "the western Pacific");
    }

    #[test]
    fn the_tour_flies_rests_says_and_ends() {
        let cam = |lat: f64| Camera { lat, lon: 0.0, alt: 3_000.0 };
        let stop = |lat: f64, line: &str| Stop { kind: StopKind::Quake, cam: cam(lat), title: String::new(), caption: String::new(), line: line.into() };
        let mut tour = Tour::new(vec![stop(10.0, "one"), stop(20.0, "two")]);
        assert_eq!(tour.step(0.0, false), TourStep::FlyTo(cam(10.0)));
        assert_eq!(tour.step(0.5, true), TourStep::Wait, "still flying");
        assert_eq!(tour.step(1.5, false), TourStep::Wait, "the first line was already said");
        assert_eq!(tour.step(1.5 + DWELL_SECS - 0.1, false), TourStep::Wait);
        assert_eq!(tour.step(1.5 + DWELL_SECS, false), TourStep::FlyTo(cam(20.0)));
        assert_eq!(tour.current().map(|s| s.line.as_str()), Some("two"));
        assert_eq!(tour.step(11.0, true), TourStep::Wait);
        assert_eq!(tour.step(12.0, false), TourStep::Say("two".into()));
        assert_eq!(tour.step(12.0 + DWELL_SECS, false), TourStep::Done);
        assert_eq!(Tour::new(Vec::new()).step(0.0, false), TourStep::Done);
    }
}
