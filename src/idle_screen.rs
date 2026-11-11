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
}

static GLANCE: Mutex<Option<(Instant, Glance)>> = Mutex::new(None);
static LOOKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Looks at the calendar and the weather when the last look is old (in the background; the
/// idle screen shows what is there).
pub fn refresh(app: &crate::app::Jai, rt: &tokio::runtime::Handle) {
    use std::sync::atomic::Ordering;
    if lock(&GLANCE).as_ref().is_some_and(|(at, _)| at.elapsed() < FRESH) || LOOKING.swap(true, Ordering::AcqRel) {
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
        *lock(&GLANCE) = Some((Instant::now(), Glance { next, weather }));
        LOOKING.store(false, Ordering::Release);
    });
}

fn glance() -> Glance {
    lock(&GLANCE).as_ref().map(|(_, g)| g.clone()).unwrap_or_default()
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

/// Paints the idle screen over `screen` at `strength` (0 … 1, eased by the HUD), with the
/// breathing ring at the emblem (`centre`, `radius`).
pub fn paint(ctx: &egui::Context, screen: Rect, centre: Pos2, radius: f32, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_idle_screen")));
    p.rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha((255.0 * DIM * strength) as u8));
    let accent = crate::hud_board::active_colour();
    let now = Local::now().naive_local();
    let time = ctx.input(|i| i.time) as f32;
    // The ring breathes where the emblem is: once in six seconds.
    let breath = 0.5 + 0.5 * (time * std::f32::consts::TAU / 6.0).sin();
    for (share, width, alpha) in [(1.0f32, 1.6f32, 0.5f32), (0.86, 1.0, 0.28)] {
        p.circle_stroke(centre, radius * share * (0.985 + 0.03 * breath), Stroke::new(width, accent.gamma_multiply(alpha * (0.55 + 0.45 * breath) * strength)));
    }
    let shift = drift(now.minute());
    let ink = Color32::from_rgb(0xE8, 0xF2, 0xFC);
    let muted = Color32::from_rgb(0x8E, 0xA4, 0xB8);
    // Under the emblem while it stands in the upper two thirds; else in the middle of the screen.
    let top = if centre.y + radius * 1.3 + 230.0 < screen.bottom() { pos2(centre.x, centre.y + radius * 1.3) } else { pos2(screen.center().x, screen.top() + screen.height() * 0.3) } + shift;
    let display = |size: f32| FontId::new(size, FontFamily::Name("display".into()));
    let clock = p.text(top, Align2::CENTER_TOP, now.format("%H:%M").to_string(), display(104.0), ink.gamma_multiply(strength));
    let mut y = clock.bottom() + 2.0;
    y = p.text(pos2(top.x, y), Align2::CENTER_TOP, date_line(now).to_uppercase(), FontId::monospace(13.0), accent.gamma_multiply(0.9 * strength)).bottom() + 20.0;
    let glance = glance();
    if let Some((title, start, place)) = &glance.next {
        y = p.text(pos2(top.x, y), Align2::CENTER_TOP, next_line(title, *start, place, now), display(19.0), ink.gamma_multiply(0.92 * strength)).bottom() + 8.0;
    }
    if let Some((degrees, sky)) = &glance.weather {
        p.text(pos2(top.x, y), Align2::CENTER_TOP, format!("{degrees:.0}°  ·  {sky}"), FontId::new(15.0, FontFamily::Name("label".into())), muted.gamma_multiply(strength));
    }
    // (The ring breathes: fifteen frames a second are plenty; the clock needs one a minute.)
    ctx.request_repaint_after(Duration::from_millis(66));
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
    fn idle_only_at_rest_and_the_figures_never_stand_still() {
        assert!(is_idle(true, AFTER, "idle"));
        assert!(!is_idle(true, AFTER - Duration::from_secs(1), "idle"));
        assert!(!is_idle(true, AFTER * 3, "speaking") && !is_idle(false, AFTER * 3, "idle"));
        let places: std::collections::HashSet<(i32, i32)> = (0..60).map(|m| (drift(m).x as i32, drift(m).y as i32)).collect();
        assert_eq!(places.len(), 25, "twenty-five places before one repeats");
        assert!((0..59).all(|m| drift(m) != drift(m + 1)), "a new place every minute");
        assert!((0..60).all(|m| drift(m).x.abs() <= 14.0 && drift(m).y.abs() <= 10.0));
    }
}
