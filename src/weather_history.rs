//! Weather beyond "right now" (that one is `get_precise_location_weather`'s): what it was on a
//! day or over a month ("what was the weather in Berlin on 3 May 2020?", "how cold was it last
//! January here?") from the Open-Meteo archive (ERA5, back to 1940), a day ahead from its
//! forecast, and the rain nowcast for the next two hours in 15-minute steps ("will it rain in
//! the next hour?"), which also drives the heads-up shortly before rain arrives. No key, no
//! account. The parsers and the sentences are pure; the fetches go through `live_layers`.

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, Timelike, Weekday};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub const ARCHIVE_URL: &str = "https://archive-api.open-meteo.com/v1/archive";
pub const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
/// The archive lags a few days behind; the forecast API keeps the last weeks itself.
const RECENT_DAYS: i64 = 60;
/// How far ahead the forecast reaches.
pub const AHEAD_DAYS: i64 = 15;
/// The archive starts in 1940.
const FIRST_YEAR: i32 = 1940;
/// A quarter hour with at least this much (mm) counts as rain (about 0.4 mm an hour).
pub const WET_MM: f64 = 0.1;

// ------------------------------------------------------------------------------ dates ----

/// A day, or a stretch of days with how Sir said it ("January 2026", "last week").
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    Day(NaiveDate),
    Range { from: NaiveDate, to: NaiveDate, label: String },
}

impl When {
    pub fn span(&self) -> (NaiveDate, NaiveDate) {
        match self {
            Self::Day(d) => (*d, *d),
            Self::Range { from, to, .. } => (*from, *to),
        }
    }
}

const MONTHS: &str = r"(january|januar|jänner|jan|february|februar|feb|march|märz|maerz|marz|april|apr|may|mai|june|juni|jun|july|juli|jul|august|aug|september|sept|sep|october|oktober|oct|okt|november|nov|december|dezember|dec|dez)";

fn month_of(word: &str) -> Option<u32> {
    Some(match word.trim_end_matches('.') {
        "january" | "januar" | "jänner" | "jan" => 1,
        "february" | "februar" | "feb" => 2,
        "march" | "märz" | "maerz" | "marz" => 3,
        "april" | "apr" => 4,
        "may" | "mai" => 5,
        "june" | "juni" | "jun" => 6,
        "july" | "juli" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sept" | "sep" => 9,
        "october" | "oktober" | "oct" | "okt" => 10,
        "november" | "nov" => 11,
        "december" | "dezember" | "dec" | "dez" => 12,
        _ => return None,
    })
}

fn weekday_of(word: &str) -> Option<Weekday> {
    Some(match word {
        "monday" | "montag" => Weekday::Mon,
        "tuesday" | "dienstag" => Weekday::Tue,
        "wednesday" | "mittwoch" => Weekday::Wed,
        "thursday" | "donnerstag" => Weekday::Thu,
        "friday" | "freitag" => Weekday::Fri,
        "saturday" | "samstag" | "sonnabend" => Weekday::Sat,
        "sunday" | "sonntag" => Weekday::Sun,
        _ => return None,
    })
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid weather-history regex")
}

static PAST_TENSE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:was|were|did|had|been|war|waren|hatte|hatten|gab|regnete|schneite|ago|vor\s+\d+|vor\s+einem|vor\s+einer|last|letzte|letzten|letzter|letztes|vergangene|vergangenen|yesterday|gestern|vorgestern)\b"));
static ISO: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b"));
static NUMERIC: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4}|\d{2})\b"));
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b(\d{{1,2}})(?:st|nd|rd|th|\.)?\s+(?:of\s+)?{MONTHS}\b\.?(?:\s+(\d{{4}}))?")));
static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b{MONTHS}\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b(?:\s+(\d{{4}}))?")));
static MONTH_YEAR: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b{MONTHS}\.?\s+(\d{{4}})\b")));
static LAST_MONTH_NAMED: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b(?:last|this\s+past|letzten|letzter|vergangenen|im\s+letzten|im\s+vergangenen|im|in|during|im\s+monat)\s+{MONTHS}\b")));
static WEEKDAY: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:(last|past|this|next|coming|on|letzten|letzter|vergangenen|nächsten|naechsten|kommenden|diesen|am)\s+)?(monday|tuesday|wednesday|thursday|friday|saturday|sunday|montag|dienstag|mittwoch|donnerstag|freitag|samstag|sonnabend|sonntag)\b"));
static DAYS_AGO: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,3}|a|one|two|three|four|five|six|seven|ten)\s+(day|days|week|weeks|year|years)\s+ago\b|\bvor\s+(\d{1,3}|einem|einer|zwei|drei|vier|fünf|sechs|sieben|zehn)\s+(tag|tagen|woche|wochen|jahr|jahren)\b"));

fn small_number(word: &str) -> Option<i64> {
    word.parse().ok().or(Some(match word {
        "a" | "one" | "einem" | "einer" => 1,
        "two" | "zwei" => 2,
        "three" | "drei" => 3,
        "four" | "vier" => 4,
        "five" | "fünf" => 5,
        "six" | "sechs" => 6,
        "seven" | "sieben" => 7,
        "ten" | "zehn" => 10,
        _ => return None,
    }))
}

/// The last day of a month.
fn month_end(year: i32, month: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    first.checked_add_months(Months::new(1)).map(|d| d - Duration::days(1))
}

fn month_name(month: u32) -> &'static str {
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][(month.clamp(1, 12) - 1) as usize]
}

/// A whole month, cut at yesterday while it is still running.
fn month_range(year: i32, month: u32, today: NaiveDate) -> Option<When> {
    let from = NaiveDate::from_ymd_opt(year, month, 1)?;
    let to = month_end(year, month)?.min(today - Duration::days(1)).max(from);
    Some(When::Range { from, to, label: format!("{} {year}", month_name(month)) })
}

/// The day Sir means, if he named one: "on 3 May 2020", "2020-05-03", "3.5.2020", "yesterday",
/// "vorgestern", "last Saturday", "two weeks ago", "last January", "March 2021", "last week",
/// "letzten Monat". `s` is lowercase.
pub fn parse_when(s: &str, today: NaiveDate) -> Option<When> {
    let s = s.replace(',', " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let past = PAST_TENSE.is_match(&s);
    // A day without a year: the last one when he asks about the past, else the coming one.
    let year_for = |month: u32, day: u32| -> Option<NaiveDate> {
        let this = NaiveDate::from_ymd_opt(today.year(), month, day)?;
        if (past && this >= today) || this > today + Duration::days(AHEAD_DAYS) {
            NaiveDate::from_ymd_opt(today.year() - 1, month, day)
        } else {
            Some(this)
        }
    };
    if let Some(c) = ISO.captures(&s) {
        return NaiveDate::from_ymd_opt(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?).map(When::Day);
    }
    if let Some(c) = NUMERIC.captures(&s) {
        let mut year: i32 = c[3].parse().ok()?;
        if year < 100 {
            year += if year <= today.year() % 100 { 2000 } else { 1900 };
        }
        return NaiveDate::from_ymd_opt(year, c[2].parse().ok()?, c[1].parse().ok()?).map(When::Day);
    }
    if let Some(c) = DAY_MONTH.captures(&s) {
        let (day, month) = (c[1].parse().ok()?, month_of(&c[2])?);
        return match c.get(3) {
            Some(y) => NaiveDate::from_ymd_opt(y.as_str().parse().ok()?, month, day),
            None => year_for(month, day),
        }
        .map(When::Day);
    }
    if let Some(c) = MONTH_DAY.captures(&s) {
        // ("may I ask …" has no number after it.)
        let (month, day) = (month_of(&c[1])?, c[2].parse().ok()?);
        return match c.get(3) {
            Some(y) => NaiveDate::from_ymd_opt(y.as_str().parse().ok()?, month, day),
            None => year_for(month, day),
        }
        .map(When::Day);
    }
    if let Some(c) = MONTH_YEAR.captures(&s) {
        return month_range(c[2].parse().ok()?, month_of(&c[1])?, today);
    }
    if let Some(c) = DAYS_AGO.captures(&s) {
        let (n, unit) = match (c.get(1), c.get(3)) {
            (Some(n), _) => (small_number(n.as_str())?, c.get(2)?.as_str()),
            (_, Some(n)) => (small_number(n.as_str())?, c.get(4)?.as_str()),
            _ => return None,
        };
        return Some(When::Day(if unit.starts_with("year") || unit.starts_with("jahr") {
            today.checked_sub_months(Months::new(12 * n.clamp(0, 90) as u32))?
        } else if unit.starts_with("week") || unit.starts_with("woche") {
            today - Duration::days(7 * n)
        } else {
            today - Duration::days(n)
        }));
    }
    let has = |words: &[&str]| words.iter().any(|w| s.contains(w));
    if has(&["day before yesterday", "vorgestern"]) {
        return Some(When::Day(today - Duration::days(2)));
    }
    if has(&["yesterday", "gestern"]) {
        return Some(When::Day(today - Duration::days(1)));
    }
    if has(&["day after tomorrow", "übermorgen", "uebermorgen"]) {
        return Some(When::Day(today + Duration::days(2)));
    }
    let padded = format!(" {s} ");
    if padded.contains(" tomorrow ") || (padded.contains(" morgen ") && !has(&["heute morgen", "morgen früh", "this morning"]) || padded.contains(" morgen früh ")) {
        return Some(When::Day(today + Duration::days(1)));
    }
    if has(&["last week", "past week", "letzte woche", "letzten woche", "vergangene woche", "vergangenen woche"]) {
        let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64 + 7);
        return Some(When::Range { from: monday, to: monday + Duration::days(6), label: "last week".into() });
    }
    if has(&["last month", "past month", "letzten monat", "letzter monat", "vergangenen monat", "im letzten monat"]) {
        let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?.checked_sub_months(Months::new(1))?;
        return month_range(first.year(), first.month(), today);
    }
    if has(&["last year", "letztes jahr", "letzten jahr", "vergangenes jahr", "vergangenen jahr"]) {
        let year = today.year() - 1;
        return Some(When::Range { from: NaiveDate::from_ymd_opt(year, 1, 1)?, to: NaiveDate::from_ymd_opt(year, 12, 31)?, label: year.to_string() });
    }
    if let Some(c) = LAST_MONTH_NAMED.captures(&s) {
        let month = month_of(&c[1])?;
        let explicitly_last = ["last", "past", "letzt", "vergangen"].iter().any(|w| c[0].contains(w));
        // ("in May" said in May is this month so far; "last May" is last year's.)
        let year = if month < today.month() || (month == today.month() && !explicitly_last) { today.year() } else { today.year() - 1 };
        // "In June" asked without a past tense is a plan, not a memory: no date then.
        if !explicitly_last && !past {
            return None;
        }
        return month_range(year, month, today);
    }
    if let Some(c) = WEEKDAY.captures(&s) {
        let day = weekday_of(&c[2])?;
        let lead = c.get(1).map_or("", |m| m.as_str());
        let back = match lead {
            "last" | "past" | "letzten" | "letzter" | "vergangenen" => true,
            "next" | "coming" | "nächsten" | "naechsten" | "kommenden" => false,
            _ => past,
        };
        let today_n = today.weekday().num_days_from_monday() as i64;
        let day_n = day.num_days_from_monday() as i64;
        let date = if back {
            let d = (today_n - day_n).rem_euclid(7);
            today - Duration::days(if d == 0 { 7 } else { d })
        } else {
            let d = (day_n - today_n).rem_euclid(7);
            today + Duration::days(if d == 0 && lead != "this" && lead != "diesen" { 7 } else { d })
        };
        return Some(When::Day(date));
    }
    if has(&["today", "heute"]) {
        return Some(When::Day(today));
    }
    None
}

/// Strips the date words from a sentence (what is left may name a place).
pub fn without_dates(s: &str) -> String {
    let mut out = s.replace(',', " ");
    for r in [&*ISO, &*NUMERIC, &*DAY_MONTH, &*MONTH_DAY, &*MONTH_YEAR, &*DAYS_AGO, &*LAST_MONTH_NAMED, &*WEEKDAY] {
        out = r.replace_all(&out, " ").to_string();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------------- daily data ----

/// One day's weather.
#[derive(Debug, Clone, PartialEq)]
pub struct DayWeather {
    pub date: NaiveDate,
    pub t_max: Option<f64>,
    pub t_min: Option<f64>,
    pub precip_mm: Option<f64>,
    pub code: Option<i64>,
    pub wind_max_kmh: Option<f64>,
}

/// The URL for a stretch of days: the forecast API for the last weeks and the days ahead, the
/// archive for anything older.
pub fn daily_url(lat: f64, lon: f64, from: NaiveDate, to: NaiveDate, today: NaiveDate) -> String {
    let host = if from >= today - Duration::days(RECENT_DAYS) { FORECAST_URL } else { ARCHIVE_URL };
    format!(
        "{host}?latitude={lat:.3}&longitude={lon:.3}&start_date={from}&end_date={to}&daily=temperature_2m_max,temperature_2m_min,precipitation_sum,weather_code,wind_speed_10m_max&timezone=auto"
    )
}

/// Whether a stretch can be asked for at all: not before 1940, not past the forecast, and at
/// most a year and a bit in one go.
pub fn check_span(from: NaiveDate, to: NaiveDate, today: NaiveDate) -> Result<(), String> {
    if from.year() < FIRST_YEAR {
        return Err("The weather archive only goes back to 1940, Sir.".into());
    }
    if to > today + Duration::days(AHEAD_DAYS) {
        return Err(format!("I can only see about {AHEAD_DAYS} days ahead, Sir."));
    }
    if (to - from).num_days() > 400 {
        return Err("That's rather a long stretch, Sir: a year at most, please.".into());
    }
    Ok(())
}

pub fn parse_daily(v: &Value) -> Vec<DayWeather> {
    let d = &v["daily"];
    let Some(times) = d["time"].as_array() else { return Vec::new() };
    let num = |key: &str, i: usize| d[key].get(i).and_then(Value::as_f64);
    times
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            Some(DayWeather {
                date: NaiveDate::parse_from_str(t.as_str()?, "%Y-%m-%d").ok()?,
                t_max: num("temperature_2m_max", i),
                t_min: num("temperature_2m_min", i),
                precip_mm: num("precipitation_sum", i),
                code: d["weather_code"].get(i).and_then(Value::as_f64).map(|c| c as i64),
                wind_max_kmh: num("wind_speed_10m_max", i),
            })
        })
        .collect()
}

/// "7", "-3" (never "-0").
pub fn deg(t: f64) -> String {
    let r = t.round();
    if r == 0.0 {
        "0".into()
    } else {
        format!("{r:.0}")
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn mm(v: f64) -> String {
    if v < 0.05 {
        "no".into()
    } else if v < 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    }
}

/// How a day reads when spoken: "yesterday", "today", "tomorrow", "on Sunday the 3rd of May 2020".
pub fn spoken_day(date: NaiveDate, today: NaiveDate) -> String {
    match (date - today).num_days() {
        -1 => "yesterday".into(),
        0 => "today".into(),
        1 => "tomorrow".into(),
        _ if date.year() == today.year() => format!("on {} the {} of {}", date.format("%A"), ordinal(date.day()), month_name(date.month())),
        _ => format!("on {} the {} of {} {}", date.format("%A"), ordinal(date.day()), month_name(date.month()), date.year()),
    }
}

/// (the block, the line Jarvis says) for one day or a stretch of days at `place`. `focus` is
/// what he asked about ("cold", "warm", "rain", "wind"), if anything.
pub fn answer(days: &[DayWeather], when: &When, place: &str, today: NaiveDate, focus: Option<&str>) -> (String, String) {
    let Some(first) = days.first() else {
        return ("No weather data for that day".into(), "The archive has nothing for that day, Sir.".into());
    };
    if days.len() == 1 || matches!(when, When::Day(_)) {
        let d = first;
        let past = d.date < today;
        let sky = d.code.map(crate::location_engine::weather_code_text).unwrap_or("unknown weather");
        let mut display = format!("{place} · {}\n{}", d.date.format("%A, %-d %B %Y"), sky);
        if let (Some(lo), Some(hi)) = (d.t_min, d.t_max) {
            display.push_str(&format!(" · {} to {} °C", deg(lo), deg(hi)));
        }
        if let Some(p) = d.precip_mm {
            display.push_str(&format!(" · {} mm precipitation", mm(p)));
        }
        if let Some(w) = d.wind_max_kmh {
            display.push_str(&format!(" · wind up to {w:.0} km/h"));
        }
        let when_said = spoken_day(d.date, today);
        let lead = {
            let mut l = when_said.clone();
            if let Some(c) = l.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            l
        };
        let temps = match (d.t_min, d.t_max, focus) {
            (Some(lo), _, Some("cold")) => format!("the low was {} degrees", deg(lo)),
            (_, Some(hi), Some("warm")) => format!("it reached {} degrees", deg(hi)),
            (Some(lo), Some(hi), _) => format!("{} to {} degrees", deg(lo), deg(hi)),
            _ => String::new(),
        };
        let rain = match d.precip_mm {
            Some(p) if p >= 0.1 => format!(", with {} millimetres of rain", mm(p)),
            Some(_) if focus == Some("rain") => ", and it stayed dry".to_string(),
            _ => String::new(),
        };
        // Ahead of time it is a forecast: "a low of 6", "up to 15".
        let temps = if past { temps } else { temps.replace("the low was", "a low of").replace("it reached", "up to") };
        let spoken = if past {
            format!("{lead} {place} had {sky}, Sir: {temps}{rain}.")
        } else if d.date == today {
            format!("Today in {place}: {sky}, {temps}{rain}, Sir.")
        } else {
            format!("{lead} {place} can expect {sky}, Sir: {temps}{rain}.")
        };
        return (display, spoken.replace(": ,", ":").replace(", ,", ","));
    }
    // A stretch of days.
    let label = match when {
        When::Range { label, .. } => label.clone(),
        When::Day(d) => d.to_string(),
    };
    let maxes: Vec<(NaiveDate, f64)> = days.iter().filter_map(|d| Some((d.date, d.t_max?))).collect();
    let mins: Vec<(NaiveDate, f64)> = days.iter().filter_map(|d| Some((d.date, d.t_min?))).collect();
    let mean = |xs: &[(NaiveDate, f64)]| if xs.is_empty() { None } else { Some(xs.iter().map(|x| x.1).sum::<f64>() / xs.len() as f64) };
    let coldest = mins.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1));
    let warmest = maxes.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1));
    let rain_total: f64 = days.iter().filter_map(|d| d.precip_mm).sum();
    let wet_days = days.iter().filter(|d| d.precip_mm.is_some_and(|p| p >= 1.0)).count();
    let mut display = format!("{place} · {label} ({} days)", days.len());
    if let (Some(hi), Some(lo)) = (mean(&maxes), mean(&mins)) {
        display.push_str(&format!("\nAverage: {} °C by day, {} °C at night", deg(hi), deg(lo)));
    }
    if let Some((d, t)) = coldest {
        display.push_str(&format!("\nColdest: {} °C on {}", deg(t), d.format("%-d %B")));
    }
    if let Some((d, t)) = warmest {
        display.push_str(&format!("\nWarmest: {} °C on {}", deg(t), d.format("%-d %B")));
    }
    display.push_str(&format!("\nPrecipitation: {} mm, {wet_days} wet day(s)", mm(rain_total)));
    let mut parts: Vec<String> = Vec::new();
    match focus {
        Some("cold") => {
            if let Some((d, t)) = coldest {
                parts.push(format!("the coldest it got was {} degrees on the {}", deg(t), ordinal(d.day())));
            }
            if let Some(lo) = mean(&mins) {
                parts.push(format!("nights averaged {} degrees", deg(lo)));
            }
        }
        Some("warm") => {
            if let Some((d, t)) = warmest {
                parts.push(format!("the warmest day reached {} degrees on the {}", deg(t), ordinal(d.day())));
            }
            if let Some(hi) = mean(&maxes) {
                parts.push(format!("days averaged {} degrees", deg(hi)));
            }
        }
        _ => {
            if let Some(hi) = mean(&maxes) {
                parts.push(format!("days averaged {} degrees", deg(hi)));
            }
            if let Some((d, t)) = coldest {
                parts.push(format!("the coldest night fell to {} on the {}", deg(t), ordinal(d.day())));
            }
            if let Some((d, t)) = warmest {
                parts.push(format!("the warmest day reached {} on the {}", deg(t), ordinal(d.day())));
            }
        }
    }
    parts.push(if rain_total < 0.5 { "it stayed almost completely dry".to_string() } else { format!("{} millimetres of rain fell over {wet_days} wet day{}", mm(rain_total), if wet_days == 1 { "" } else { "s" }) });
    let spoken = format!("{label} in {place}, Sir: {}.", join_and(&parts));
    (display, spoken)
}

fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

/// What a weather question is about: "how cold", "how warm", "did it rain", "how windy".
pub fn focus_of(s: &str) -> Option<&'static str> {
    if ["cold", "kalt", "frost", "freezing", "chilly"].iter().any(|w| s.contains(w)) {
        Some("cold")
    } else if ["warm", "hot", "heiß", "heiss", "heat"].iter().any(|w| s.contains(w)) {
        Some("warm")
    } else if ["rain", "regen", "regne", "wet", "nass", "snow", "schnee"].iter().any(|w| s.contains(w)) {
        Some("rain")
    } else {
        None
    }
}

// ----------------------------------------------------------------------- the nowcast ----

/// One quarter hour of the nowcast. Open-Meteo's precipitation is the sum over the quarter
/// hour *before* `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Quarter {
    pub at: NaiveDateTime,
    pub mm: f64,
    pub probability: Option<f64>,
}

impl Quarter {
    /// When the quarter hour this one sums up begins.
    pub fn starts(&self) -> NaiveDateTime {
        self.at - Duration::minutes(15)
    }
    pub fn wet(&self) -> bool {
        self.mm >= WET_MM
    }
}

pub fn nowcast_url(lat: f64, lon: f64) -> String {
    format!("{FORECAST_URL}?latitude={lat:.3}&longitude={lon:.3}&minutely_15=precipitation,rain,precipitation_probability&forecast_minutely_15=10&timezone=auto")
}

pub fn parse_nowcast(v: &Value) -> Vec<Quarter> {
    let m = &v["minutely_15"];
    let Some(times) = m["time"].as_array() else { return Vec::new() };
    times
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            Some(Quarter {
                at: NaiveDateTime::parse_from_str(t.as_str()?, "%Y-%m-%dT%H:%M").ok()?,
                mm: m["precipitation"].get(i).and_then(Value::as_f64).unwrap_or(0.0),
                probability: m["precipitation_probability"].get(i).and_then(Value::as_f64),
            })
        })
        .collect()
}

/// The place's local time now, from the answer's UTC offset.
pub fn local_now(v: &Value, utc_now: chrono::DateTime<chrono::Utc>) -> NaiveDateTime {
    let offset = v["utc_offset_seconds"].as_i64().unwrap_or(0);
    utc_now.naive_utc() + Duration::seconds(offset)
}

fn intensity(mm_per_quarter: f64) -> &'static str {
    if mm_per_quarter < 0.25 {
        "light"
    } else if mm_per_quarter < 1.0 {
        "moderate"
    } else {
        "heavy"
    }
}

/// The quarters still ahead (the one running now first).
fn ahead(quarters: &[Quarter], now: NaiveDateTime) -> Vec<&Quarter> {
    quarters.iter().filter(|q| q.at > now).collect()
}

/// "Will it rain in the next hour?": (the block, the line Jarvis says).
pub fn nowcast_answer(quarters: &[Quarter], now: NaiveDateTime, place: &str, minutes: i64) -> (String, String) {
    let next = ahead(quarters, now);
    if next.is_empty() {
        return ("No nowcast".into(), "The rain nowcast didn't answer, Sir.".into());
    }
    let horizon = now + Duration::minutes(minutes.clamp(15, 150));
    let window: Vec<&Quarter> = next.iter().copied().filter(|q| q.starts() < horizon).collect();
    let display = format!(
        "Rain nowcast · {place}\n{}",
        window
            .iter()
            .map(|q| format!("{}–{}  {:.1} mm{}", q.starts().format("%H:%M"), q.at.format("%H:%M"), q.mm, q.probability.map(|p| format!("  ({p:.0} %)")).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let span = if minutes <= 60 { "the next hour".to_string() } else { format!("the next {} minutes", minutes) };
    let raining = window.first().is_some_and(|q| q.wet());
    let spoken = if raining {
        let heaviest = window.iter().map(|q| q.mm).fold(0.0, f64::max);
        match window.iter().position(|q| !q.wet()) {
            Some(i) => format!("It's raining in {place} right now, Sir, {} — it should ease off around {}.", intensity(heaviest), window[i].starts().format("%H:%M")),
            None => format!("It's raining in {place} right now, Sir, and it will keep going for at least {span} — {} rain.", intensity(heaviest)),
        }
    } else {
        match window.iter().find(|q| q.wet()) {
            Some(q) => {
                let starts = q.starts().max(now);
                let mins = (starts - now).num_minutes().max(1);
                let chance = q.probability.map(|p| format!(", {p:.0} percent likely")).unwrap_or_default();
                format!("Rain is on its way, Sir: {} rain should start in {place} around {} — in about {mins} minutes{chance}.", intensity(q.mm), starts.format("%H:%M"))
            }
            None => {
                let chance = window.iter().filter_map(|q| q.probability).fold(0.0, f64::max);
                if chance >= 30.0 {
                    format!("No rain expected in {place} for {span}, Sir, though the models give it a {chance:.0} percent chance.")
                } else {
                    format!("No rain expected in {place} for {span}, Sir.")
                }
            }
        }
    };
    (display, spoken)
}

/// The heads-up: rain starting 10 to 25 minutes from now while it is dry now. (minutes until
/// it starts, mm in its first quarter hour).
pub fn rain_heads_up(quarters: &[Quarter], now: NaiveDateTime) -> Option<(i64, f64)> {
    let next = ahead(quarters, now);
    if next.first()?.wet() {
        return None;
    }
    let q = next.iter().find(|q| q.wet())?;
    let minutes = (q.starts() - now).num_minutes();
    (10..=25).contains(&minutes).then_some((minutes, q.mm))
}

/// A quiet nowcast for the demo and the tests: dry for half an hour, then light rain.
pub fn demo_nowcast(now: NaiveDateTime) -> Vec<Quarter> {
    let first = now.date().and_hms_opt(now.hour(), now.minute() / 15 * 15, 0).unwrap_or(now);
    (0..10)
        .map(|i| Quarter { at: first + Duration::minutes(15 * i), mm: if (3..6).contains(&i) { 0.3 + 0.1 * i as f64 } else { 0.0 }, probability: Some(if (3..6).contains(&i) { 70.0 } else { 10.0 }) })
        .collect()
}

/// A plausible day for the demo (no network): a mild, damp spring day.
pub fn demo_days(from: NaiveDate, to: NaiveDate) -> Vec<DayWeather> {
    let mut out = Vec::new();
    let mut d = from;
    let mut i = 0;
    while d <= to && i < 400 {
        let wave = ((d.ordinal() as f64) / 365.0 * std::f64::consts::TAU).cos();
        out.push(DayWeather {
            date: d,
            t_max: Some(14.0 - 9.0 * wave + (i % 3) as f64),
            t_min: Some(5.0 - 7.0 * wave - (i % 4) as f64),
            precip_mm: Some(if i % 3 == 0 { 2.4 } else { 0.0 }),
            code: Some(if i % 3 == 0 { 61 } else { 2 }),
            wind_max_kmh: Some(18.0 + (i % 5) as f64),
        });
        d += Duration::days(1);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // Monday, 9 November 2026.
    fn today() -> NaiveDate {
        day(2026, 11, 9)
    }

    #[test]
    fn days_are_read_the_way_sir_says_them() {
        let t = today();
        let cases: &[(&str, When)] = &[
            ("what was the weather in berlin on 3 may 2020", When::Day(day(2020, 5, 3))),
            ("what was the weather in berlin on the 3rd of may 2020", When::Day(day(2020, 5, 3))),
            ("weather on may 3rd, 2020 in paris", When::Day(day(2020, 5, 3))),
            ("wie war das wetter am 3. mai 2020 in berlin", When::Day(day(2020, 5, 3))),
            ("wetter am 03.05.2020", When::Day(day(2020, 5, 3))),
            ("weather 2020-05-03", When::Day(day(2020, 5, 3))),
            ("what was the weather yesterday", When::Day(day(2026, 11, 8))),
            ("wie war das wetter vorgestern", When::Day(day(2026, 11, 7))),
            ("what was the weather last saturday", When::Day(day(2026, 11, 7))),
            ("what was the weather last monday", When::Day(day(2026, 11, 2))),
            ("how warm was it two weeks ago", When::Day(day(2026, 10, 26))),
            ("wie war das wetter vor einem jahr", When::Day(day(2025, 11, 9))),
            ("what will the weather be on friday in hamburg", When::Day(day(2026, 11, 13))),
            ("weather on the 3rd of may", When::Day(day(2026, 5, 3))),
            ("what was the weather on 12 december", When::Day(day(2025, 12, 12))),
        ];
        for (said, want) in cases {
            assert_eq!(parse_when(said, t).as_ref(), Some(want), "{said}");
        }
        assert_eq!(parse_when("how cold was it last january here", t), Some(When::Range { from: day(2026, 1, 1), to: day(2026, 1, 31), label: "January 2026".into() }));
        assert_eq!(parse_when("wie kalt war es im letzten dezember", t), Some(When::Range { from: day(2025, 12, 1), to: day(2025, 12, 31), label: "December 2025".into() }));
        assert_eq!(parse_when("how much did it rain in march 2021", t), Some(When::Range { from: day(2021, 3, 1), to: day(2021, 3, 31), label: "March 2021".into() }));
        assert_eq!(parse_when("how warm was it last week", t), Some(When::Range { from: day(2026, 11, 2), to: day(2026, 11, 8), label: "last week".into() }));
        // This month so far: cut at yesterday.
        assert_eq!(parse_when("how cold was it in november", t), Some(When::Range { from: day(2026, 11, 1), to: day(2026, 11, 8), label: "November 2026".into() }));
        assert_eq!(parse_when("how cold was it last month", t), Some(When::Range { from: day(2026, 10, 1), to: day(2026, 10, 31), label: "October 2026".into() }));
        // No date at all, or "may" as a verb.
        assert_eq!(parse_when("what is the weather like", t), None);
        assert_eq!(parse_when("may i ask what the weather is", t), None);
        assert_eq!(parse_when("going to paris in june", t), None);
    }

    #[test]
    fn the_place_is_what_is_left_without_the_date() {
        assert_eq!(without_dates("what was the weather in berlin on 3 may 2020"), "what was the weather in berlin on");
        assert_eq!(without_dates("how cold was it last january here"), "how cold was it here");
    }

    #[test]
    fn the_archive_answer_reads_and_is_spoken() {
        let v: Value = serde_json::from_str(r#"{"latitude":52.54833,"longitude":13.407822,"utc_offset_seconds":3600,"timezone":"Europe/Berlin","daily_units":{"time":"iso8601"},"daily":{"time":["2020-05-03"],"temperature_2m_max":[14.5],"temperature_2m_min":[6.4],"precipitation_sum":[0.40],"weather_code":[51],"wind_speed_10m_max":[20.6]}}"#).unwrap();
        let days = parse_daily(&v);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].code, Some(51));
        let (display, spoken) = answer(&days, &When::Day(day(2020, 5, 3)), "Berlin", today(), None);
        assert!(display.contains("Sunday, 3 May 2020"), "{display}");
        assert!(display.contains("6 to 15 °C"), "{display}");
        assert_eq!(spoken, "On Sunday the 3rd of May 2020 Berlin had drizzle, Sir: 6 to 15 degrees, with 0.4 millimetres of rain.");
        let (_, cold) = answer(&days, &When::Day(day(2020, 5, 3)), "Berlin", today(), Some("cold"));
        assert!(cold.contains("the low was 6 degrees"), "{cold}");
    }

    #[test]
    fn a_month_is_summed_up() {
        let (from, to) = (day(2026, 1, 1), day(2026, 1, 31));
        let days = demo_days(from, to);
        assert_eq!(days.len(), 31);
        let when = When::Range { from, to, label: "January 2026".into() };
        let (display, spoken) = answer(&days, &when, "Stuttgart", today(), Some("cold"));
        assert!(display.contains("Coldest:"), "{display}");
        assert!(spoken.starts_with("January 2026 in Stuttgart, Sir: the coldest it got was"), "{spoken}");
        assert!(spoken.contains("millimetres of rain fell over"), "{spoken}");
    }

    #[test]
    fn urls_pick_the_archive_for_old_days() {
        let t = today();
        assert!(daily_url(52.5, 13.4, day(2020, 5, 3), day(2020, 5, 3), t).starts_with(ARCHIVE_URL));
        assert!(daily_url(52.5, 13.4, day(2026, 11, 1), day(2026, 11, 8), t).starts_with(FORECAST_URL));
        assert!(daily_url(52.5, 13.4, day(2020, 5, 3), day(2020, 5, 3), t).contains("start_date=2020-05-03&end_date=2020-05-03"));
        assert!(check_span(day(1930, 1, 1), day(1930, 1, 2), t).is_err());
        assert!(check_span(day(2026, 12, 20), day(2026, 12, 20), t).is_err());
        assert!(check_span(day(2026, 1, 1), day(2026, 1, 31), t).is_ok());
    }

    fn quarters(from: &str, mm: &[f64]) -> Vec<Quarter> {
        let start = NaiveDateTime::parse_from_str(from, "%Y-%m-%dT%H:%M").unwrap();
        mm.iter().enumerate().map(|(i, m)| Quarter { at: start + Duration::minutes(15 * i as i64), mm: *m, probability: Some(if *m > 0.0 { 80.0 } else { 5.0 }) }).collect()
    }

    #[test]
    fn the_nowcast_answers_and_warns_in_time() {
        let v: Value = serde_json::from_str(r#"{"utc_offset_seconds":3600,"minutely_15":{"time":["2026-11-09T00:00","2026-11-09T00:15","2026-11-09T00:30"],"precipitation":[0.00,0.00,0.20],"rain":[0.0,0.0,0.2],"precipitation_probability":[0,5,60]}}"#).unwrap();
        let q = parse_nowcast(&v);
        assert_eq!(q.len(), 3);
        assert_eq!(q[2].starts().format("%H:%M").to_string(), "00:15");
        let utc = chrono::DateTime::parse_from_rfc3339("2026-11-08T23:06:00Z").unwrap().with_timezone(&chrono::Utc);
        let now = local_now(&v, utc);
        assert_eq!(now.format("%H:%M").to_string(), "00:06");

        let now = NaiveDateTime::parse_from_str("2026-11-09T00:06", "%Y-%m-%dT%H:%M").unwrap();
        // Dry now, rain from 00:30: 24 minutes away, inside the heads-up window.
        let q = quarters("2026-11-09T00:00", &[0.0, 0.0, 0.0, 0.4, 0.6, 0.2, 0.0]);
        assert_eq!(rain_heads_up(&q, now), Some((24, 0.4)));
        let (_, spoken) = nowcast_answer(&q, now, "Stuttgart", 60);
        assert_eq!(spoken, "Rain is on its way, Sir: moderate rain should start in Stuttgart around 00:30 — in about 24 minutes, 80 percent likely.");
        // Rain right now: no heads-up, and when it stops.
        let wet = quarters("2026-11-09T00:00", &[0.3, 0.3, 0.3, 0.0, 0.0]);
        assert_eq!(rain_heads_up(&wet, now), None);
        assert!(nowcast_answer(&wet, now, "Stuttgart", 60).1.contains("ease off around 00:30"));
        // Too far away still, or dry.
        let later = quarters("2026-11-09T00:00", &[0.0, 0.0, 0.0, 0.0, 0.0, 0.5]);
        assert_eq!(rain_heads_up(&later, now), None);
        let dry = quarters("2026-11-09T00:00", &[0.0; 8]);
        assert_eq!(nowcast_answer(&dry, now, "Stuttgart", 60).1, "No rain expected in Stuttgart for the next hour, Sir.");
        assert_eq!(demo_nowcast(now).len(), 10);
    }

    #[test]
    fn numbers_read_cleanly() {
        assert_eq!(deg(-0.3), "0");
        assert_eq!(deg(-3.6), "-4");
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(23), "23rd");
        assert_eq!(focus_of("how cold was it"), Some("cold"));
    }
}
