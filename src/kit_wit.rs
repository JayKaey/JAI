//! A dry line now and then, the way the films' J.A.R.V.I.S. has one (2026-11-16): tied to what
//! happens (a long stretch at the PC, the same build failing again, a hot GPU), at most once an
//! hour, never in a game, at night, in a call or over a full-screen app. Each line is kept in
//! the kit's store once said, so none comes twice; an event whose lines are used up stays quiet.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

/// Lines already said: key = the line's id.
const SAID: &str = "wit_said";
/// One line an hour at most.
const GAP_SECS: i64 = 3600;
/// A stretch at the PC that earns a line.
const LONG_SESSION_MINUTES: u64 = 180;
/// Idle this long ends a stretch.
const BREAK_SECS: u64 = 10 * 60;
const HOT_GPU_C: f32 = 83.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Hours at the PC without a break.
    LongSession(u64),
    /// The same project's build failed this many times in a row.
    BuildFailedAgain(u32),
    /// The GPU's temperature.
    HotGpu(f32),
}

fn lines(event: Event) -> Vec<(&'static str, String)> {
    match event {
        Event::LongSession(h) => vec![
            ("long_1", format!("{h} hours without a break, Sir. I'd suggest water, but I know how that goes.")),
            ("long_2", format!("{h} hours at the desk, Sir. The chair would like a word.")),
            ("long_3", "Still here, Sir. So am I, though I have the advantage of not needing to stretch.".to_string()),
        ],
        Event::BuildFailedAgain(n) => vec![
            ("build_1", format!("That is the same build failing {n} times, Sir. Persistence is admirable; the compiler remains unconvinced.")),
            ("build_2", format!("Attempt {n}, Sir. The compiler and I are developing a rapport.")),
            ("build_3", "Another red build, Sir. Shall I start keeping a scrapbook?".to_string()),
        ],
        Event::HotGpu(t) => vec![
            ("gpu_1", format!("The GPU is at {t:.0} degrees, Sir. Toasty, but within tolerance.")),
            ("gpu_2", format!("{t:.0} degrees on the GPU, Sir. I could fry an egg, were I so equipped.")),
        ],
    }
}

/// The first line of the event not said yet.
fn next_line(event: Event, said: &[String]) -> Option<(&'static str, String)> {
    lines(event).into_iter().find(|(id, _)| !said.iter().any(|s| s == id))
}

/// Whether a line may be said now (`last`: when the last one was, seconds).
fn may_speak(now: i64, last: i64, in_game: bool, night: bool, in_call: bool, busy: bool) -> bool {
    now - last >= GAP_SECS && !in_game && !night && !in_call && !busy
}

static LAST: AtomicI64 = AtomicI64::new(i64::MIN / 2);

/// Something happened that may earn a line: said (through the announcements) when the rules
/// allow and a fresh line is left.
pub fn on(event: Event) {
    if crate::platform::hands_off() {
        return;
    }
    let now = chrono::Local::now().timestamp();
    let busy = crate::win32::foreground_is_fullscreen();
    if !may_speak(now, LAST.load(Ordering::Acquire), crate::vram_guard::in_game(), crate::hud_feel::night_active(), crate::discord_bot::in_call(), busy) {
        return;
    }
    let said: Vec<String> = crate::kit::list(SAID).into_iter().map(|(key, _, _)| key).collect();
    let Some((id, line)) = next_line(event, &said) else { return };
    LAST.store(now, Ordering::Release);
    let _ = crate::kit::put(SAID, id, &serde_json::json!(true));
    tracing::info!("[WIT] {id}");
    crate::announcements::say(line);
}

/// Watches the stretch at the PC and the GPU's heat (once a minute, from the telemetry feed).
pub fn start(mut telemetry: tokio::sync::watch::Receiver<crate::telemetry::SystemTelemetry>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut active_minutes = 0u64;
        loop {
            tick.tick().await;
            match crate::win32::idle_seconds() {
                Some(idle) if idle < BREAK_SECS => active_minutes += 1,
                _ => active_minutes = 0,
            }
            if active_minutes >= LONG_SESSION_MINUTES && active_minutes % 60 == 0 {
                on(Event::LongSession(active_minutes / 60));
            }
            let hot = telemetry.borrow_and_update().gpu.as_ref().and_then(|g| g.temperature_c);
            if let Some(t) = hot.filter(|t| *t >= HOT_GPU_C) {
                on(Event::HotGpu(t));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_said_once_and_an_event_runs_dry() {
        let mut said: Vec<String> = Vec::new();
        let mut heard = Vec::new();
        while let Some((id, line)) = next_line(Event::BuildFailedAgain(3), &said) {
            said.push(id.to_string());
            heard.push(line);
        }
        assert_eq!(heard.len(), 3, "three lines, then quiet");
        assert!(heard[0].contains("3 times"));
        assert_eq!(next_line(Event::BuildFailedAgain(4), &said), None, "never a line twice");
        assert!(next_line(Event::HotGpu(86.4), &said).is_some_and(|(_, l)| l.contains("86 degrees")));
    }

    #[test]
    fn never_in_a_game_at_night_in_a_call_over_a_full_screen_or_twice_an_hour() {
        let (now, long_ago) = (10_000_000, 0);
        assert!(may_speak(now, long_ago, false, false, false, false));
        assert!(!may_speak(now, now - 1800, false, false, false, false), "within the hour");
        assert!(!may_speak(now, long_ago, true, false, false, false), "in a game");
        assert!(!may_speak(now, long_ago, false, true, false, false), "at night");
        assert!(!may_speak(now, long_ago, false, false, true, false), "in a call");
        assert!(!may_speak(now, long_ago, false, false, false, true), "over a full-screen app");
    }
}
