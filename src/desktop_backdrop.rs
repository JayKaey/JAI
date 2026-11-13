//! The live desktop background (Sir, 2026-11-12): on a monitor the board is not on, the desktop's
//! background itself becomes his: the emblem, the clock and what he is doing, behind the
//! desktop's icons. A borderless window of his own, handed to the desktop as the layer between
//! its wallpaper and its icons (`win32::attach_behind_desktop_icons`); it takes no clicks and
//! never the focus, draws ten frames a second and one while a game is in front. When the
//! desktop will not take it, it closes again and says so.

use crate::rose_emblem_renderer::{RoseEmblem, RoseEmblemState};
use chrono::Local;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, ViewportBuilder, ViewportId};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The window's title (it is found by it once it exists).
pub const TITLE: &str = "J.A.I. // desktop";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Backdrop {
    emblem: RoseEmblem,
    /// The monitor it stands on (physical pixels: left, top, right, bottom).
    monitor: [i32; 4],
    opened: Instant,
    last_frame: Instant,
    /// Asked the desktop to take it (once the window exists).
    asked: bool,
    /// When its place under the icons was last looked at.
    kept: Instant,
    /// It is the dashboard on the upright monitor (not the plain background).
    dashboard: bool,
}

static BACKDROP: Mutex<Option<Backdrop>> = Mutex::new(None);
/// What came of asking the desktop: Ok (its window) once it is its background, or why not.
static ATTACHED: Mutex<Option<Result<isize, String>>> = Mutex::new(None);
/// How often its place under the icons is looked at (a wallpaper program may re-order its own
/// windows there, over it).
const KEEP_EVERY: Duration = Duration::from_secs(5);

/// The monitor the background takes: the first that the board (`hud`, its monitor's rect) is not
/// on, a secondary one before the primary. None with a single monitor (the board needs it).
pub fn target(monitors: &[([i32; 4], bool)], hud: Option<[i32; 4]>, board: bool) -> Option<[i32; 4]> {
    let free = |primary: bool| monitors.iter().find(|(rect, p)| *p == primary && (!board || Some(*rect) != hud)).map(|(rect, _)| *rect);
    // (`board` off: he is the orb or the slim strip and holds no monitor, so even the only one
    // is free for it.)
    if monitors.len() < 2 && board {
        return None;
    }
    free(false).or_else(|| free(true))
}

/// Why the background cannot be shown right now (said once, and the switch goes back off).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// The rect of the monitor the HUD's window (`hwnd`) is on.
#[cfg(windows)]
fn monitor_of(hwnd: isize) -> Option<[i32; 4]> {
    (hwnd != 0).then(|| crate::win32::window_display_rect(hwnd)).flatten()
}

#[cfg(not(windows))]
fn monitor_of(_hwnd: isize) -> Option<[i32; 4]> {
    None
}

/// The upright monitor the dashboard takes: the first that is taller than wide and does not
/// hold the board.
pub fn upright(monitors: &[([i32; 4], bool)], hud: Option<[i32; 4]>, board: bool) -> Option<[i32; 4]> {
    monitors.iter().map(|(rect, _)| *rect).find(|rect| rect[3] - rect[1] > rect[2] - rect[0] && (!board || Some(*rect) != hud))
}

/// Shows the background while `on` (each HUD frame). `hud`: the HUD's own window (the monitor
/// it is on is left alone); `spanning`: the board is across all of them; `dashboard`: it is
/// the dashboard on the upright monitor; `state`: what he is doing.
pub fn show(ctx: &egui::Context, on: bool, hud: isize, spanning: bool, board: bool, dashboard: bool, state: &str) -> Option<Refused> {
    // (Switched between the plain background and the dashboard: it closes and opens anew,
    // on the monitor the other one belongs on.)
    let other = lock(&BACKDROP).as_ref().is_some_and(|b| b.dashboard != dashboard);
    if !on || other {
        if lock(&BACKDROP).take().is_some() {
            *lock(&ATTACHED) = None;
            tracing::info!("[DESKTOP] The live background is off");
        }
        return None;
    }
    if let Some(Err(why)) = lock(&ATTACHED).take_if(|r| r.is_err()) {
        lock(&BACKDROP).take();
        return Some(Refused(format!("The desktop would not take me as its background ({why}).")));
    }
    let mut guard = lock(&BACKDROP);
    if guard.is_none() {
        let monitors: Vec<([i32; 4], bool)> = displays();
        // (A scripted session sends it to the monitor it names, `JAI_BACKDROP_MONITOR=2`: its
        // own test screen, never the one Sir works on.)
        let named = crate::platform::in_test_session().then(|| std::env::var("JAI_BACKDROP_MONITOR").ok()).flatten().and_then(|n| n.trim().parse::<usize>().ok()).and_then(|n| monitors.get(n.saturating_sub(1)).map(|(rect, _)| *rect));
        let free = if dashboard { (!spanning).then(|| upright(&monitors, monitor_of(hud), board)).flatten() } else { (!spanning).then(|| target(&monitors, monitor_of(hud), board)).flatten() };
        let Some(monitor) = named.or(free) else {
            return Some(Refused(if spanning {
                "My board is across every monitor: there is no desktop left for me to stand behind.".into()
            } else if dashboard {
                "The dashboard needs an upright monitor my board is not on.".into()
            } else {
                "I need a second monitor for that: this one holds my board.".into()
            }));
        };
        tracing::info!("[DESKTOP] The live {} opens on the monitor at {},{} ({}×{})", if dashboard { "dashboard" } else { "background" }, monitor[0], monitor[1], monitor[2] - monitor[0], monitor[3] - monitor[1]);
        *guard = Some(Backdrop { emblem: RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem), monitor, opened: Instant::now(), last_frame: Instant::now(), asked: false, kept: Instant::now(), dashboard });
    }
    let Some(b) = guard.as_mut() else { return None };
    let ppp = ctx.native_pixels_per_point().unwrap_or(1.0).max(0.5);
    let [l, t, r, bottom] = b.monitor;
    let builder = ViewportBuilder::default()
        .with_title(TITLE)
        .with_decorations(false)
        .with_taskbar(false)
        .with_resizable(false)
        .with_active(false)
        .with_position(pos2(l as f32 / ppp, t as f32 / ppp))
        .with_inner_size(vec2((r - l) as f32 / ppp, (bottom - t) as f32 / ppp));
    let dt = b.last_frame.elapsed().as_secs_f32().min(0.2);
    b.last_frame = Instant::now();
    b.emblem.step(dt, 0.0, false);
    let emblem = &b.emblem;
    let gaming = crate::hud_feel::game_in_front();
    let as_dashboard = b.dashboard;
    ctx.show_viewport_immediate(ViewportId::from_hash_of("jai_desktop_backdrop"), builder, |ctx, _class| {
        if as_dashboard {
            paint_dashboard(ctx, emblem, state, &crate::kit_screen::dash());
        } else {
            paint(ctx, emblem, state);
        }
        ctx.request_repaint_after(Duration::from_millis(if gaming { 1000 } else { 100 }));
    });
    // Once the window exists: the desktop is asked to take it behind its icons.
    if !b.asked && b.opened.elapsed() > Duration::from_millis(400) {
        b.asked = true;
        let monitor = b.monitor;
        attach(monitor);
    }
    // Every few seconds: is it still the first thing under the icons?
    if b.kept.elapsed() > KEEP_EVERY {
        b.kept = Instant::now();
        if let Some(Ok(hwnd)) = *lock(&ATTACHED) {
            keep(hwnd);
        }
    }
    None
}

#[cfg(windows)]
fn displays() -> Vec<([i32; 4], bool)> {
    crate::win32::displays().into_iter().map(|d| (d.rect, d.primary)).collect()
}

#[cfg(not(windows))]
fn displays() -> Vec<([i32; 4], bool)> {
    Vec::new()
}

#[cfg(windows)]
fn attach(monitor: [i32; 4]) {
    crate::win32::run_detached(move || {
        let result = match crate::win32::own_window(TITLE) {
            Some(hwnd) => crate::win32::attach_behind_desktop_icons(hwnd, monitor).map(|()| hwnd),
            None => Err("its window did not open".to_string()),
        };
        match &result {
            Ok(hwnd) => tracing::info!("[DESKTOP] The desktop took the live background behind its icons ({})", crate::win32::backdrop_standing(*hwnd)),
            Err(why) => tracing::warn!("[DESKTOP] The live background was not taken: {why}"),
        }
        *lock(&ATTACHED) = Some(result);
    });
}

/// Looks whether the background is still the first thing under the icons, and puts it back
/// there when it is not.
#[cfg(windows)]
fn keep(hwnd: isize) {
    crate::win32::run_detached(move || match crate::win32::keep_behind_desktop_icons(hwnd) {
        Ok(true) => tracing::info!("[DESKTOP] Something had come between the icons and the live background; it is right under them again"),
        Ok(false) => {}
        Err(why) => tracing::debug!("[DESKTOP] The live background's place was not looked at: {why}"),
    });
}

#[cfg(not(windows))]
fn keep(_hwnd: isize) {}

#[cfg(not(windows))]
fn attach(_monitor: [i32; 4]) {
    *lock(&ATTACHED) = Some(Err("only on Windows".to_string()));
}

/// The background's picture: the obsidian canvas, the emblem at rest, the clock and the date
/// under it, and what he is doing.
fn paint(ctx: &egui::Context, emblem: &RoseEmblem, state: &str) {
    crate::gui::paint_canvas(ctx);
    let screen = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::background());
    emblem.paint(&p, screen);
    let frame = emblem.frame(screen);
    let now = Local::now().naive_local();
    let accent = crate::hud_board::active_colour();
    let top = pos2(frame.centre.x, frame.centre.y + frame.radius * 1.3);
    let clock = p.text(top, Align2::CENTER_TOP, now.format("%H:%M").to_string(), FontId::new(72.0, FontFamily::Name("display".into())), Color32::from_rgb(0xE8, 0xF2, 0xFC));
    let date = p.text(pos2(top.x, clock.bottom() + 2.0), Align2::CENTER_TOP, crate::idle_screen::date_line(now).to_uppercase(), FontId::monospace(12.0), accent.gamma_multiply(0.9));
    let word = match state {
        "listening" => "LISTENING",
        "thinking" => "THINKING",
        "speaking" => "SPEAKING",
        _ => "",
    };
    if !word.is_empty() {
        p.text(pos2(top.x, date.bottom() + 14.0), Align2::CENTER_TOP, word, FontId::monospace(11.0), Color32::WHITE);
    }
}

/// The dashboard's picture for an upright monitor: the emblem and the clock in the upper
/// part, and under them what the day holds, section by section.
pub fn paint_dashboard(ctx: &egui::Context, emblem: &RoseEmblem, state: &str, dash: &crate::kit_screen::Dash) {
    crate::gui::paint_canvas(ctx);
    let screen = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::background());
    let head = egui::Rect::from_min_max(screen.min, pos2(screen.right(), screen.top() + screen.height() * 0.40));
    emblem.paint(&p, head);
    let frame = emblem.frame(head);
    let now = Local::now().naive_local();
    let accent = crate::hud_board::active_colour();
    let ink = Color32::from_rgb(0xE8, 0xF2, 0xFC);
    let muted = Color32::from_rgb(0x8E, 0xA4, 0xB8);
    let top = pos2(frame.centre.x, frame.centre.y + frame.radius * 1.3);
    let clock = p.text(top, Align2::CENTER_TOP, now.format("%H:%M").to_string(), FontId::new(84.0, FontFamily::Name("display".into())), ink);
    let date = p.text(pos2(top.x, clock.bottom() + 2.0), Align2::CENTER_TOP, crate::idle_screen::date_line(now).to_uppercase(), FontId::monospace(13.0), accent.gamma_multiply(0.9));
    let word = match state {
        "listening" => "LISTENING",
        "thinking" => "THINKING",
        "speaking" => "SPEAKING",
        _ => "",
    };
    let mut y = date.bottom() + 12.0;
    if !word.is_empty() {
        y = p.text(pos2(top.x, y), Align2::CENTER_TOP, word, FontId::monospace(11.0), Color32::WHITE).bottom() + 8.0;
    }
    // The sections: a column with the same margin left and right.
    let margin = (screen.width() * 0.09).clamp(28.0, 110.0);
    let (left, right) = (screen.left() + margin, screen.right() - margin);
    y += 34.0;
    let section = |title: &str, lines: &[String], y: &mut f32| {
        if lines.is_empty() || *y > screen.bottom() - 120.0 {
            return;
        }
        p.text(pos2(left, *y), Align2::LEFT_TOP, title, FontId::monospace(11.0), accent);
        *y += 18.0;
        p.line_segment([pos2(left, *y), pos2(right, *y)], egui::Stroke::new(1.0f32, crate::hud_style::white(0.10)));
        *y += 10.0;
        for line in lines {
            let galley = p.layout(line.clone(), FontId::new(19.0, FontFamily::Name("label".into())), ink, right - left);
            if *y + galley.size().y > screen.bottom() - 70.0 {
                break;
            }
            let h = galley.size().y;
            p.galley(pos2(left, *y), galley, ink);
            *y += h + 6.0;
        }
        *y += 22.0;
    };
    section("NOW PLAYING", &dash.playing.clone().into_iter().collect::<Vec<_>>(), &mut y);
    section("NEXT", &dash.next, &mut y);
    section("NOTES", &dash.notes, &mut y);
    if let Some((total, apps)) = &dash.day {
        let mut lines = vec![format!("{total} at the screen today")];
        lines.extend(apps.iter().map(|(app, span)| format!("{app}  \u{00B7}  {span}")));
        section("TODAY", &lines, &mut y);
    }
    section("LATELY", &dash.notices, &mut y);
    // Whom he answers to, and the other short states, along the foot.
    p.text(pos2(screen.center().x, screen.bottom() - 44.0), Align2::CENTER_CENTER, dash.states.join("     \u{00B7}     "), FontId::monospace(11.0), muted);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_takes_the_upright_monitor_the_board_is_not_on() {
        let (main, side, tall) = ([0, 0, 2560, 1440], [2560, 0, 5120, 1440], [-1080, -290, 0, 1630]);
        let all = [(main, true), (side, false), (tall, false)];
        assert_eq!(upright(&all, Some(main), true), Some(tall));
        assert_eq!(upright(&all, Some(tall), true), None, "the board is on it");
        assert_eq!(upright(&all, Some(tall), false), Some(tall), "as the orb he holds no monitor");
        assert_eq!(upright(&[(main, true), (side, false)], Some(main), true), None, "no monitor stands upright");
    }

    #[test]
    fn it_takes_a_monitor_the_board_is_not_on() {
        let (main, side) = ([0, 0, 2560, 1440], [2560, 0, 4480, 1080]);
        let both = [(main, true), (side, false)];
        assert_eq!(target(&both, Some(main), true), Some(side), "the board is on the main monitor");
        assert_eq!(target(&both, Some(side), true), Some(main), "the board is on the second one");
        assert_eq!(target(&both, None, true), Some(side), "a secondary one before the primary");
        assert_eq!(target(&[(main, true)], Some(main), true), None, "one monitor holds the board");
        assert_eq!(target(&[], None, true), None);
        // As the orb or the slim strip he holds no monitor: the only one is free for it too.
        assert_eq!(target(&[(main, true)], None, false), Some(main));
        assert_eq!(target(&both, None, false), Some(side));
    }
}
