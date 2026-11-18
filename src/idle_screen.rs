//! The idle screen (Sir, 2026-11-12: "calm screen"): unused for some minutes, the HUD dims down
//! to a large clock on a dial of the minute's seconds, the date, the next appointment and the weather,
//! where the core stands. His voice or the mouse brings everything back. The figures move a few
//! pixels every minute, so nothing burns into an OLED.

use chrono::{Datelike, Local, NaiveDateTime, Timelike};
use crate::holo::{self, Face};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Pos2, Rect, Rounding};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Unused for this long, the HUD goes idle.
pub const AFTER: Duration = Duration::from_secs(5 * 60);
/// How dark the idle screen lays over the board (the share of black).
const DIM: f32 = 0.88;
/// The glance at the calendar and the weather is fresh for this long.
const FRESH: Duration = Duration::from_secs(20 * 60);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the idle screen shows besides the time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glance {
    /// The next appointment: its title, when it starts and where.
    pub next: Option<(String, NaiveDateTime, String)>,
    /// The weather outside: degrees and a word for the sky.
    pub weather: Option<(f64, String)>,
    /// The lesson that runs or comes next: its subject, when it starts and its room.
    pub lesson: Option<(String, NaiveDateTime, String)>,
}

static GLANCE: Mutex<Option<(Instant, Glance)>> = Mutex::new(None);
static LOOKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Looks at the calendar and the weather when the last look is old (in the background; the
/// idle screen shows what is there).
pub fn refresh(app: &crate::app::Jai, rt: &tokio::runtime::Handle) {
    use std::sync::atomic::Ordering;
    // (A scripted session never looks at Sir's day by itself: no sign-in at his school, none
    // of his appointments on a test's screen.)
    if crate::platform::in_test_session() || lock(&GLANCE).as_ref().is_some_and(|(at, _)| at.elapsed() < FRESH) || LOOKING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    rt.spawn(async move {
        let now = Local::now().naive_local();
        let tools = app.tools();
        let next = tools
            .pim
            .calendar()
            .events_between(now, now + chrono::Duration::hours(36))
            .into_iter()
            .filter(|e| !e.all_day && e.start > now)
            .min_by_key(|e| e.start)
            .map(|e| (crate::english::in_english(&e.title), e.start, e.location));
        let weather = match tokio::time::timeout(Duration::from_secs(8), crate::location_engine::weather_report(tools, 1, None)).await {
            Ok(Ok(report)) => {
                let mut sky = crate::location_engine::weather_code_text(report.current.code).to_string();
                // The sunrise or sunset that comes next, worked out offline for his home.
                if let Some(sun) = crate::location_engine::next_sun(now) {
                    sky.push_str(&format!("  ·  {sun}"));
                }
                Some((report.current.temperature, sky))
            }
            _ => None,
        };
        let lesson = tokio::time::timeout(Duration::from_secs(12), crate::briefing::next_lesson(tools)).await.ok().flatten();
        *lock(&GLANCE) = Some((Instant::now(), Glance { next, weather, lesson }));
        LOOKING.store(false, Ordering::Release);
    });
}

/// The last glance at the calendar, the timetable and the weather (`refresh` keeps it fresh).
pub fn glance() -> Glance {
    lock(&GLANCE).as_ref().map(|(_, g)| g.clone()).unwrap_or_default()
}

/// The last glance while it is fresh (`FRESH`): what it tells of Sir's day still holds.
pub fn fresh_glance() -> Option<Glance> {
    lock(&GLANCE).as_ref().filter(|(at, _)| at.elapsed() < FRESH).map(|(_, g)| g.clone())
}

/// "Physics in 25 min · 204", "Dentist tomorrow at 14:30 · Main Street 4".
pub fn next_line(title: &str, start: NaiveDateTime, place: &str, now: NaiveDateTime) -> String {
    let minutes = (start - now).num_minutes();
    let when = if minutes < 1 {
        "now".to_string()
    } else if minutes < 60 {
        format!("in {minutes} min")
    } else if start.date() == now.date() {
        format!("at {}", start.format("%H:%M"))
    } else if start.date() == now.date() + chrono::Duration::days(1) {
        format!("tomorrow at {}", start.format("%H:%M"))
    } else {
        format!("{} at {}", start.format("%A"), start.format("%H:%M"))
    };
    let place = place.trim();
    if place.is_empty() { format!("{title} {when}") } else { format!("{title} {when}  ·  {place}") }
}

/// "Thursday, 12 November".
pub fn date_line(now: NaiveDateTime) -> String {
    format!("{}, {} {}", now.format("%A"), now.day(), now.format("%B"))
}

/// The desktop clock's time as Sir set it: twenty-four hours or twelve, its seconds or not.
/// (Sir, 2026-11-14: "I want the desktop view to have a clock".)
pub fn clock_text(now: NaiveDateTime, seconds: bool, twelve: bool) -> String {
    now.format(match (twelve, seconds) {
        (false, false) => "%H:%M",
        (false, true) => "%H:%M:%S",
        (true, false) => "%-I:%M %p",
        (true, true) => "%-I:%M:%S %p",
    })
    .to_string()
}

/// The sizes of the desktop clock's letters under an emblem of `radius`: the time's, the
/// date's and the weather's.
pub fn clock_sizes(radius: f32) -> [f32; 3] {
    [(radius * 0.42).clamp(40.0, 110.0), (radius * 0.07).clamp(10.5, 15.0), 14.0]
}

/// How far under the top of its time the desktop clock's next line begins, in the time's size.
const CLOCK_LEAD: f32 = 1.14;

/// How tall the desktop clock stands: the time, the date under it, the weather under that.
pub fn clock_height(radius: f32, date: bool, weather: bool) -> f32 {
    let [time, small, sky] = clock_sizes(radius);
    time * CLOCK_LEAD + if date { small + 10.0 } else { 0.0 } + if weather { sky + 8.0 } else { 0.0 }
}

/// The room the desktop clock takes under an emblem of `radius`: as wide as its time (and
/// never narrower than the emblem's core), as tall as its lines.
pub fn clock_size(ctx: &egui::Context, radius: f32, time: &str, date: bool, weather: bool) -> egui::Vec2 {
    let width = ctx.fonts(|f| crate::holo::figure_galley(f, time, holo::font(Face::Figure, clock_sizes(radius)[0]), Color32::WHITE)).size().x;
    vec2(width.max(radius * 1.2), clock_height(radius, date, weather))
}

/// Paints the desktop clock into `rect` (`clock_size`): the time in thin figures, the date
/// under it in the accent's capitals and the weather under that, where each is given.
pub fn paint_clock(p: &egui::Painter, rect: Rect, radius: f32, time: &str, date: Option<&str>, weather: Option<&(f64, String)>) {
    let t = holo::theme();
    let [time_size, small, sky] = clock_sizes(radius);
    let x = rect.center().x;
    // Even-width digits, so the time does not shuffle sideways as the minutes turn; a digit that
    // changes rolls up into place.
    let font = holo::font(Face::Figure, time_size);
    let width = p.fonts(|f| crate::holo::figure_galley(f, time, font.clone(), Color32::WHITE)).size().x;
    crate::holo::rolling_figures(p, egui::Id::new("jai_desk_clock"), pos2(x - width * 0.5, rect.top()), time, font, t.ink());
    let mut y = rect.top() + time_size * CLOCK_LEAD;
    if let Some(date) = date {
        holo::text(p, pos2(x, y), Align2::CENTER_TOP, Face::Label, date, small * 0.78, t.accent());
        y += small + 10.0;
    }
    if let Some((degrees, sky_word)) = weather {
        holo::text(p, pos2(x, y), Align2::CENTER_TOP, Face::Body, &format!("{degrees:.0}°  ·  {sky_word}"), sky, t.muted());
    }
}

/// Where the figures stand this minute, off their place by a few pixels (it changes every
/// minute and comes round after twenty-five).
pub fn drift(minute: u32) -> egui::Vec2 {
    vec2(((minute % 5) as f32 - 2.0) * 7.0, ((minute / 5 % 5) as f32 - 2.0) * 5.0)
}

/// Whether the HUD is idle: nothing was touched or said for `AFTER`, he is at rest, and the
/// idle screen is switched on.
pub fn is_idle(on: bool, since_activity: Duration, state: &str) -> bool {
    on && state == "idle" && since_activity >= AFTER
}

/// Where the sixty marks of the minute stand round the ring: mark `k`'s angle (the first at
/// the top, clockwise).
pub fn minute_mark(k: usize) -> f32 {
    -std::f32::consts::FRAC_PI_2 + k as f32 / 60.0 * std::f32::consts::TAU
}

/// Paints the idle screen over `screen` at `strength` (0 … 1, eased by the HUD): a dial where
/// the core stands (`centre`, `radius`). Round it the minute's sixty seconds as marks, those
/// passed lit and the present one white-hot; inside it the hour's minutes as an arc of light
/// and the time in thin figures; under it the date, the next appointment and the weather.
pub fn paint(ctx: &egui::Context, screen: Rect, centre: Pos2, radius: f32, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    let t = holo::theme();
    let a = holo::a;
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_idle_screen")));
    p.rect_filled(screen, Rounding::ZERO, a(t.void(), DIM * strength));
    let now = Local::now().naive_local();
    let centre = centre + drift(now.minute());
    // What is inside the dial goes dark, so the time stands alone.
    p.circle_filled(centre, radius * 0.93, a(t.void(), 0.98 * strength));
    let mut light = holo::Light::new(&p);
    light.ring(centre, radius, holo::px(&p), a(t.ink(), 0.20 * strength));
    let second = now.second() as usize;
    for k in 0..60 {
        let angle = minute_mark(k);
        let (from, to) = (radius * 1.07, radius * if k % 5 == 0 { 1.125 } else { 1.10 });
        let (from, to) = (holo::polar(centre, from, angle), holo::polar(centre, to, angle));
        if k == second {
            light.neon(&[from, to], false, 1.8, a(t.hot(), strength), 1.0);
        } else {
            light.seg(from, to, if k % 5 == 0 { 1.5 } else { 1.0 }, if k < second { a(t.accent(), 0.85 * strength) } else { a(t.ink(), 0.14 * strength) });
        }
    }
    let hour_share = (now.minute() as f32 + now.second() as f32 / 60.0) / 60.0;
    if hour_share > 0.004 {
        let to = holo::NORTH + hour_share * std::f32::consts::TAU;
        light.neon_arc(centre, radius * 0.93, holo::NORTH, to, 1.3, a(t.accent(), strength), 0.8);
        light.spark(holo::polar(centre, radius * 0.93, to), 5.0, holo::lit(t.hot(), strength));
    }
    light.paint(&p);
    // Inside: what this is, the time, and the date under it.
    holo::text(&p, centre - vec2(0.0, radius * 0.42), Align2::CENTER_CENTER, Face::Label, "STANDING BY", (radius * 0.042).clamp(8.0, 10.0), a(t.faint(), strength));
    holo::text(&p, centre - vec2(0.0, radius * 0.05), Align2::CENTER_CENTER, Face::Figure, &now.format("%H:%M").to_string(), (radius * 0.52).clamp(36.0, 150.0), a(t.ink(), strength));
    holo::text(&p, centre + vec2(0.0, radius * 0.35), Align2::CENTER_CENTER, Face::Label, &date_line(now), (radius * 0.05).clamp(8.0, 12.0), a(t.accent(), strength));
    // Under it (above it where the dial stands low): the next appointment and the weather.
    let glance = glance();
    let lines = glance.next.is_some() as u8 + glance.weather.is_some() as u8;
    let below = centre.y + radius * 1.22 + lines as f32 * 30.0 < screen.bottom();
    let mut y = if below { centre.y + radius * 1.22 } else { centre.y - radius * 1.22 - lines as f32 * 30.0 };
    if let Some((title, start, place)) = &glance.next {
        y = holo::text(&p, pos2(centre.x, y), Align2::CENTER_TOP, Face::Body, &next_line(title, *start, place, now), 18.0, a(t.ink(), 0.92 * strength)).bottom() + 8.0;
    }
    if let Some((degrees, sky)) = &glance.weather {
        holo::text(&p, pos2(centre.x, y), Align2::CENTER_TOP, Face::Label, &format!("{degrees:.0}°  ·  {sky}"), 9.0, a(t.muted(), strength));
    }
    // (The seconds step: fifteen frames a second are plenty.)
    ctx.request_repaint_after(Duration::from_millis(66));
}

/// The glance is one for the whole test process: tests that set it take turns.
#[cfg(test)]
pub static GLANCE_TURN: Mutex<()> = Mutex::new(());

/// What the idle screen shows of the calendar and the weather, set by hand (the gallery).
#[cfg(test)]
pub fn show_glance(glance: Glance) {
    *lock(&GLANCE) = Some((Instant::now(), glance));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32, h: u32, m: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn the_next_appointment_in_a_line() {
        let now = at(12, 8, 10);
        assert_eq!(next_line("Physics", at(12, 8, 35), "204", now), "Physics in 25 min  ·  204");
        assert_eq!(next_line("Dentist", at(12, 14, 30), "", now), "Dentist at 14:30");
        assert_eq!(next_line("Dentist", at(13, 14, 30), "Main Street 4", now), "Dentist tomorrow at 14:30  ·  Main Street 4");
        assert_eq!(next_line("Exam", at(16, 9, 0), "", now), "Exam Monday at 09:00");
        assert_eq!(next_line("Call", at(12, 8, 10), "", now), "Call now");
        assert_eq!(date_line(now), "Thursday, 12 November");
    }

    #[test]
    fn the_desktop_clock_reads_as_sir_set_it() {
        assert_eq!(clock_text(at(12, 8, 5), false, false), "08:05");
        assert_eq!(clock_text(at(12, 8, 5), true, false), "08:05:00");
        assert_eq!(clock_text(at(12, 8, 5), false, true), "8:05 AM");
        assert_eq!(clock_text(at(12, 14, 30), true, true), "2:30:00 PM");
        assert_eq!(clock_text(at(12, 0, 15), false, true), "12:15 AM");
        // The time alone is shorter than with its date and the weather; a small emblem's
        // clock is still readable, a large one's not a poster.
        assert!(clock_height(200.0, false, false) < clock_height(200.0, true, false) && clock_height(200.0, true, false) < clock_height(200.0, true, true));
        assert_eq!(clock_sizes(40.0)[0], 40.0);
        assert_eq!(clock_sizes(900.0)[0], 110.0);
    }

    #[test]
    fn idle_only_at_rest_and_the_figures_never_stand_still() {
        assert!(is_idle(true, AFTER, "idle"));
        assert!(!is_idle(true, AFTER - Duration::from_secs(1), "idle"));
        assert!(!is_idle(true, AFTER * 3, "speaking") && !is_idle(false, AFTER * 3, "idle"));
        let places: std::collections::HashSet<(i32, i32)> = (0..60).map(|m| (drift(m).x as i32, drift(m).y as i32)).collect();
        assert_eq!(places.len(), 25, "twenty-five places before one repeats");
        assert!((0..59).all(|m| drift(m) != drift(m + 1)), "a new place every minute");
        assert!((0..60).all(|m| drift(m).x.abs() <= 14.0 && drift(m).y.abs() <= 10.0));
        // The minute's marks: the first at the top, the sixteenth on the right.
        assert!((minute_mark(0).sin() + 1.0).abs() < 1e-5 && (minute_mark(15).cos() - 1.0).abs() < 1e-5);
    }
}
