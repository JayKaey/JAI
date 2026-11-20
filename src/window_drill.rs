//! `--window-drill`: the real HUD window driven the way Sir drives it, and what Windows then
//! shows checked against his monitors. The SCREENS menu is opened from the header and its
//! buttons are clicked; his cursor and keyboard are never touched. A click goes into the
//! window's own pointer input (a press and a release on the button's middle). After each, the
//! window's real place is read back from Windows: `[ MOVE HERE ]` puts the fullscreen board on
//! that monitor, `[ SPAN ALL MONITORS ]` stretches it over all of them, `[ BACK TO ONE MONITOR ]`
//! brings it to the first, `[ CLOSE ]` closes the menu. (That is the board of a scripted session
//! without a monitor of its own: everywhere else he is the desktop's background.)
//!
//! (Posted mouse messages were tried first: once the window has left the monitor under his
//! resting cursor, Windows answers each posted move with "the mouse left", and the press that
//! follows lands nowhere.)
//!
//! It runs in a silent session with the real window (`--silent --show --window-drill --chat
//! "…"`): the settings file is never written, nothing is heard. Every step is a `[DRILL]` line
//! in jai.log, and the session ends when the drill is through.

use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

static ENABLED: AtomicBool = AtomicBool::new(false);
static FINISHED: AtomicBool = AtomicBool::new(false);
/// What the last HUD frame drew that the drill can click, by name ("header:screens",
/// "screens:move:2", "screens:span:on", "screens:span:off", "screens:close"), in points.
static MARKS: Mutex<Vec<(String, egui::Rect)>> = Mutex::new(Vec::new());
/// Pointer input waiting for the window's next frame.
static INPUT: Mutex<Vec<egui::Event>> = Mutex::new(Vec::new());

/// Turns the drill on for this session (before the HUD's first frame).
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// The drill is on and not through yet (the silent session waits for it).
pub fn running() -> bool {
    enabled() && !FINISHED.load(Ordering::Acquire)
}

fn marks() -> std::sync::MutexGuard<'static, Vec<(String, egui::Rect)>> {
    MARKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A frame begins: what the previous one drew is forgotten.
pub fn begin_frame() {
    if enabled() {
        marks().clear();
    }
}

/// Something clickable was drawn at `rect` this frame.
pub fn mark(name: &str, rect: egui::Rect) {
    if enabled() {
        let mut all = marks();
        all.retain(|(n, _)| n != name);
        all.push((name.to_string(), rect));
    }
}

fn drawn(name: &str) -> Option<egui::Rect> {
    marks().iter().find(|(n, _)| n == name).map(|(_, r)| *r)
}

fn input() -> std::sync::MutexGuard<'static, Vec<egui::Event>> {
    INPUT.lock().unwrap_or_else(|e| e.into_inner())
}

/// The window's next frame begins: the drill's pointer input joins the real input (the main
/// window only; the detached blocks are left alone).
pub fn feed(raw: &mut egui::RawInput) {
    if enabled() && raw.viewport_id == egui::ViewportId::ROOT {
        raw.events.append(&mut input());
    }
}

/// A left button going down (or up) on `pos`, the pointer there first; after the release the
/// pointer leaves again, as his resting cursor is elsewhere.
fn click_events(pos: egui::Pos2, down: bool) -> Vec<egui::Event> {
    let button = egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE };
    let mut events = vec![egui::Event::PointerMoved(pos), button];
    if !down {
        events.push(egui::Event::PointerGone);
    }
    events
}

/// What a step does.
#[derive(Debug, Clone)]
enum Act {
    /// A left click in the middle of the named target.
    Click(String),
}

/// What must hold afterwards (and keep holding for a moment).
#[derive(Debug, Clone, PartialEq)]
enum Expect {
    Drawn(String),
    NotDrawn(String),
    /// The window covers exactly this monitor (1-based).
    Covers(usize),
    /// The window covers exactly the box around every monitor.
    SpansAll,
}

#[derive(Debug, Clone)]
struct Step {
    why: String,
    act: Act,
    expect: Expect,
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Waiting for the window to be up and settled.
    Starting,
    /// Waiting for the step's target to be drawn.
    Finding { deadline: Instant },
    /// The button (or key) is down; it is released at `at` (a button on `pos`).
    Pressing { at: Instant, pos: egui::Pos2 },
    /// Waiting for the expectation; it has held since `since`.
    Checking { deadline: Instant, since: Option<Instant> },
    /// A breath between steps.
    Pause { until: Instant },
    Done,
}

/// How long an expectation must hold before it counts (the fullscreen board re-checks its
/// place every 1.5 s, so a place that drifts back shows within this).
const HOLD: Duration = Duration::from_millis(1800);
/// How long a step may take to come true.
const STEP_TIME: Duration = Duration::from_secs(8);
/// The whole drill, at most.
const DRILL_TIME: Duration = Duration::from_secs(240);

pub struct Drill {
    started: Instant,
    steps: Vec<Step>,
    at: usize,
    phase: Phase,
    passed: usize,
    failed: Vec<String>,
}

impl Default for Drill {
    fn default() -> Self {
        Self::new()
    }
}

impl Drill {
    pub fn new() -> Self {
        Self { started: Instant::now(), steps: Vec::new(), at: 0, phase: Phase::Starting, passed: 0, failed: Vec::new() }
    }

    /// One HUD frame: the drill looks at what the previous frame drew and at the window, and
    /// takes its next step. Call it at the very start of the frame, before `begin_frame`.
    pub fn step(&mut self, ctx: &egui::Context, hwnd: isize) {
        if matches!(self.phase, Phase::Done) {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(40));
        let now = Instant::now();
        if now.duration_since(self.started) > DRILL_TIME {
            self.failed.push(format!("the drill ran out of time at step {} of {}", self.at + 1, self.steps.len()));
            self.finish();
            return;
        }
        match self.phase {
            Phase::Starting => {
                // The window and its first frames (the fullscreen board takes it over at once).
                if hwnd == 0 || now.duration_since(self.started) < Duration::from_secs(6) {
                    return;
                }
                self.steps = plan(hwnd);
                info!("[DRILL] {} steps on {}: {}", self.steps.len(), describe_monitors(), self.steps.iter().map(|s| s.why.as_str()).collect::<Vec<_>>().join(" → "));
                self.begin_step(now);
            }
            Phase::Finding { deadline } => {
                let step = self.steps[self.at].clone();
                match step.act {
                    Act::Click(target) => match drawn(&target) {
                        Some(rect) => {
                            input().extend(click_events(rect.center(), true));
                            ctx.request_repaint();
                            self.phase = Phase::Pressing { at: now + Duration::from_millis(90), pos: rect.center() };
                        }
                        None if now > deadline => {
                            self.fail(&format!("{target} was never drawn"));
                        }
                        None => {}
                    },
                }
            }
            Phase::Pressing { at, pos } => {
                if now < at {
                    return;
                }
                input().extend(click_events(pos, false));
                ctx.request_repaint();
                self.phase = Phase::Checking { deadline: now + STEP_TIME, since: None };
            }
            Phase::Checking { deadline, since } => {
                let expect = self.steps[self.at].expect.clone();
                if holds(&expect, hwnd) {
                    let since = since.unwrap_or(now);
                    if now.duration_since(since) >= HOLD {
                        info!("[DRILL] PASS {} ({})", self.steps[self.at].why, seen(hwnd));
                        self.passed += 1;
                        self.next(now);
                    } else {
                        self.phase = Phase::Checking { deadline, since: Some(since) };
                    }
                } else if now > deadline {
                    self.fail(&format!("expected {expect:?}, the window is {}", seen(hwnd)));
                } else {
                    self.phase = Phase::Checking { deadline, since: None };
                }
            }
            Phase::Pause { until } => {
                if now >= until {
                    self.begin_step(now);
                }
            }
            Phase::Done => {}
        }
    }

    fn begin_step(&mut self, now: Instant) {
        if self.at >= self.steps.len() {
            self.finish();
            return;
        }
        info!("[DRILL] Step {} of {}: {}", self.at + 1, self.steps.len(), self.steps[self.at].why);
        self.phase = Phase::Finding { deadline: now + STEP_TIME };
    }

    fn next(&mut self, now: Instant) {
        self.at += 1;
        self.phase = Phase::Pause { until: now + Duration::from_millis(500) };
    }

    fn fail(&mut self, what: &str) {
        let line = format!("{}: {what}", self.steps[self.at].why);
        warn!("[DRILL] FAIL {line}");
        self.failed.push(line);
        self.next(Instant::now());
    }

    fn finish(&mut self) {
        self.phase = Phase::Done;
        let total = self.passed + self.failed.len();
        if self.failed.is_empty() {
            info!("[DRILL] All {total} steps passed in {:.0} s", self.started.elapsed().as_secs_f32());
        } else {
            warn!("[DRILL] {} of {total} steps passed; failed: {}", self.passed, self.failed.join(" | "));
        }
        FINISHED.store(true, Ordering::Release);
    }
}

/// The drill for these monitors, from the shape the window is in now: first to the fullscreen
/// board on one monitor (`[ BACK TO ONE MONITOR ]` from a span, by way of the span from a
/// window), then every `[ MOVE HERE ]` and back, the span and back, and `[ CLOSE ]`.
fn plan(hwnd: isize) -> Vec<Step> {
    let monitors = crate::monitors::list();
    let primary = crate::monitors::primary(&monitors).map_or(1, |m| m.index);
    let state = window_rect(hwnd);
    let spanning = monitors.len() > 1 && state.zip(crate::monitors::virtual_bounds(&monitors)).is_some_and(|((r, _), b)| close(r, b));
    let windowed = monitors.len() > 1 && !spanning && (state.is_some_and(|(_, maximised)| maximised) || state.is_some_and(|(r, _)| !monitors.iter().any(|m| close(r, as_xywh(m.rect)))));
    let centre = state.map(|(r, _)| (r[0] + r[2] / 2, r[1] + r[3] / 2));
    let mut here = centre.and_then(|(x, y)| crate::monitors::monitor_at(&monitors, x, y)).unwrap_or(primary);
    let click = |why: String, target: String, expect: Expect| Step { why, act: Act::Click(target), expect };
    let mut steps = vec![click("[ SCREENS ] opens".into(), "header:screens".into(), Expect::Drawn("screens:close".into()))];
    if windowed {
        steps.push(click("[ SPAN ALL MONITORS ] (from the window it started in)".into(), "screens:span:on".into(), Expect::SpansAll));
    }
    if spanning || windowed {
        steps.push(click("[ BACK TO ONE MONITOR ]: the fullscreen board".into(), "screens:span:off".into(), Expect::Covers(primary)));
        here = primary;
    }
    let start = here;
    for m in monitors.iter().map(|m| m.index).filter(|i| *i != start) {
        steps.push(click(format!("[ MOVE HERE ] on display {m}"), format!("screens:move:{m}"), Expect::Covers(m)));
        here = m;
    }
    if here != start {
        steps.push(click(format!("[ MOVE HERE ] back on display {start}"), format!("screens:move:{start}"), Expect::Covers(start)));
    }
    if monitors.len() > 1 {
        steps.push(click("SPAN ALL MONITORS".into(), "screens:span:on".into(), Expect::SpansAll));
        steps.push(click("BACK TO ONE MONITOR".into(), "screens:span:off".into(), Expect::Covers(primary)));
    }
    steps.push(click("[ CLOSE ] closes the menu".into(), "screens:close".into(), Expect::NotDrawn("screens:close".into())));
    steps
}

fn as_xywh([l, t, r, b]: [i32; 4]) -> [i32; 4] {
    [l, t, r - l, b - t]
}

fn close(a: [i32; 4], b: [i32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2)
}

/// The window's rect (x, y, w, h) as Windows shows it, and whether it is maximised (None:
/// minimised or gone).
fn window_rect(hwnd: isize) -> Option<([i32; 4], bool)> {
    #[cfg(windows)]
    use crate::win32 as wm;
    #[cfg(not(windows))]
    use crate::x11 as wm;
    wm::window_state(hwnd).filter(|(_, _, minimised)| !minimised).map(|(r, maximised, _)| (r, maximised))
}

fn holds(expect: &Expect, hwnd: isize) -> bool {
    let monitors = crate::monitors::list();
    match expect {
        Expect::Drawn(name) => drawn(name).is_some(),
        Expect::NotDrawn(name) => drawn(name).is_none(),
        Expect::Covers(index) => {
            let Some(m) = monitors.iter().find(|m| m.index == *index) else { return false };
            window_rect(hwnd).is_some_and(|(r, maximised)| !maximised && close(r, as_xywh(m.rect)))
        }
        Expect::SpansAll => crate::monitors::virtual_bounds(&monitors).zip(window_rect(hwnd)).is_some_and(|(b, (r, _))| close(r, b)),
    }
}

/// The window as Windows shows it, for the log.
fn seen(hwnd: isize) -> String {
    match window_rect(hwnd) {
        Some(([x, y, w, h], maximised)) => {
            let monitors = crate::monitors::list();
            let on = crate::monitors::monitor_at(&monitors, x + w / 2, y + h / 2).map_or("no monitor".to_string(), |i| format!("display {i}"));
            format!("{w}x{h} at {x},{y} on {on}{}", if maximised { ", maximised" } else { "" })
        }
        None => "minimised or gone".to_string(),
    }
}

fn describe_monitors() -> String {
    let monitors = crate::monitors::list();
    monitors
        .iter()
        .map(|m| format!("display {} {}x{} at {},{}{}", m.index, m.width(), m.height(), m.rect[0], m.rect[1], if m.primary { " (primary)" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_is_a_press_then_a_release_on_the_same_spot() {
        let at = egui::pos2(812.5, 40.0);
        let down = click_events(at, true);
        assert_eq!(down.len(), 2);
        assert!(matches!(down[0], egui::Event::PointerMoved(p) if p == at));
        assert!(matches!(down[1], egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } if pos == at));
        let up = click_events(at, false);
        assert!(matches!(up[1], egui::Event::PointerButton { pos, pressed: false, .. } if pos == at));
        // His cursor rests elsewhere: the pointer leaves after the click.
        assert!(matches!(up.last(), Some(egui::Event::PointerGone)));
    }

    #[test]
    fn marks_are_kept_only_while_the_drill_is_on() {
        // Off (as in every other session): nothing is kept.
        if !enabled() {
            mark("header:screens", egui::Rect::from_min_size(egui::pos2(1.0, 2.0), egui::vec2(3.0, 4.0)));
            assert!(drawn("header:screens").is_none());
        }
    }
}
