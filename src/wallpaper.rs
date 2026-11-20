//! J.A.I. as the desktop's background (Sir, 2026-11-13: "I want Jay to basically only be my PC
//! background and all the windows he can create should … be detached windows … when I talk to
//! JAI but he isn't visible his orb should detach from the background and move to the left down
//! corner of the monitor I am using … and when he isn't used the orb should move back to the
//! background after 5 seconds. But he shouldn't teleport windows and his orb around").
//!
//! The board's own window, spread over the monitors, is handed to the desktop as the layer
//! between its wallpaper and its icons: it is the board across every monitor, drawn by the very
//! same code. The desktop lets no click through to it, so a mouse hook passes the pointer on
//! while it is over the bare desktop: a press on one of his keys is his alone, every other
//! click stays the desktop's. Nothing here draws: `gui` does, and asks this module where things
//! stand.

use eframe::egui::{self, pos2, vec2, Rect};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the background stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Standing {
    /// He is the board, in a window.
    Off,
    /// The window is being made plain and put in place; the desktop is asked next.
    Arriving,
    /// The desktop has it behind its icons.
    On,
    /// The desktop would not take it (why). It is asked again every `ASK_AGAIN`.
    #[cfg_attr(not(windows), allow(dead_code))]
    Refused(String),
}

struct State {
    standing: Standing,
    since: Instant,
    /// The desktop was asked (once per arrival, and again when the rect changes).
    #[cfg_attr(not(windows), allow(dead_code))]
    asked: bool,
    hwnd: isize,
    /// Where it stands on the screen: left, top, right, bottom.
    rect: [i32; 4],
    looked: Instant,
    ppp: f32,
    /// The pointer over the desktop, as the board is to see it in its next frame.
    events: Vec<egui::Event>,
    over: bool,
    /// The buttons whose press was his (their release is his too).
    held: u8,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// What came of asking the desktop (from the Win32 worker).
#[cfg(windows)]
static ASKED: Mutex<Option<Result<(), String>>> = Mutex::new(None);
/// Read by the mouse hook without a lock: he is the background; one of his keys is under the
/// pointer; a game is in front.
static ON: AtomicBool = AtomicBool::new(false);
static HOT: AtomicBool = AtomicBool::new(false);
static GAME: AtomicBool = AtomicBool::new(false);
/// Another program's window hides the background's emblem (`hidden_now`).
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// How long the window is given to become plain and stand in place before the desktop is asked.
const SETTLE: Duration = Duration::from_millis(350);
/// How often its place in the desktop is looked at.
const LOOK_EVERY: Duration = Duration::from_secs(2);
/// How long after a refusal the desktop is asked again (Explorer may have been starting).
#[cfg(windows)]
const ASK_AGAIN: Duration = Duration::from_secs(30);
/// The emblem counts as hidden when one window covers more than this share of it, and as
/// seen again once less than `SEEN_BELOW` of it is covered. (Between the two it stays what it
/// was: a window half over it does not send the orb to and fro.)
const HIDDEN_FROM: f32 = 0.3;
const SEEN_BELOW: f32 = 0.12;

/// Whether the emblem is hidden, with the `share` of it (0 to 1) that a window covers now.
fn hidden_now(was: bool, share: f32) -> bool {
    share > HIDDEN_FROM || (was && share >= SEEN_BELOW)
}

/// The rect (left, top, right, bottom) the background covers: the box around every monitor, or
/// Sir's first one. None without a monitor.
pub fn rect_for(all: bool, monitors: &[crate::monitors::Monitor]) -> Option<[i32; 4]> {
    if all {
        crate::monitors::virtual_bounds(monitors).map(|[x, y, w, h]| [x, y, x + w, y + h])
    } else {
        crate::monitors::primary(monitors).map(|m| m.rect)
    }
}

/// Whether the background is asked for or standing (the board is no window then).
pub fn in_use() -> bool {
    lock(&STATE).as_ref().is_some_and(|s| matches!(s.standing, Standing::Arriving | Standing::On))
}

/// One step, from every frame of the board. `want`: the rect the background is to cover (None:
/// he is the board); `ppp`: the window's pixels per point; `gaming`: a game is in front (its
/// mouse is left alone). Returns where the background stands; `gui` sends the window's own
/// commands when that changes.
#[cfg(windows)]
pub fn tick(hwnd: isize, want: Option<[i32; 4]>, ppp: f32, gaming: bool) -> Standing {
    use crate::win32_desktop as desk;
    GAME.store(gaming, Ordering::Release);
    let mut guard = lock(&STATE);
    let now = Instant::now();
    let s = guard.get_or_insert_with(|| State { standing: Standing::Off, since: now, asked: false, hwnd, rect: [0; 4], looked: now, ppp, events: Vec::new(), over: false, held: 0 });
    s.ppp = ppp.max(0.1);
    s.hwnd = hwnd;
    let Some(rect) = want.filter(|_| hwnd != 0) else {
        if matches!(s.standing, Standing::On | Standing::Arriving) {
            let was_on = s.standing == Standing::On;
            ON.store(false, Ordering::Release);
            HOT.store(false, Ordering::Release);
            desk::mouse_hook(false);
            s.events.clear();
            (s.over, s.held) = (false, 0);
            if was_on {
                // Back out of the desktop, on the window's own thread: a plain window on Sir's
                // first monitor, which `gui` then gives its shape.
                // (A scripted session's: on that session's own monitor, never on Sir's.)
                let monitors = crate::monitors::list();
                let own = crate::gui::test_session_monitor().and_then(|n| monitors.iter().find(|m| m.index == n));
                let to = own.or_else(|| crate::monitors::primary(&monitors)).map_or([80, 60, 1280, 720], |m| {
                    let inset = (m.width().min(m.height()) / 8).max(40);
                    [m.rect[0] + inset, m.rect[1] + inset, m.width() - 2 * inset, m.height() - 2 * inset]
                });
                match desk::detach(hwnd, to) {
                    Ok(()) => tracing::info!("[DESKTOP] He is the board again: his window has left the desktop"),
                    Err(e) => tracing::warn!("[DESKTOP] His window could not leave the desktop: {e}"),
                }
            }
        }
        (s.standing, s.since, s.asked) = (Standing::Off, now, false);
        return Standing::Off;
    };
    match s.standing.clone() {
        Standing::Off => {
            (s.standing, s.since, s.asked, s.rect) = (Standing::Arriving, now, false, rect);
            *lock(&ASKED) = None;
            tracing::info!("[DESKTOP] He becomes the desktop's background: {}×{} at {},{}", rect[2] - rect[0], rect[3] - rect[1], rect[0], rect[1]);
        }
        Standing::Arriving => {
            if !s.asked && s.since.elapsed() > SETTLE {
                s.asked = true;
                s.rect = rect;
                ask(hwnd, rect);
            }
            match lock(&ASKED).take() {
                Some(Ok(())) => {
                    // (Looked at again soon: at the start the window library shows the window
                    // once more after its first frame, and writes its styles anew as it does.)
                    (s.standing, s.since, s.looked) = (Standing::On, now, now.checked_sub(LOOK_EVERY - Duration::from_millis(400)).unwrap_or(now));
                    ON.store(true, Ordering::Release);
                    tracing::info!("[DESKTOP] The desktop took him behind its icons ({})", crate::win32::backdrop_standing(hwnd));
                }
                Some(Err(why)) => {
                    tracing::warn!("[DESKTOP] The desktop would not take him: {why}");
                    (s.standing, s.since) = (Standing::Refused(why), now);
                }
                None => {}
            }
        }
        Standing::On => {
            desk::mouse_hook(!gaming);
            // (An answer to a second asking: a refusal now is only logged, he stays.)
            if let Some(Err(why)) = lock(&ASKED).take() {
                tracing::warn!("[DESKTOP] His place behind the icons could not be put right: {why}");
            }
            if rect != s.rect {
                // The monitors changed, or he is to cover all of them (or one) from now on.
                tracing::info!("[DESKTOP] The background now covers {}×{} at {},{}", rect[2] - rect[0], rect[3] - rect[1], rect[0], rect[1]);
                s.rect = rect;
                ask(hwnd, rect);
            } else if s.looked.elapsed() > LOOK_EVERY {
                s.looked = now;
                crate::win32::run_detached(move || {
                    desk::learn_desktop();
                    match desk::standing(hwnd) {
                        // (Gone with the desktop: `gui` opens the window anew.)
                        None => {}
                        Some((true, true, at)) if at == rect => {
                            if let Ok(true) = crate::win32::keep_behind_desktop_icons(hwnd) {
                                tracing::info!("[DESKTOP] Something had come between the icons and him; he is right under them again");
                            }
                        }
                        Some((hung, styled, at)) => {
                            tracing::info!("[DESKTOP] His window had left its place in the desktop (in it: {hung}, as the desktop needs it: {styled}, at {at:?}): put back");
                            *lock(&ASKED) = Some(crate::win32::attach_behind_desktop_icons(hwnd, rect));
                        }
                    }
                });
            }
        }
        // He is always the background: the desktop is asked again until it takes him.
        Standing::Refused(_) => match lock(&ASKED).take() {
            Some(Ok(())) => {
                (s.standing, s.since, s.looked) = (Standing::On, now, now.checked_sub(LOOK_EVERY).unwrap_or(now));
                ON.store(true, Ordering::Release);
                tracing::info!("[DESKTOP] The desktop took him behind its icons after all ({})", crate::win32::backdrop_standing(hwnd));
            }
            Some(Err(why)) => tracing::warn!("[DESKTOP] Asked again, the desktop would still not take him: {why}"),
            None if s.since.elapsed() > ASK_AGAIN => {
                (s.since, s.rect) = (now, rect);
                ask(hwnd, rect);
            }
            None => {}
        },
    }
    s.standing.clone()
}

/// KDE (his windows are X11 ones, under XWayland): no desktop there takes a window behind its
/// icons, so his stands under every other window instead, over the rect, on every virtual
/// desktop and off the task manager; Plasma's own desktop lies behind him then. His keys take
/// their clicks as any window's do: no hook.
#[cfg(not(windows))]
pub fn tick(hwnd: isize, want: Option<[i32; 4]>, ppp: f32, gaming: bool) -> Standing {
    GAME.store(gaming, Ordering::Release);
    let mut guard = lock(&STATE);
    let now = Instant::now();
    let s = guard.get_or_insert_with(|| State { standing: Standing::Off, since: now, asked: false, hwnd, rect: [0; 4], looked: now, ppp, events: Vec::new(), over: false, held: 0 });
    (s.ppp, s.hwnd) = (ppp.max(0.1), hwnd);
    let Some(rect) = want.filter(|_| hwnd != 0) else {
        if matches!(s.standing, Standing::On | Standing::Arriving) {
            // (Back as a window: `gui` gives it its place and shape.)
            stand(hwnd, None);
            tracing::info!("[DESKTOP] He is the board again: his window is a window again");
        }
        (s.standing, s.since) = (Standing::Off, now);
        return Standing::Off;
    };
    match s.standing.clone() {
        Standing::Off => {
            (s.standing, s.since, s.rect) = (Standing::Arriving, now, rect);
            tracing::info!("[DESKTOP] He becomes the desktop's background: {}×{} at {},{}", rect[2] - rect[0], rect[3] - rect[1], rect[0], rect[1]);
        }
        Standing::Arriving if s.since.elapsed() > SETTLE => {
            stand(hwnd, Some(rect));
            (s.standing, s.since, s.rect, s.looked) = (Standing::On, now, rect, now);
            tracing::info!("[DESKTOP] He stands under every window as the desktop's background");
        }
        // Put in place again every look: the monitors change, and KWin takes no state of a
        // window it does not show yet (at the start it may not).
        Standing::On if rect != s.rect || s.looked.elapsed() > LOOK_EVERY => {
            if rect != s.rect {
                tracing::info!("[DESKTOP] The background now covers {}×{} at {},{}", rect[2] - rect[0], rect[3] - rect[1], rect[0], rect[1]);
            }
            (s.rect, s.looked) = (rect, now);
            stand(hwnd, Some(rect));
        }
        _ => {}
    }
    s.standing.clone()
}

/// His window under every other one, on every virtual desktop and off the task manager, over
/// `rect` (left, top, right, bottom); None undoes that.
#[cfg(not(windows))]
pub fn stand(hwnd: isize, rect: Option<[i32; 4]>) {
    use crate::x11 as wm;
    let on = rect.is_some();
    wm::keep_below(hwnd, on);
    wm::skip_taskbar(hwnd, on);
    wm::sticky(hwnd, on);
    if let Some([l, t, r, b]) = rect {
        wm::place(hwnd, [l, t, r - l, b - t]);
        wm::sink(hwnd);
    }
}

/// Asks the desktop to take window `hwnd` behind its icons at `rect` (on the Win32 worker: the
/// desktop is sent a message there and may take its time). The answer lands in `ASKED`.
#[cfg(windows)]
fn ask(hwnd: isize, rect: [i32; 4]) {
    use crate::win32_desktop as desk;
    crate::win32::run_detached(move || {
        desk::before_attach(hwnd);
        let result = crate::win32::attach_behind_desktop_icons(hwnd, rect);
        desk::learn_desktop();
        *lock(&ASKED) = Some(result);
        crate::gui::refresh();
    });
}

/// He starts as the desktop's background: the desktop is asked at once, before the window is
/// first shown, so the board never lies over Sir's windows on its way there. (Under KDE `tick`
/// puts the window in place once it is on the screen.)
pub fn arrive(hwnd: isize, rect: [i32; 4], ppp: f32) {
    let now = Instant::now();
    *lock(&STATE) = Some(State { standing: Standing::Arriving, since: now, asked: true, hwnd, rect, looked: now, ppp: ppp.max(0.1), events: Vec::new(), over: false, held: 0 });
    tracing::info!("[DESKTOP] He starts as the desktop's background: {}×{} at {},{}", rect[2] - rect[0], rect[3] - rect[1], rect[0], rect[1]);
    #[cfg(windows)]
    {
        *lock(&ASKED) = None;
        ask(hwnd, rect);
    }
}

/// The window is gone (the desktop took it down with it, as when Explorer starts again):
/// nothing of it stands any more.
#[cfg(windows)]
pub fn lost() {
    ON.store(false, Ordering::Release);
    HOT.store(false, Ordering::Release);
    crate::win32_desktop::mouse_hook(false);
    *lock(&STATE) = None;
    *lock(&ASKED) = None;
}

// ------------------------------------------------------------------------- the pointer ----

/// A screen point as the board's window sees it (points from its own corner).
fn local(rect: [i32; 4], ppp: f32, x: i32, y: i32) -> egui::Pos2 {
    pos2((x - rect[0]) as f32 / ppp, (y - rect[1]) as f32 / ppp)
}

/// What the mouse did, as the hook sees it (screen pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseKind {
    Move,
    /// 0 the left button, 1 the right, 2 the middle.
    Down(u8),
    Up(u8),
    /// Notches of the wheel (away from Sir is positive).
    #[cfg_attr(not(windows), allow(dead_code))]
    Wheel(f32),
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub x: i32,
    pub y: i32,
}

/// What the mouse did, from the hook's thread. True when the event is his alone (a press on
/// one of his keys, and its release): it then goes no further. Never waits for anything.
#[cfg(windows)]
pub fn on_mouse(m: Mouse) -> bool {
    if !ON.load(Ordering::Relaxed) || GAME.load(Ordering::Relaxed) {
        return false;
    }
    let over = crate::win32_desktop::desktop_at(m.x, m.y);
    let Ok(mut guard) = STATE.try_lock() else { return false };
    let Some(s) = guard.as_mut() else { return false };
    let (ours, woke) = took(s, m.kind, local(s.rect, s.ppp, m.x, m.y), over, HOT.load(Ordering::Relaxed));
    drop(guard);
    if woke {
        wake();
    }
    ours
}

/// What one event of the mouse means for the board: (it is his alone, his board is to draw).
fn took(s: &mut State, kind: MouseKind, at: egui::Pos2, over: bool, hot: bool) -> (bool, bool) {
    use egui::{Event, Modifiers, PointerButton};
    let button = |b: u8| match b {
        0 => PointerButton::Primary,
        1 => PointerButton::Secondary,
        _ => PointerButton::Middle,
    };
    match kind {
        // Over the bare desktop, or while one of his keys is held (a drag may leave it).
        MouseKind::Move if over || s.held != 0 => {
            // (Many moves between two frames are one move to the frame.)
            if matches!(s.events.last(), Some(Event::PointerMoved(_))) {
                s.events.pop();
            }
            s.events.push(Event::PointerMoved(at));
            s.over = true;
            (false, true)
        }
        MouseKind::Move if s.over => {
            s.events.push(Event::PointerGone);
            s.over = false;
            (false, true)
        }
        MouseKind::Move => (false, false),
        MouseKind::Down(b) if over && hot => {
            s.held |= 1 << b;
            s.events.push(Event::PointerMoved(at));
            s.events.push(Event::PointerButton { pos: at, button: button(b), pressed: true, modifiers: Modifiers::NONE });
            (true, true)
        }
        MouseKind::Up(b) if s.held & (1 << b) != 0 => {
            s.held &= !(1 << b);
            s.events.push(Event::PointerMoved(at));
            s.events.push(Event::PointerButton { pos: at, button: button(b), pressed: false, modifiers: Modifiers::NONE });
            (true, true)
        }
        MouseKind::Wheel(notches) if over && hot => {
            s.events.push(Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, notches), modifiers: Modifiers::NONE });
            (true, true)
        }
        _ => (false, false),
    }
}

/// A scripted session's stand-in for Sir's mouse over the desktop (`JAI_DEMO=desk-click`): the
/// way the hook's events take from `took` on, without the hook and as if the bare desktop
/// were under the point. `step` 0 moves the pointer to the screen point, 1 presses the left
/// button, 2 lets it go. For a press: whether it was his (one of his keys was under it).
pub fn demo_mouse(step: u8, x: i32, y: i32) -> Option<bool> {
    let mut guard = lock(&STATE);
    let s = guard.as_mut()?;
    let kind = match step {
        0 => MouseKind::Move,
        1 => MouseKind::Down(0),
        _ => MouseKind::Up(0),
    };
    let (ours, _) = took(s, kind, local(s.rect, s.ppp, x, y), true, HOT.load(Ordering::Acquire));
    (step == 1).then_some(ours)
}

/// Asks the board for a frame, from the hook's thread, without ever waiting: a thread of its
/// own does the asking, at most every few milliseconds.
#[cfg(windows)]
fn wake() {
    static WAKER: std::sync::OnceLock<Option<std::sync::mpsc::Sender<()>>> = std::sync::OnceLock::new();
    let waker = WAKER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("jai-desk-wake".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    while rx.try_recv().is_ok() {}
                    crate::gui::refresh();
                    std::thread::sleep(Duration::from_millis(7));
                }
            })
            .ok()
            .map(|_| tx)
    });
    if let Some(tx) = waker {
        let _ = tx.send(());
    }
}

/// The pointer over the desktop becomes the board's own pointer input (`eframe`'s raw input
/// hook, as the window drill's clicks do).
pub fn feed(raw: &mut egui::RawInput) {
    if raw.viewport_id != egui::ViewportId::ROOT || !ON.load(Ordering::Acquire) {
        return;
    }
    if let Some(s) = lock(&STATE).as_mut() {
        raw.events.append(&mut s.events);
    }
}

/// After each frame of the board as the background: is one of his keys (anything that takes a
/// click or a drag) under the pointer? Only then is a press his and not the desktop's.
pub fn after_frame(ctx: &egui::Context) {
    if !ON.load(Ordering::Acquire) {
        return;
    }
    let (held, hovered): (bool, Vec<egui::Id>) = ctx.interaction_snapshot(|s| (s.dragged.is_some(), s.hovered.iter().copied().collect()));
    let hot = held || hovered.into_iter().any(|id| ctx.read_response(id).is_some_and(|r| r.sense.click || r.sense.drag));
    HOT.store(hot, Ordering::Release);
}

// ---------------------------------------------------------------- is he seen? the orb ----

/// Looks (on the Win32 worker) how much of the background's emblem, at `emblem` on the screen
/// (left, top, right, bottom), other programs' windows cover. A few times a second is plenty.
#[cfg(windows)]
pub fn look_at(emblem: [i32; 4]) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = lock(&LAST);
    if last.is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
        return;
    }
    *last = Some(Instant::now());
    crate::win32::run_detached(move || HIDDEN.store(hidden_now(HIDDEN.load(Ordering::Acquire), crate::win32_desktop::covered_share(emblem)), Ordering::Release));
}

// shortcut: KWin's and XWayland's coordinates are taken as one (true without display scaling)
// and windows on other virtual desktops count too; upgrade with the outputs' scale and Plasma's
// virtual-desktop list.
#[cfg(not(windows))]
pub fn look_at(emblem: [i32; 4]) {
    use crate::plasma::state;
    let [l, t, r, b] = emblem;
    let me = std::process::id();
    // (As on Windows: every other program's window, and of his own those that carry a name, never
    // the orb; his background itself is the one kept below.)
    let most = crate::plasma::windows()
        .into_iter()
        .filter(|w| !w.is(state::MINIMIZED) && !w.is(state::KEEP_BELOW) && (w.pid != me || !(w.title.is_empty() || w.title == ORB_TITLE)))
        .map(|w| {
            let [x, y, width, height] = w.geometry;
            (r.min(x + width) - l.max(x)).max(0) as i64 * (b.min(y + height) - t.max(y)).max(0) as i64
        })
        .max()
        .unwrap_or(0);
    let share = most as f32 / ((r - l).max(1) as i64 * (b - t).max(1) as i64) as f32;
    HIDDEN.store(hidden_now(HIDDEN.load(Ordering::Acquire), share), Ordering::Release);
}

/// The corner orb's window, by its title.
pub const ORB_TITLE: &str = "J.A.I. // orb";

/// Whether the background's emblem is hidden behind a window.
pub fn hidden() -> bool {
    HIDDEN.load(Ordering::Acquire)
}

/// Whether the orb is called out of the background: its emblem is `covered` and he listens.
/// Not listening he does not show himself either (Sir, 2026-11-16): every noise is still
/// transcribed then, and that read as him thinking.
pub fn orb_called(covered: bool) -> bool {
    covered && !crate::listening_lock::is_locked()
}

/// Whether the orb is out of the background right now (in its corner, or on its way): then
/// "make the orb smaller" means that one.
static ORB_OUT: AtomicBool = AtomicBool::new(false);

pub fn orb_out() -> bool {
    ORB_OUT.load(Ordering::Acquire)
}

pub fn set_orb_out(out: bool) {
    ORB_OUT.store(out, Ordering::Release);
}

/// The monitor Sir is using: the one the window in front is on (another program's), else the
/// one under the pointer, else his first.
pub fn monitor_in_use(monitors: &[crate::monitors::Monitor]) -> Option<crate::monitors::Monitor> {
    // (A scripted session knows one monitor only, its own: nothing of it ever goes where Sir
    // works. Without one of its own it has none.)
    if let Some(own) = crate::gui::test_session_monitor() {
        return monitors.iter().find(|m| m.index == own).cloned();
    }
    #[cfg(windows)]
    let at = crate::win32_desktop::foreground_rect().map(|[l, t, r, b]| ((l + r) / 2, (t + b) / 2)).or_else(crate::win32_desktop::cursor);
    // (KDE: Wayland tells no program where the pointer is.)
    #[cfg(not(windows))]
    let at = crate::plasma::active().filter(|w| w.pid != std::process::id()).map(|w| (w.geometry[0] + w.geometry[2] / 2, w.geometry[1] + w.geometry[3] / 2));
    let in_use = at.and_then(|(x, y)| monitors.iter().find(|m| m.contains(x, y))).or_else(|| crate::monitors::primary(monitors));
    // A game fills that monitor: his orb and windows wait on another one until it closes.
    let other = in_use.filter(|_| crate::hud_feel::game_in_front()).and_then(|game| monitors.iter().find(|m| m.index != game.index));
    other.or(in_use).cloned()
}

/// Whether Sir holds the left mouse button down right now.
pub fn button_down() -> bool {
    #[cfg(windows)]
    return crate::win32_desktop::left_button_down();
    #[cfg(not(windows))]
    crate::x11::pointer().is_some_and(|(_, _, held)| held)
}

/// How long the orb takes from the background to its corner (and back).
pub const FLIGHT: Duration = Duration::from_millis(520);
/// How long it stays in the corner once nothing is going on. (Sir: "after 5 seconds".)
pub const LINGER: Duration = Duration::from_secs(5);
/// How far it keeps from the edges of the screen.
const CORNER_MARGIN: i32 = 26;

/// Where the orb rests on a monitor (its work area `work`: left, top, right, bottom): the spot
/// Sir dragged it to there (`spot`: its corner as a share of the work area), else the lower
/// right corner. Always wholly on that monitor.
pub fn corner(work: [i32; 4], size: i32, spot: Option<[f32; 2]>) -> Rect {
    let (w, h) = ((work[2] - work[0]).max(size), (work[3] - work[1]).max(size));
    let (x, y) = match spot {
        Some([fx, fy]) => (work[0] + (fx.clamp(0.0, 1.0) * (w - size) as f32).round() as i32, work[1] + (fy.clamp(0.0, 1.0) * (h - size) as f32).round() as i32),
        None => (work[2] - CORNER_MARGIN - size, work[3] - CORNER_MARGIN - size),
    };
    let (x, y) = (x.clamp(work[0], work[0] + w - size), y.clamp(work[1], work[1] + h - size));
    Rect::from_min_size(pos2(x as f32, y as f32), vec2(size as f32, size as f32))
}

/// The share of a monitor's work area at which an orb at `at` (its corner, screen pixels)
/// stands: what `corner` takes as `spot`.
pub fn spot_of(work: [i32; 4], size: i32, at: egui::Pos2) -> [f32; 2] {
    let (w, h) = ((work[2] - work[0] - size).max(1) as f32, (work[3] - work[1] - size).max(1) as f32);
    [((at.x - work[0] as f32) / w).clamp(0.0, 1.0), ((at.y - work[1] as f32) / h).clamp(0.0, 1.0)]
}

/// Slow, fast, slow.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn between(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * t, a.max + (b.max - a.max) * t)
}

/// Where the orb is in its life.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrbPhase {
    /// In the background (no window of its own).
    Home,
    /// On its way to the corner.
    Leaving,
    /// In the corner, in front of everything.
    Out,
    /// On its way back into the background.
    Returning,
}

/// The orb that leaves the background for a corner of the monitor Sir is using, and goes back.
#[derive(Debug, Clone)]
pub struct Orb {
    phase: OrbPhase,
    since: Instant,
    from: Rect,
    to: Rect,
    /// Since when nothing is going on (while it is out).
    quiet: Option<Instant>,
    /// Sir has hold of it, or has just let go (`grabbed`, `let_go`).
    in_hand: bool,
    /// Where Windows last said its window stands, while it is in his hand.
    seen_at: Option<egui::Pos2>,
}

/// One frame of the orb: where its window stands on the screen (pixels), and how far out of
/// the background it is (0 at home, 1 in the corner).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbFrame {
    pub rect: Rect,
    pub out: f32,
    pub phase: OrbPhase,
}

impl Default for Orb {
    fn default() -> Self {
        Self { phase: OrbPhase::Home, since: Instant::now(), from: Rect::NOTHING, to: Rect::NOTHING, quiet: None, in_hand: false, seen_at: None }
    }
}

impl Orb {
    pub fn is_home(&self) -> bool {
        self.phase == OrbPhase::Home
    }

    /// One step at `now`. `busy`: he listens, thinks, speaks or works; `hidden`: the emblem on
    /// the background cannot be seen; `home`: that emblem on the screen (pixels); `corner`:
    /// where the orb rests on the monitor Sir is using (asked only when it sets out). None: it
    /// is at home, part of the background.
    pub fn step(&mut self, now: Instant, busy: bool, hidden: bool, home: Rect, corner: impl FnOnce() -> Rect) -> Option<OrbFrame> {
        let flown = |since: Instant| now.saturating_duration_since(since).as_secs_f32() / FLIGHT.as_secs_f32();
        match self.phase {
            OrbPhase::Home => {
                if !(busy && hidden) {
                    return None;
                }
                (self.phase, self.since, self.from, self.to, self.quiet) = (OrbPhase::Leaving, now, home, corner(), None);
            }
            // (The emblem is seen again: back to it at once, from where it is, busy or not. Sir,
            // 2026-11-14: "the orb should only move to the corner when the big orb is blocked by
            // something. If not then it should show the original big orb." Not out of his hand.)
            OrbPhase::Leaving if !hidden => {
                let here = between(self.from, self.to, ease(flown(self.since)));
                (self.phase, self.since, self.from) = (OrbPhase::Returning, now, here);
            }
            OrbPhase::Out if !hidden && !self.in_hand => {
                (self.phase, self.since, self.from) = (OrbPhase::Returning, now, self.to);
            }
            OrbPhase::Leaving => {
                if flown(self.since) >= 1.0 {
                    (self.phase, self.since, self.quiet) = (OrbPhase::Out, now, None);
                }
            }
            OrbPhase::Out => {
                if busy {
                    self.quiet = None;
                } else if now.saturating_duration_since(*self.quiet.get_or_insert(now)) >= LINGER {
                    (self.phase, self.since, self.from) = (OrbPhase::Returning, now, self.to);
                }
            }
            OrbPhase::Returning => {
                if busy && hidden {
                    // Called again on the way back: it turns round where it is.
                    let here = between(self.from, home, ease(flown(self.since)));
                    (self.phase, self.since, self.from, self.quiet) = (OrbPhase::Leaving, now, here, None);
                } else if flown(self.since) >= 1.0 {
                    self.phase = OrbPhase::Home;
                    return None;
                }
            }
        }
        Some(match self.phase {
            OrbPhase::Leaving => {
                let t = ease(flown(self.since));
                // (Turned round mid-way it starts nearer the corner: as far out as it then was.)
                let whole = (self.to.center() - home.center()).length().max(1.0);
                let start = 1.0 - ((self.to.center() - self.from.center()).length() / whole).clamp(0.0, 1.0);
                OrbFrame { rect: between(self.from, self.to, t), out: start + (1.0 - start) * t, phase: self.phase }
            }
            OrbPhase::Returning => {
                let t = ease(flown(self.since));
                OrbFrame { rect: between(self.from, home, t), out: 1.0 - t, phase: self.phase }
            }
            _ => OrbFrame { rect: self.to, out: 1.0, phase: OrbPhase::Out },
        })
    }

    /// Its window is not on show yet: the flight out of the background waits for it, so it
    /// leaves from the emblem and does not appear half-way to its corner.
    pub fn wait(&mut self, now: Instant) {
        if self.phase == OrbPhase::Leaving {
            self.since = now;
        }
    }

    /// Sir went to another monitor while it is out: it glides to its place there (`to`), from
    /// where it rests. Not out of his hand. (His choice of 2026-11-14: "Orb follows you".)
    pub fn follow(&mut self, now: Instant, to: Rect) {
        // (A pixel of rounding is no other place.)
        let elsewhere = (to.min - self.to.min).length() > 2.0 || (to.width() - self.to.width()).abs() > 0.5;
        if self.phase == OrbPhase::Out && !self.in_hand && elsewhere {
            (self.phase, self.since, self.from, self.to) = (OrbPhase::Leaving, now, self.to, to);
        }
    }

    /// Sir takes hold of the orb: where he lets go of it counts. False while it flies (then
    /// it is not his to drag).
    pub fn grabbed(&mut self) -> bool {
        if self.phase == OrbPhase::Out {
            (self.in_hand, self.seen_at, self.quiet) = (true, None, None);
        }
        self.phase == OrbPhase::Out
    }

    /// Once a frame while it is out: `reported` is where Windows says its window stands
    /// (pixels), `button_down` whether Sir holds the mouse button. Some: he moved it by hand
    /// and let go there, where it rests from now on. Only after he took hold of it, and only
    /// a place the window has come to rest at: Windows tells of a move a frame or two late,
    /// and what it tells of the orb's own flight is not Sir's hand.
    pub fn let_go(&mut self, reported: egui::Pos2, button_down: bool) -> Option<Rect> {
        if self.phase != OrbPhase::Out || !self.in_hand {
            return None;
        }
        // (In his hand it does not go back into the background.)
        self.quiet = None;
        if button_down || self.seen_at.replace(reported) != Some(reported) {
            return None;
        }
        (self.in_hand, self.seen_at) = (false, None);
        if (reported - self.to.min).length() <= 3.0 {
            return None;
        }
        self.to = Rect::from_min_size(reported, self.to.size());
        Some(self.to)
    }

    /// Where it rests while it is out.
    pub fn resting(&self) -> Option<Rect> {
        (self.phase == OrbPhase::Out).then_some(self.to)
    }
}

/// The spots Sir dragged the orb to, by monitor (its rect as a key), kept in the kit's store.
pub fn spot_on(monitor: [i32; 4]) -> Option<[f32; 2]> {
    let spots = crate::kit::get("orb", "spots")?;
    let at = spots.get(key(monitor))?;
    Some([at.get(0)?.as_f64()? as f32, at.get(1)?.as_f64()? as f32])
}

pub fn remember_spot(monitor: [i32; 4], spot: [f32; 2]) {
    let mut spots = crate::kit::get("orb", "spots").filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({}));
    spots[key(monitor)] = serde_json::json!([spot[0], spot[1]]);
    let _ = crate::kit::put("orb", "spots", &spots);
}

fn key([l, t, r, b]: [i32; 4]) -> String {
    format!("{l},{t},{r},{b}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(index: usize, rect: [i32; 4], primary: bool) -> crate::monitors::Monitor {
        crate::monitors::Monitor { index, name: format!("DISPLAY{index}"), rect, work: [rect[0], rect[1], rect[2], rect[3] - 48], primary }
    }

    #[test]
    fn the_background_covers_every_monitor_or_the_first() {
        let three = [monitor(1, [0, 0, 2560, 1440], true), monitor(2, [2560, 0, 5120, 1440], false), monitor(3, [-1080, -290, 0, 1630], false)];
        assert_eq!(rect_for(true, &three), Some([-1080, -290, 5120, 1630]), "the box around all of them");
        assert_eq!(rect_for(false, &three), Some([0, 0, 2560, 1440]), "his first one");
        assert_eq!(rect_for(true, &[]), None);
    }

    fn state() -> State {
        State { standing: Standing::On, since: Instant::now(), asked: true, hwnd: 1, rect: [-1080, -290, 5120, 1630], looked: Instant::now(), ppp: 1.0, events: Vec::new(), over: false, held: 0 }
    }

    /// A press is his only on one of his keys; every other click is the desktop's, and the
    /// pointer is passed on wherever the bare desktop shows.
    #[test]
    fn a_press_is_his_only_on_one_of_his_keys() {
        let mut s = state();
        let at = local(s.rect, s.ppp, 100, 50);
        assert_eq!(at, pos2(1180.0, 340.0), "from the corner of his window, which begins on the left monitor");
        // Moving over the desktop: he sees the pointer, the event goes on.
        assert_eq!(took(&mut s, MouseKind::Move, at, true, false), (false, true));
        assert!(matches!(s.events.as_slice(), [egui::Event::PointerMoved(p)] if *p == at));
        // Two moves before his next frame are one.
        took(&mut s, MouseKind::Move, at + vec2(4.0, 0.0), true, false);
        assert_eq!(s.events.len(), 1);
        // A click on the bare desktop, nothing of his under it: the desktop's.
        assert_eq!(took(&mut s, MouseKind::Down(0), at, true, false), (false, false));
        assert_eq!(took(&mut s, MouseKind::Up(0), at, true, false), (false, false));
        // On one of his keys: his alone, press and release.
        assert_eq!(took(&mut s, MouseKind::Down(0), at, true, true), (true, true));
        assert_eq!(s.held, 1);
        // (Held, the pointer may leave the desktop: he still follows it.)
        assert_eq!(took(&mut s, MouseKind::Move, at, false, false), (false, true));
        assert_eq!(took(&mut s, MouseKind::Up(0), at, false, false), (true, true));
        assert_eq!(s.held, 0);
        assert!(s.events.iter().filter(|e| matches!(e, egui::Event::PointerButton { .. })).count() == 2);
        // Off the desktop (over a window): the pointer is gone for him, once.
        s.events.clear();
        assert_eq!(took(&mut s, MouseKind::Move, at, false, false), (false, true));
        assert!(matches!(s.events.as_slice(), [egui::Event::PointerGone]));
        assert_eq!(took(&mut s, MouseKind::Move, at, false, false), (false, false));
        // A press over another program's window is never his, whatever lies under it.
        assert_eq!(took(&mut s, MouseKind::Down(0), at, false, true), (false, false));
        // The wheel over one of his lists.
        assert_eq!(took(&mut s, MouseKind::Wheel(-1.0), at, true, true), (true, true));
        assert_eq!(took(&mut s, MouseKind::Wheel(-1.0), at, true, false), (false, false));
    }

    #[test]
    fn the_orb_rests_in_the_lower_right_corner_or_where_sir_put_it() {
        // (Sir, 2026-11-14: "in the right down corner … on standard". Until then: the left.)
        let work = [0, 0, 2560, 1392];
        let at = corner(work, 132, None);
        assert_eq!((at.right(), at.bottom()), (2560.0 - 26.0, 1392.0 - 26.0), "the lower right corner, off the taskbar");
        assert_eq!(at.size(), vec2(132.0, 132.0));
        // The upright monitor to the left: its own corner.
        let tall = corner([-1080, -290, 0, 1582], 132, None);
        assert_eq!((tall.right(), tall.bottom()), (-26.0, 1556.0));
        // Where Sir dragged it, as a share of the screen: found again, also after the screen changed size.
        let dragged = pos2(1900.0, 300.0);
        let spot = spot_of(work, 132, dragged);
        assert!((corner(work, 132, Some(spot)).min - dragged).length() < 1.5);
        let smaller = corner([0, 0, 1920, 1040], 132, Some(spot));
        assert!(smaller.right() <= 1920.0 && smaller.bottom() <= 1040.0);
        // Never off the monitor.
        assert_eq!(corner(work, 132, Some([2.0, -1.0])).min, pos2(2560.0 - 132.0, 0.0));
    }

    /// Spoken to while the background is covered, the orb flies to the corner, stays while he
    /// is busy and five seconds more, and flies back. Nothing jumps.
    #[test]
    fn the_orb_comes_out_when_he_is_spoken_to_unseen_and_goes_back_five_seconds_after() {
        let home = Rect::from_center_size(pos2(1280.0, 720.0), vec2(320.0, 320.0));
        let rest = corner([0, 0, 2560, 1392], 132, None);
        let mut orb = Orb::default();
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Seen on the desktop, or idle: it stays in the background.
        assert_eq!(orb.step(ms(0), true, false, home, || rest), None);
        assert_eq!(orb.step(ms(0), false, true, home, || rest), None);
        // Spoken to while a window covers it: it sets out from where the emblem is.
        let first = orb.step(ms(0), true, true, home, || rest).unwrap();
        assert_eq!((first.rect, first.out, first.phase), (home, 0.0, OrbPhase::Leaving));
        // Half-way it is between the two, smaller than at home, larger than in the corner.
        let mid = orb.step(ms(260), true, true, home, || rest).unwrap();
        assert!(mid.rect.width() < home.width() && mid.rect.width() > rest.width());
        assert!((mid.out - 0.5).abs() < 0.05);
        // No step of the flight is a jump.
        let mut last = first.rect;
        for n in (0..=540).step_by(16) {
            let f = orb.clone().step(ms(n), true, true, home, || rest).unwrap();
            assert!((f.rect.center() - last.center()).length() < 140.0, "a jump of {} px at {n} ms", (f.rect.center() - last.center()).length());
            last = f.rect;
        }
        // Arrived: in the corner, and it stays while he is busy.
        orb.step(ms(540), true, true, home, || rest);
        let out = orb.step(ms(560), true, true, home, || rest).unwrap();
        assert_eq!((out.rect, out.out, out.phase), (rest, 1.0, OrbPhase::Out));
        assert_eq!(orb.step(ms(60_000), true, true, home, || rest).unwrap().phase, OrbPhase::Out, "busy for a minute: still out");
        // Quiet: five seconds more, then back.
        assert_eq!(orb.step(ms(60_100), false, true, home, || rest).unwrap().phase, OrbPhase::Out);
        assert_eq!(orb.step(ms(64_900), false, true, home, || rest).unwrap().phase, OrbPhase::Out);
        // (Spoken to again within the five seconds: they start anew.)
        orb.step(ms(65_000), true, true, home, || rest);
        assert_eq!(orb.step(ms(69_500), false, true, home, || rest).unwrap().phase, OrbPhase::Out);
        assert_eq!(orb.step(ms(74_600), false, true, home, || rest).unwrap().phase, OrbPhase::Returning);
        let back = orb.step(ms(74_860), false, true, home, || rest).unwrap();
        assert!(back.rect.width() > rest.width() && back.out < 1.0 && back.out > 0.0);
        // Called on the way back: it turns round where it is, it does not start from home.
        let would = orb.clone().step(ms(74_900), false, true, home, || rest).unwrap();
        let mut turned = orb.clone();
        let again = turned.step(ms(74_900), true, true, home, || rest).unwrap();
        assert_eq!(again.phase, OrbPhase::Leaving);
        assert!((again.rect.center() - would.rect.center()).length() < 1.0, "it turns round where it is");
        assert!(again.out > 0.2 && again.out < 0.5, "{}", again.out);
        // Left alone it is home again, and has no window.
        assert_eq!(orb.step(ms(75_200), false, true, home, || rest), None);
        assert!(orb.is_home());
        // While its window is not on show yet the flight waits: it leaves from the emblem.
        let mut waiting = Orb::default();
        waiting.step(ms(0), true, true, home, || rest);
        waiting.wait(ms(300));
        let still = waiting.step(ms(310), true, true, home, || rest).unwrap();
        assert!((still.rect.center() - home.center()).length() < 1.0 && still.out < 0.01, "{still:?}");
        assert_eq!(waiting.step(ms(900), true, true, home, || rest).unwrap().phase, OrbPhase::Out, "and then it flies as ever");
    }

    /// Sir, 2026-11-14: "the orb should only move to the corner when the big orb is blocked by
    /// something. If not then it should show the original big orb" (wherever on his desktop he
    /// has put it: `home` is where it stands).
    #[test]
    fn the_orb_is_out_only_while_the_emblem_is_hidden() {
        // (The emblem where Sir dragged it: low on the right of his first monitor.)
        let home = Rect::from_center_size(pos2(1920.0, 1094.0), vec2(400.0, 400.0));
        let rest = corner([0, 0, 2560, 1392], 168, None);
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Spoken to while the emblem is seen: nothing leaves it, however long he is busy.
        let mut orb = Orb::default();
        assert!((0..100).all(|n| orb.step(ms(n * 100), true, false, home, || rest).is_none()));
        // A window covers it and he is spoken to: out, from where the emblem stands.
        assert_eq!(orb.step(ms(10_000), true, true, home, || rest).unwrap().rect, home);
        assert_eq!(orb.step(ms(10_600), true, true, home, || rest).unwrap().phase, OrbPhase::Out);
        // The window goes: back at once, though he is still speaking, and home again.
        let back = orb.step(ms(10_700), true, false, home, || rest).unwrap();
        assert_eq!((back.phase, back.rect), (OrbPhase::Returning, rest));
        assert!(orb.step(ms(11_300), true, false, home, || rest).is_none() && orb.is_home());
        // Uncovered while it is still on its way out: it turns back where it is.
        orb.step(ms(20_000), true, true, home, || rest);
        let mid = orb.step(ms(20_260), true, true, home, || rest).unwrap();
        let turned = orb.step(ms(20_262), true, false, home, || rest).unwrap();
        assert_eq!(turned.phase, OrbPhase::Returning);
        assert!((turned.rect.center() - mid.rect.center()).length() < 60.0, "no jump: {:?} from {:?}", turned.rect, mid.rect);
        assert!(orb.step(ms(20_900), true, false, home, || rest).is_none());
        // In his hand it is not pulled away, seen emblem or not.
        orb.step(ms(30_000), true, true, home, || rest);
        orb.step(ms(30_600), true, true, home, || rest);
        assert!(orb.grabbed());
        assert_eq!(orb.step(ms(30_700), false, false, home, || rest).unwrap().phase, OrbPhase::Out);
    }

    /// Sir, 2026-11-16: "when jai is in not listening mode his orb shouldnt show either". Under
    /// the lock every noise is still transcribed (and thrown away), which the HUD reads as busy.
    #[test]
    fn not_listening_the_orb_stays_in_the_background() {
        use crate::listening_lock::{apply, LockCommand};
        let _g = crate::listening_lock::test_lock();
        let home = Rect::from_center_size(pos2(1280.0, 720.0), vec2(400.0, 400.0));
        let rest = corner([0, 0, 2560, 1392], 168, None);
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut orb = Orb::default();
        // Out for "stop listening" itself, the emblem covered by his video...
        apply(LockCommand::Resume);
        orb.step(ms(0), true, orb_called(true), home, || rest);
        assert_eq!(orb.step(ms(600), true, orb_called(true), home, || rest).unwrap().phase, OrbPhase::Out);
        // ...the lock goes on while he still answers: back at once, and home.
        apply(LockCommand::Lock);
        assert_eq!(orb.step(ms(700), true, orb_called(true), home, || rest).unwrap().phase, OrbPhase::Returning);
        assert!(orb.step(ms(1_300), true, orb_called(true), home, || rest).is_none() && orb.is_home());
        // A minute of noises transcribed and thrown away: never out, nor under a timed pause.
        assert!((0..600).all(|n| orb.step(ms(2_000 + n * 100), true, orb_called(true), home, || rest).is_none()));
        apply(LockCommand::Pause(Duration::from_secs(600)));
        assert!((0..100).all(|n| orb.step(ms(62_000 + n * 100), true, orb_called(true), home, || rest).is_none()));
        // Listening again: out for the next word, as ever.
        apply(LockCommand::Resume);
        assert!(orb.step(ms(80_000), true, orb_called(true), home, || rest).is_some());
    }

    /// While it is out and Sir goes to another monitor, it glides to its corner there, without
    /// a jump, and rests there; never out of his hand, and not while it flies.
    #[test]
    fn the_orb_follows_sir_to_another_monitor() {
        let home = Rect::from_center_size(pos2(1280.0, 720.0), vec2(400.0, 400.0));
        let (here, there) = (corner([0, 0, 2560, 1392], 168, None), corner([2560, 0, 5120, 1392], 168, None));
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut orb = Orb::default();
        orb.step(ms(0), true, true, home, || here);
        orb.follow(ms(100), there);
        assert_eq!(orb.step(ms(600), true, true, home, || here).unwrap().rect, here, "on its way out it keeps its course");
        orb.follow(ms(700), there);
        let first = orb.step(ms(700), true, true, home, || here).unwrap();
        assert_eq!((first.phase, first.rect), (OrbPhase::Leaving, here), "it sets out from where it rested");
        let mut last = first.rect;
        for n in (700..=1_260).step_by(16) {
            let f = orb.clone().step(ms(n), true, true, home, || here).unwrap();
            assert!((f.rect.center() - last.center()).length() < 260.0, "a jump at {n} ms");
            last = f.rect;
        }
        let arrived = orb.step(ms(1_300), true, true, home, || here).unwrap();
        assert_eq!((arrived.phase, arrived.rect), (OrbPhase::Out, there));
        // In his hand it stays where he holds it.
        assert!(orb.grabbed());
        orb.follow(ms(1_400), here);
        assert_eq!(orb.resting(), Some(there));
    }

    /// A window half over the emblem does not send the orb to and fro: hidden from more than
    /// three tenths of it, seen again under an eighth.
    #[test]
    fn the_emblem_is_hidden_or_seen_with_room_between() {
        assert!(!hidden_now(false, 0.0) && !hidden_now(false, 0.29) && hidden_now(false, 0.31) && hidden_now(false, 1.0));
        assert!(hidden_now(true, 0.2) && hidden_now(true, 0.12), "still hidden while a good part of it is covered");
        assert!(!hidden_now(true, 0.1) && !hidden_now(true, 0.0), "seen again");
        assert!(!hidden_now(false, 0.2), "and not hidden by as much, coming from seen");
    }

    /// 2026-11-14, Sir's own session: the flight's last frames came 59 ms apart, Windows told
    /// of the window's place a frame late, and that late word of his own flight was taken for
    /// Sir's hand: the orb hopped between two spots 5 px apart for good (and wrote each down as
    /// "where Sir put it"), and never went back into the background.
    #[test]
    fn a_late_report_of_its_own_flight_is_not_sirs_hand() {
        let home = Rect::from_center_size(pos2(1184.0, 781.0), vec2(400.0, 400.0));
        let rest = corner([2560, 0, 5120, 1392], 168, None);
        let mut orb = Orb::default();
        let t0 = Instant::now();
        // Each frame the window is put where the orb is, and Windows reports where it was put
        // the frame before (whole pixels).
        let (mut reported, mut places) = (home.min, Vec::new());
        for n in (0..7_500).step_by(59) {
            let Some(frame) = orb.step(t0 + Duration::from_millis(n), n < 1_000, true, home, || rest) else { break };
            if frame.phase == OrbPhase::Out {
                assert_eq!(orb.let_go(reported, false), None, "nobody touched it ({n} ms, reported at {reported:?})");
                places.push(orb.resting().unwrap().min);
            }
            reported = orb.resting().map_or(frame.rect.min, |r| r.min).round();
        }
        assert!(places.len() > 50 && places.iter().all(|p| *p == rest.min), "it rests in its corner and stays there");
        assert!(orb.is_home(), "five seconds after he fell silent it is back in the background");
    }

    #[test]
    fn where_sir_drags_the_orb_it_stays() {
        let home = Rect::from_center_size(pos2(1280.0, 720.0), vec2(320.0, 320.0));
        let rest = corner([0, 0, 2560, 1392], 132, None);
        let mut orb = Orb::default();
        let t0 = Instant::now();
        orb.step(t0, true, true, home, || rest);
        orb.step(t0 + Duration::from_millis(600), true, true, home, || rest);
        let put = Rect::from_min_size(pos2(2000.0, 200.0), vec2(132.0, 132.0));
        // Not in his hand: wherever its window is said to stand, it rests where it was.
        assert_eq!(orb.let_go(put.min, false), None);
        assert_eq!(orb.let_go(put.min, false), None);
        assert_eq!(orb.resting(), Some(rest));
        // He takes hold of it and drags: nothing counts while the button is down, nor while
        // its window is still on its way.
        orb.grabbed();
        assert_eq!(orb.let_go(pos2(900.0, 700.0), true), None);
        assert_eq!(orb.let_go(pos2(900.0, 700.0), true), None, "held still for a moment, the button down");
        assert_eq!(orb.let_go(pos2(1990.0, 204.0), false), None, "let go: the window's place is told late");
        assert_eq!(orb.let_go(put.min, false), None);
        // At rest where he let go: there it stays, told once.
        assert_eq!(orb.let_go(put.min, false), Some(put));
        assert_eq!(orb.let_go(put.min, false), None);
        assert_eq!(orb.resting(), Some(put));
        assert_eq!(orb.step(t0 + Duration::from_millis(700), true, true, home, || rest).unwrap().rect, put);
        // A click that moved nothing leaves it where it is.
        orb.grabbed();
        assert_eq!((orb.let_go(put.min, false), orb.let_go(put.min, false)), (None, None));
        assert_eq!(orb.resting(), Some(put));
        // While it is in his hand it does not go back into the background under it.
        orb.grabbed();
        for n in 0..200u64 {
            orb.let_go(pos2(n as f32, 0.0), true);
            assert_eq!(orb.step(t0 + Duration::from_secs(1 + n), false, true, home, || rest).unwrap().phase, OrbPhase::Out);
        }
    }
}
