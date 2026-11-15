//! The idle screen (Sir, 2026-11-12: "calm screen"): unused for some minutes, the HUD dims down
//! to a large clock, the date, the next appointment and the weather, with a ring that breathes
//! where the emblem is. His voice or the mouse brings everything back. The figures move a few
//! pixels every minute, so nothing burns into an OLED.

use chrono::{Datelike, Local, NaiveDateTime, Timelike};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Pos2, Rect, Rounding, Stroke};
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
            .map(|e| (e.title, e.start, e.location));
        let weather = match tokio::time::timeout(Duration::from_secs(8), crate::location_engine::weather_report(tools, 1, None)).await {
            Ok(Ok(report)) => Some((report.current.temperature, crate::location_engine::weather_code_text(report.current.code).to_string())),
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

fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

/// The room the desktop clock takes under an emblem of `radius`: as wide as its time (and
/// never narrower than the emblem's core), as tall as its lines.
pub fn clock_size(ctx: &egui::Context, radius: f32, time: &str, date: bool, weather: bool) -> egui::Vec2 {
    let width = ctx.fonts(|f| f.layout_no_wrap(time.to_string(), display(clock_sizes(radius)[0]), Color32::WHITE)).size().x;
    vec2(width.max(radius * 1.2), clock_height(radius, date, weather))
}

/// Paints the desktop clock into `rect` (`clock_size`): the time, the date under it and the
/// weather under that, where each is given.
pub fn paint_clock(p: &egui::Painter, rect: Rect, radius: f32, time: &str, date: Option<&str>, weather: Option<&(f64, String)>) {
    let [time_size, small, sky] = clock_sizes(radius);
    let x = rect.center().x;
    p.text(pos2(x, rect.top()), Align2::CENTER_TOP, time, display(time_size), Color32::from_rgb(0xE8, 0xF2, 0xFC));
    let mut y = rect.top() + time_size * CLOCK_LEAD;
    if let Some(date) = date {
        p.text(pos2(x, y), Align2::CENTER_TOP, date, FontId::monospace(small), crate::hud_board::active_colour().gamma_multiply(0.9));
        y += small + 10.0;
    }
    if let Some((degrees, sky_word)) = weather {
        p.text(pos2(x, y), Align2::CENTER_TOP, format!("{degrees:.0}°  ·  {sky_word}"), FontId::new(sky, FontFamily::Name("label".into())), Color32::from_rgb(0x8E, 0xA4, 0xB8));
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

/// Paints the idle screen over `screen` at `strength` (0 … 1, eased by the HUD). The ring
/// stands at `centre` with `radius` (where the emblem is) and breathes; the time stands inside
/// it, the minute's seconds run round it as sixty marks, and the date, the next appointment and
/// the weather stand under it.
pub fn paint(ctx: &egui::Context, screen: Rect, centre: Pos2, radius: f32, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_idle_screen")));
    p.rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha((255.0 * DIM * strength) as u8));
    let accent = crate::hud_board::active_colour();
    let now = Local::now().naive_local();
    let time = ctx.input(|i| i.time) as f32;
    let centre = centre + drift(now.minute());
    let ink = Color32::from_rgb(0xE8, 0xF2, 0xFC);
    let muted = Color32::from_rgb(0x8E, 0xA4, 0xB8);
    // The ring breathes where the emblem is (once in six seconds); what is inside it goes dark,
    // so the time stands alone.
    p.circle_filled(centre, radius * 0.93, Color32::from_black_alpha((250.0 * strength) as u8));
    let breath = 0.5 + 0.5 * (time * std::f32::consts::TAU / 6.0).sin();
    for (share, width, alpha) in [(1.0f32, 1.6f32, 0.55f32), (0.93, 1.0, 0.25)] {
        p.circle_stroke(centre, radius * share * (0.99 + 0.02 * breath), Stroke::new(width, accent.gamma_multiply(alpha * (0.6 + 0.4 * breath) * strength)));
    }
    // The minute as it passes: sixty marks round the ring, lit up to the second.
    let second = now.second() as usize;
    for k in 0..60 {
        let angle = minute_mark(k);
        let dir = vec2(angle.cos(), angle.sin());
        let (from, to) = (radius * 1.07, radius * if k % 5 == 0 { 1.125 } else { 1.1 });
        let colour = if k <= second { accent.gamma_multiply(0.9 * strength) } else { crate::hud_style::white(0.14 * strength) };
        p.line_segment([centre + dir * from, centre + dir * to], Stroke::new(if k % 5 == 0 { 1.6f32 } else { 1.0 }, colour));
    }
    // Inside: what this is, the time, and the date under it.
    p.text(centre - vec2(0.0, radius * 0.42), Align2::CENTER_CENTER, "STANDING BY", FontId::monospace((radius * 0.05).clamp(8.5, 12.0)), muted.gamma_multiply(0.8 * strength));
    p.text(centre - vec2(0.0, radius * 0.07), Align2::CENTER_CENTER, now.format("%H:%M").to_string(), display((radius * 0.5).clamp(36.0, 150.0)), ink.gamma_multiply(strength));
    p.text(centre + vec2(0.0, radius * 0.33), Align2::CENTER_CENTER, date_line(now).to_uppercase(), FontId::monospace((radius * 0.062).clamp(9.5, 15.0)), accent.gamma_multiply(0.9 * strength));
    // Under it (above it where the ring stands low): the next appointment and the weather.
    let glance = glance();
    let lines = glance.next.is_some() as u8 + glance.weather.is_some() as u8;
    let below = centre.y + radius * 1.22 + lines as f32 * 30.0 < screen.bottom();
    let mut y = if below { centre.y + radius * 1.22 } else { centre.y - radius * 1.22 - lines as f32 * 30.0 };
    if let Some((title, start, place)) = &glance.next {
        y = p.text(pos2(centre.x, y), Align2::CENTER_TOP, next_line(title, *start, place, now), display(19.0), ink.gamma_multiply(0.92 * strength)).bottom() + 6.0;
    }
    if let Some((degrees, sky)) = &glance.weather {
        p.text(pos2(centre.x, y), Align2::CENTER_TOP, format!("{degrees:.0}°  ·  {sky}"), FontId::new(15.0, FontFamily::Name("label".into())), muted.gamma_multiply(strength));
    }
    // (The ring breathes: fifteen frames a second are plenty.)
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
