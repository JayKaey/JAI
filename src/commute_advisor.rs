//! The Stark commute, ride and geofence advisor: Sir's live position, the hyper-local forecast
//! and his next class (WebUntis) or event (Apple Calendar) in one answer.
//!
//! * "How is the weather for riding my motorcycle right now and later today?": temperature, rain
//!   probability over the next six hours (and for the ride back), wind gusts and the road-wetness
//!   risk at his exact coordinates, with a verdict.
//! * "When do I need to leave for school tomorrow?": the first lesson that is not cancelled, the
//!   real travel time (OSRM) for the way he travels, a buffer (more when the roads are wet) and an
//!   exact departure time.

use crate::location_engine::{self, CurrentWeather, HourWeather};
use crate::tools::{str_arg, ToolContext};
use crate::webuntis_client::LessonStatus;
use chrono::{Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime};
use serde_json::{json, Value};

/// A place Sir has to be, and when.
#[derive(Debug, Clone)]
struct Appointment {
    what: String,
    start: NaiveDateTime,
    end: Option<NaiveDateTime>,
    location: String,
    school: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Good,
    Care,
    Poor,
    NoGo,
}

impl Verdict {
    fn text(self) -> &'static str {
        match self {
            Self::Good => "good riding weather",
            Self::Care => "rideable, with care",
            Self::Poor => "poor riding weather — consider the car or the bus",
            Self::NoGo => "not advisable on two wheels",
        }
    }
}

/// Rider hazards for one stretch of time.
#[derive(Debug, Clone, Default)]
pub struct RideAssessment {
    pub hazards: Vec<String>,
    pub max_rain_pct: f64,
    pub max_gust_kmh: f64,
    pub min_temp: f64,
    pub wet_roads: bool,
}

impl RideAssessment {
    pub fn verdict(&self, thunder: bool) -> Verdict {
        let mut v = Verdict::Good;
        let mut raise = |to: Verdict| {
            if to > v {
                v = to;
            }
        };
        if thunder || self.max_gust_kmh >= 65.0 {
            raise(Verdict::NoGo);
        }
        if self.min_temp < 2.0 {
            raise(Verdict::Poor);
        } else if self.min_temp < 8.0 {
            raise(Verdict::Care);
        }
        if self.max_rain_pct >= 70.0 {
            raise(Verdict::Poor);
        } else if self.max_rain_pct >= 35.0 || self.wet_roads {
            raise(Verdict::Care);
        }
        if self.max_gust_kmh >= 45.0 {
            raise(Verdict::Poor);
        } else if self.max_gust_kmh >= 32.0 {
            raise(Verdict::Care);
        }
        v
    }
}

/// Hazards in [from, to) of the hourly forecast (plus the current conditions when `now`).
pub fn assess(hourly: &[HourWeather], current: Option<&CurrentWeather>, from: NaiveDateTime, to: NaiveDateTime) -> (RideAssessment, bool) {
    let window: Vec<&HourWeather> = hourly.iter().filter(|h| h.time + ChronoDuration::minutes(59) >= from && h.time <= to).collect();
    let mut a = RideAssessment { min_temp: f64::MAX, ..RideAssessment::default() };
    let mut thunder = false;
    for h in &window {
        a.max_rain_pct = a.max_rain_pct.max(h.precip_probability);
        a.max_gust_kmh = a.max_gust_kmh.max(h.gusts_kmh);
        a.min_temp = a.min_temp.min(h.temperature);
        thunder |= matches!(h.code, 95 | 96 | 99);
    }
    if let Some(c) = current {
        a.max_gust_kmh = a.max_gust_kmh.max(c.gusts_kmh);
        a.min_temp = a.min_temp.min(c.temperature);
        thunder |= matches!(c.code, 95 | 96 | 99);
        a.wet_roads |= c.precipitation > 0.05 || c.rain > 0.05;
    }
    // Rain in the three hours before: the roads are still wet.
    let recent: f64 = hourly.iter().filter(|h| h.time < from && h.time >= from - ChronoDuration::hours(3)).map(|h| h.precipitation).sum();
    a.wet_roads |= recent > 0.2;
    if a.min_temp == f64::MAX {
        a.min_temp = current.map(|c| c.temperature).unwrap_or(15.0);
    }
    if thunder {
        a.hazards.push("thunderstorms".into());
    }
    if a.min_temp < 2.0 {
        a.hazards.push(format!("near-freezing {:.0}°C — black ice possible on bridges and in shade", a.min_temp));
    } else if a.min_temp < 8.0 {
        a.hazards.push(format!("cold {:.0}°C — cold tyres and less grip", a.min_temp));
    }
    if a.max_rain_pct >= 35.0 {
        a.hazards.push(format!("up to {:.0}% rain probability", a.max_rain_pct));
    }
    if a.wet_roads {
        a.hazards.push("wet roads".into());
    }
    if a.max_gust_kmh >= 32.0 {
        a.hazards.push(format!("gusts up to {:.0} km/h", a.max_gust_kmh));
    }
    (a, thunder)
}

fn mode_of(raw: &str, default: &str) -> &'static str {
    let m = if raw.trim().is_empty() { default } else { raw };
    let m = m.to_lowercase();
    if ["motorcycle", "motorbike", "motorrad", "bike_motor", "scooter", "roller", "moped"].iter().any(|w| m.contains(w)) {
        "motorcycle"
    } else if ["bicycle", "bike", "fahrrad", "cycle", "rad"].iter().any(|w| m.contains(w)) {
        "bike"
    } else if ["walk", "foot", "fuß", "laufen"].iter().any(|w| m.contains(w)) {
        "walk"
    } else if ["bus", "train", "bahn", "transit", "öpnv", "tram"].iter().any(|w| m.contains(w)) {
        "transit"
    } else {
        "car"
    }
}

/// The next lesson that takes place (not cancelled) on `day`, or the first one after now.
async fn next_lesson(tools: &ToolContext, day: NaiveDate, now: NaiveDateTime) -> Option<Appointment> {
    let lessons = tools.pim.lessons(day, day + ChronoDuration::days(if day == now.date() { 1 } else { 0 })).await.ok()?;
    let mut list: Vec<_> = lessons.into_iter().filter(|l| l.status != LessonStatus::Cancelled && l.date.and_time(l.start) > now).collect();
    list.sort_by_key(|l| (l.date, l.start));
    let first = list.first()?;
    let last_today = list.iter().filter(|l| l.date == first.date).map(|l| l.end).max();
    Some(Appointment {
        what: format!("{} (first lesson)", first.subject_name()),
        start: first.date.and_time(first.start),
        end: last_today.map(|t| first.date.and_time(t)),
        location: first.rooms.join(", "),
        school: true,
    })
}

/// The next timed calendar event from `day` on.
async fn next_event(tools: &ToolContext, day: NaiveDate, now: NaiveDateTime) -> Option<Appointment> {
    let (events, _) = tools.pim.schedule(day, day + ChronoDuration::days(1)).await;
    let ev = events.into_iter().filter(|e| !e.all_day && e.start > now).min_by_key(|e| e.start)?;
    Some(Appointment { what: ev.title.clone(), start: ev.start, end: Some(ev.end), location: ev.location.clone(), school: false })
}

/// Travel time in seconds for a mode, with a note on where it came from.
async fn travel(tools: &ToolContext, mode: &str, from: (f64, f64), to: Option<(f64, f64)>) -> (f64, String) {
    let Some(to) = to else {
        return (20.0 * 60.0, "no destination coordinates, so I assumed 20 minutes".into());
    };
    let crow = location_engine::haversine_m(from.0, from.1, to.0, to.1);
    let road = location_engine::driving(tools.stark.location.http(), from, to).await;
    match mode {
        "walk" => (crow * 1.25 / (4.8 / 3.6), format!("{} on foot", location_engine::spoken_distance(crow * 1.25))),
        "bike" => (crow * 1.3 / (16.0 / 3.6), format!("{} by bike", location_engine::spoken_distance(crow * 1.3))),
        "transit" => match road {
            Some((secs, m)) => (secs * 1.6 + 600.0, format!("{} by bus/train (estimated from the road time)", location_engine::spoken_distance(m))),
            None => (crow / (25.0 / 3.6) + 600.0, "estimated public transport time".into()),
        },
        _ => match road {
            Some((secs, m)) => (secs, format!("{} by road (OSRM)", location_engine::spoken_distance(m))),
            None => (crow * 1.35 / (45.0 / 3.6), "estimated road time (the router was not reachable)".into()),
        },
    }
}

pub fn is_tool(name: &str) -> bool {
    name == "check_ride_and_commute_status"
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "check_ride_and_commute_status",
        "description": "Ride and commute advisor at Sir's GPS position: temperature, gusts, rain chance now and for the way back, wet-road risk, plus his next class (WebUntis) or event and the time to leave. For 'how's the weather for riding my motorcycle', 'when do I need to leave for school tomorrow'.",
        "parameters": {"type": "object", "properties": {
            "destination": {"type": "string", "description": "school, home, an address, next (the next class or event), or empty for the riding conditions only"},
            "travel_mode": {"type": "string", "enum": ["motorcycle", "car", "bike", "walk", "transit"]},
            "day": {"type": "string", "description": "today (default) or tomorrow / a weekday"}
        }, "required": []}
    }})
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let cfg = tools.config.read(|c| c.location.clone());
    let mode = mode_of(str_arg(args, &["travel_mode", "mode"]).unwrap_or(""), &cfg.default_travel_mode);
    let now = Local::now().naive_local();
    let today = now.date();
    let day = str_arg(args, &["day", "date"]).and_then(|d| crate::pim::parse_day(d, today)).unwrap_or(today);
    let destination = str_arg(args, &["destination", "to", "place"]).unwrap_or("").to_lowercase();
    tools.stark.pulse(&["brain", "commute", "gps"], false);
    let fix = tools.stark.location.current(tools).await?;
    let (current, hourly) = location_engine::fetch_weather(tools.stark.location.http(), fix.lat, fix.lon).await?;
    tools.stark.pulse(&["commute", "webuntis"], false);
    tools.stark.pulse(&["commute", "calendar"], false);

    // Where Sir has to be.
    let wants_school = destination.contains("school") || destination.contains("schule") || destination == "next";
    let mut appointment = if wants_school { next_lesson(tools, day, now).await } else { None };
    if appointment.is_none() && destination == "next" {
        appointment = next_event(tools, day, now).await;
    }
    let target: Option<(f64, f64, String)> = if destination.contains("school") || appointment.as_ref().is_some_and(|a| a.school) {
        cfg.school_lat.zip(cfg.school_lon).map(|(a, b)| (a, b, if cfg.school_label.is_empty() { "school".into() } else { cfg.school_label.clone() }))
    } else if destination == "home" || destination == "nach hause" {
        cfg.home_lat.zip(cfg.home_lon).map(|(a, b)| (a, b, "home".into()))
    } else if !destination.is_empty() && destination != "next" {
        tools.stark.location.geocode(&destination, Some((fix.lat, fix.lon))).await.ok()
    } else if let Some(loc) = appointment.as_ref().map(|a| a.location.clone()).filter(|l| l.len() > 4) {
        tools.stark.location.geocode(&loc, Some((fix.lat, fix.lon))).await.ok()
    } else {
        None
    };

    let mut out = format!(
        "Commute check from Sir's position ({}; {}), travelling by {mode}.\nNow: {:.0}°C (feels {:.0}°C), {}, wind {:.0} km/h, gusts {:.0} km/h.",
        fix.address.clone().unwrap_or_else(|| format!("{:.4}, {:.4}", fix.lat, fix.lon)),
        fix.source,
        current.temperature,
        current.apparent,
        location_engine::weather_code_text(current.code),
        current.wind_kmh,
        current.gusts_kmh
    );

    // The ride out (now or before the appointment) and the ride back.
    let two_wheels = matches!(mode, "motorcycle" | "bike");
    let (start, back) = match &appointment {
        Some(a) => (a.start - ChronoDuration::minutes(45), a.end.unwrap_or(a.start + ChronoDuration::hours(6))),
        None => (now, now + ChronoDuration::hours(6)),
    };
    let start = start.max(now);
    let (out_ride, out_thunder) = assess(&hourly, (start <= now + ChronoDuration::minutes(30)).then_some(&current), start, start + ChronoDuration::hours(1));
    let (back_ride, back_thunder) = assess(&hourly, None, back, back + ChronoDuration::hours(1));
    let (later, later_thunder) = assess(&hourly, None, now, now + ChronoDuration::hours(6));
    if two_wheels {
        let v_out = out_ride.verdict(out_thunder);
        let v_back = back_ride.verdict(back_thunder);
        out.push_str(&format!(
            "\nRide out ({}): {}{}.",
            start.format("%H:%M"),
            v_out.text(),
            if out_ride.hazards.is_empty() { String::new() } else { format!(" — {}", out_ride.hazards.join(", ")) }
        ));
        out.push_str(&format!(
            "\nRide back (around {}): {}{}.",
            back.format("%a %H:%M"),
            v_back.text(),
            if back_ride.hazards.is_empty() { String::new() } else { format!(" — {}", back_ride.hazards.join(", ")) }
        ));
        if appointment.is_none() {
            out.push_str(&format!(
                "\nNext 6 hours: up to {:.0}% rain, gusts up to {:.0} km/h, low {:.0}°C ({}).",
                later.max_rain_pct,
                later.max_gust_kmh,
                later.min_temp,
                later.verdict(later_thunder).text()
            ));
        }
        if let Some(dry) = hourly.iter().find(|h| h.time > now && h.precip_probability < 25.0 && h.precipitation < 0.1 && h.gusts_kmh < 35.0 && h.time < now + ChronoDuration::hours(12)) {
            if out_ride.verdict(out_thunder) >= Verdict::Poor {
                out.push_str(&format!("\nThe next dry, calm window starts around {}.", dry.time.format("%H:%M")));
            }
        }
    } else {
        out.push_str(&format!(
            "\nNext hours: up to {:.0}% rain, gusts up to {:.0} km/h{}.",
            later.max_rain_pct,
            later.max_gust_kmh,
            if later.wet_roads { ", wet roads" } else { "" }
        ));
    }

    match &appointment {
        Some(a) => {
            let (secs, how) = travel(tools, mode, (fix.lat, fix.lon), target.as_ref().map(|t| (t.0, t.1))).await;
            let mut buffer = cfg.commute_buffer_min as i64;
            if two_wheels && (out_ride.wet_roads || out_ride.max_rain_pct >= 50.0) {
                buffer += 5;
            }
            let leave = a.start - ChronoDuration::seconds(secs.round() as i64) - ChronoDuration::minutes(buffer);
            let rel = if leave.date() == today { "today".to_string() } else { crate::pim::spoken_day(leave.date(), today) };
            out.push_str(&format!(
                "\nNext: {} at {} {}{}.\nTravel: {} ({}), plus a {buffer}-minute buffer.\nLeave by {} {rel}{}.",
                a.what,
                a.start.format("%H:%M"),
                crate::pim::spoken_day(a.start.date(), today),
                if a.location.is_empty() { String::new() } else { format!(" ({})", a.location) },
                location_engine::minutes_text(secs),
                how,
                leave.format("%H:%M"),
                if leave < now { " — that is already past, Sir is running late" } else { "" }
            ));
            if a.school && target.is_none() {
                out.push_str("\n(No school address saved: say \"set my school to <address>\" for real travel times.)");
            }
        }
        None if wants_school && day != today => out.push_str(&format!("\nNo lessons found for {} (no WebUntis lessons, or all cancelled).", crate::pim::spoken_day(day, today))),
        None => {}
    }
    out.push_str("\nAnswer Sir briefly: the verdict, the main hazard, and the departure time if there is one.");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hour(h: u32, temp: f64, rain: f64, mm: f64, gust: f64) -> HourWeather {
        HourWeather {
            time: NaiveDate::from_ymd_opt(2026, 10, 22).unwrap().and_hms_opt(h, 0, 0).unwrap(),
            temperature: temp,
            precip_probability: rain,
            precipitation: mm,
            wind_kmh: gust / 2.0,
            gusts_kmh: gust,
            code: 3,
        }
    }

    #[test]
    fn ride_verdicts_follow_rain_wind_and_cold() {
        let day = |h: u32| NaiveDate::from_ymd_opt(2026, 10, 22).unwrap().and_hms_opt(h, 0, 0).unwrap();
        let dry = vec![hour(8, 14.0, 5.0, 0.0, 20.0), hour(9, 15.0, 10.0, 0.0, 22.0)];
        let (a, t) = assess(&dry, None, day(8), day(9));
        assert_eq!(a.verdict(t), Verdict::Good);
        let wet = vec![hour(5, 9.0, 80.0, 1.5, 25.0), hour(8, 9.0, 40.0, 0.0, 25.0), hour(9, 9.0, 30.0, 0.0, 25.0)];
        let (a, t) = assess(&wet, None, day(8), day(9));
        assert!(a.wet_roads);
        assert_eq!(a.verdict(t), Verdict::Care);
        let stormy = vec![hour(8, 12.0, 20.0, 0.0, 70.0)];
        let (a, t) = assess(&stormy, None, day(8), day(9));
        assert_eq!(a.verdict(t), Verdict::NoGo);
        let icy = vec![hour(7, 1.0, 0.0, 0.0, 10.0)];
        let (a, t) = assess(&icy, None, day(7), day(8));
        assert_eq!(a.verdict(t), Verdict::Poor);
        assert_eq!(mode_of("", "motorcycle"), "motorcycle");
        assert_eq!(mode_of("Fahrrad", "car"), "bike");
    }
}
