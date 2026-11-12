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
}

static BACKDROP: Mutex<Option<Backdrop>> = Mutex::new(None);
/// What came of asking the desktop: Ok once it is its background, or why not.
static ATTACHED: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// The monitor the background takes: the first that the board (`hud`, its monitor's rect) is not
/// on, a secondary one before the primary. None with a single monitor (the board needs it).
pub fn target(monitors: &[([i32; 4], bool)], hud: Option<[i32; 4]>) -> Option<[i32; 4]> {
    let free = |primary: bool| monitors.iter().find(|(rect, p)| *p == primary && Some(*rect) != hud).map(|(rect, _)| *rect);
    if monitors.len() < 2 {
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

/// Shows the background while `on` (each HUD frame). `hud`: the HUD's own window (the monitor
/// it is on is left alone); `spanning`: the board is across all of them; `state`: what he is
/// doing.
pub fn show(ctx: &egui::Context, on: bool, hud: isize, spanning: bool, state: &str) -> Option<Refused> {
    if !on {
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
        let Some(monitor) = named.or_else(|| (!spanning).then(|| target(&monitors, monitor_of(hud))).flatten()) else {
            return Some(Refused(if spanning { "My board is across every monitor: there is no desktop left for me to stand behind.".into() } else { "I need a second monitor for that: this one holds my board.".into() }));
        };
        tracing::info!("[DESKTOP] The live background opens on the monitor at {},{} ({}×{})", monitor[0], monitor[1], monitor[2] - monitor[0], monitor[3] - monitor[1]);
        *guard = Some(Backdrop { emblem: RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem), monitor, opened: Instant::now(), last_frame: Instant::now(), asked: false });
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
    ctx.show_viewport_immediate(ViewportId::from_hash_of("jai_desktop_backdrop"), builder, |ctx, _class| {
        paint(ctx, emblem, state);
        ctx.request_repaint_after(Duration::from_millis(if gaming { 1000 } else { 100 }));
    });
    // Once the window exists: the desktop is asked to take it behind its icons.
    if !b.asked && b.opened.elapsed() > Duration::from_millis(400) {
        b.asked = true;
        let monitor = b.monitor;
        attach(monitor);
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
            Some(hwnd) => crate::win32::attach_behind_desktop_icons(hwnd, monitor),
            None => Err("its window did not open".to_string()),
        };
        match &result {
            Ok(()) => tracing::info!("[DESKTOP] The desktop took the live background behind its icons"),
            Err(why) => tracing::warn!("[DESKTOP] The live background was not taken: {why}"),
        }
        *lock(&ATTACHED) = Some(result);
    });
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_takes_a_monitor_the_board_is_not_on() {
        let (main, side) = ([0, 0, 2560, 1440], [2560, 0, 4480, 1080]);
        let both = [(main, true), (side, false)];
        assert_eq!(target(&both, Some(main)), Some(side), "the board is on the main monitor");
        assert_eq!(target(&both, Some(side)), Some(main), "the board is on the second one");
        assert_eq!(target(&both, None), Some(side), "a secondary one before the primary");
        assert_eq!(target(&[(main, true)], Some(main)), None, "one monitor holds the board");
        assert_eq!(target(&[], None), None);
    }
}
