//! His own windows on the desktop, as windows. Lined up on the monitor Sir is using when he
//! asks ("tidy the windows"; nothing lines up by itself), and moved by gliding there, never by
//! a jump. (Sir, 2026-11-13: "he shouldn't teleport windows and his orb and so around, he should
//! move them with smooth animations".)

use crate::window_commands::{Position, Size};
use std::sync::Mutex;
// His windows through Win32, or through X11 (XWayland under KDE Plasma) on Linux.
#[cfg(windows)]
use crate::win32_desktop as desk;
#[cfg(not(windows))]
use crate::x11 as desk;
use std::time::{Duration, Instant};

/// How long a window takes to glide where it was sent.
pub const GLIDE: Duration = Duration::from_millis(280);
/// The gap between two windows that are lined up, and round them.
const GAP: i32 = 14;

/// A glide: the window, from where (x, y, w, h), to where, since when.
type Glide = (isize, [i32; 4], [i32; 4], Instant);
static GLIDES: Mutex<Vec<Glide>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where a glide from `from` to `to` (x, y, w, h) stands after the share `t` of its time: fast
/// at first, slow at the end.
pub fn between(from: [i32; 4], to: [i32; 4], t: f32) -> [i32; 4] {
    let t = 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
    std::array::from_fn(|i| (from[i] as f32 + (to[i] - from[i]) as f32 * t).round() as i32)
}

/// Window `hwnd` glides from `from` to `to` (x, y, w, h). A window already on its way goes on
/// from where it is.
pub fn glide(hwnd: isize, from: [i32; 4], to: [i32; 4]) {
    let mut glides = lock(&GLIDES);
    let now = Instant::now();
    let from = match glides.iter().position(|g| g.0 == hwnd) {
        Some(k) => {
            let (_, a, b, since) = glides.remove(k);
            between(a, b, since.elapsed().as_secs_f32() / GLIDE.as_secs_f32())
        }
        None => from,
    };
    if from != to {
        glides.push((hwnd, from, to, now));
        crate::gui::refresh();
    }
}

/// One step of every glide, from the board's frame (its thread owns his windows). Whether any
/// window is still on its way (the board then draws on).
pub fn step() -> bool {
    let mut glides = lock(&GLIDES);
    glides.retain(|(hwnd, from, to, since)| {
        // (With the interface's animations off it is simply there.)
        let t = if crate::gui::motion_enabled() { since.elapsed().as_secs_f32() / GLIDE.as_secs_f32() } else { 1.0 };
        desk::place(*hwnd, between(*from, *to, t));
        t < 1.0
    });
    !glides.is_empty()
}

/// The cells (x, y, w, h) of a grid for `count` windows in the work area `work` (left, top,
/// right, bottom): as many columns as rows, or one more; a gap between them and round them.
pub fn cells(work: [i32; 4], count: usize) -> Vec<[i32; 4]> {
    if count == 0 {
        return Vec::new();
    }
    let (w, h) = (work[2] - work[0], work[3] - work[1]);
    // (An upright monitor stacks them: more rows than columns.)
    let cols = if h > w { ((count as f32 * w as f32 / h as f32).sqrt().floor() as usize).max(1) } else { (count as f32).sqrt().ceil() as usize };
    let rows = count.div_ceil(cols);
    let (cw, ch) = ((w - GAP * (cols as i32 + 1)) / cols as i32, (h - GAP * (rows as i32 + 1)) / rows as i32);
    (0..count).map(|k| [work[0] + GAP + (k % cols) as i32 * (cw + GAP), work[1] + GAP + (k / cols) as i32 * (ch + GAP), cw, ch]).collect()
}

/// Where each of `windows` (its handle and rect: left, top, right, bottom) goes when they are
/// lined up in `work`: in reading order by where they stand now, each in its cell at its own
/// size, made smaller only where it is larger than the cell. (handle, from, to), each x, y, w, h.
pub fn lined_up(work: [i32; 4], windows: &[(isize, [i32; 4])]) -> Vec<(isize, [i32; 4], [i32; 4])> {
    let mut order: Vec<&(isize, [i32; 4])> = windows.iter().collect();
    order.sort_by_key(|(_, r)| (r[1].div_euclid(240), r[0]));
    order
        .into_iter()
        .zip(cells(work, windows.len()))
        .map(|((hwnd, [l, t, r, b]), [x, y, cw, ch])| {
            let from = [*l, *t, r - l, b - t];
            (*hwnd, from, [x, y, from[2].min(cw), from[3].min(ch)])
        })
        .collect()
}

/// His windows as windows: all but the board itself and the corner orb.
pub fn list() -> Vec<desk::OwnWindow> {
    let board = crate::gui::hud_hwnd();
    desk::own_windows().into_iter().filter(|w| w.hwnd != board && w.title != crate::wallpaper::ORB_TITLE).collect()
}

/// "Tidy the windows": his windows glide into one grid on the monitor Sir is using. How many.
pub fn tidy_now() -> usize {
    let own = list();
    let monitors = crate::monitors::recent();
    let Some(on) = crate::wallpaper::monitor_in_use(&monitors) else { return 0 };
    let moves = lined_up(on.work, &own.iter().map(|w| (w.hwnd, w.rect)).collect::<Vec<_>>());
    let n = moves.len();
    for (hwnd, from, to) in moves {
        glide(hwnd, from, to);
    }
    if n > 0 {
        tracing::info!("[HUD] Tidied: {n} of his windows lined up on the monitor at {},{}", on.rect[0], on.rect[1]);
    }
    n
}

// ----------------------------------------------------------------- one of them, by name ----

/// A window's title, or what Sir called it, down to its words: lower case, without his own name
/// in it, its marks and the words that say nothing ("the", "window").
fn words_of(text: &str) -> String {
    let text = text.to_lowercase().replace("j.a.i.", " ");
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !matches!(*w, "the" | "my" | "your" | "window" | "das" | "die" | "der" | "dein" | "deine" | "fenster")).collect::<Vec<_>>().join(" ")
}

/// Whether `name`, as Sir said it, means the window titled `title`: words of it in their order
/// ("the text console" for "Text Console — J.A.I."), or the one word that ends a part of it
/// ("console", "timetable" for "WEBUNTIS // TIMETABLE — J.A.I."; never "text": "make the text
/// bigger" is the HUD's).
pub fn names(title: &str, name: &str) -> bool {
    let name = words_of(name);
    if !name.contains(' ') {
        // ("The protocol window" is "Protocols — J.A.I.": one or more of it. Not "the new
        // window" for "News", a short word.)
        let one = |w: &str| if w.len() > 5 { w.strip_suffix('s').unwrap_or(w) } else { w }.to_string();
        return !name.is_empty() && title.split(['\u{2014}', '/', '|', ':']).any(|part| words_of(part).rsplit(' ').next().is_some_and(|last| one(last) == one(&name)));
    }
    format!(" {} ", words_of(title)).contains(&format!(" {name} "))
}

/// The one of his windows Sir means by `name` ("it": the one in front, when that is his).
pub fn find(name: &str) -> Option<desk::OwnWindow> {
    let own = list();
    if matches!(words_of(name).as_str(), "it" | "that" | "this" | "that one" | "this one" | "es" | "") {
        let front = desk::foreground_own()?;
        return own.into_iter().find(|w| w.hwnd == front);
    }
    // (Its whole name before a part of it: "console" among "Text Console" and "Console log".)
    own.iter().find(|w| words_of(&w.title) == words_of(name)).or_else(|| own.iter().find(|w| names(&w.title, name))).cloned()
}

#[cfg(test)]
thread_local! {
    /// The titles of the windows the test on this thread has open.
    static OPEN: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A test's own windows: these titles are open, for the test on this thread.
#[cfg(test)]
pub fn pretend(titles: &[&str]) {
    OPEN.with(|open| *open.borrow_mut() = titles.iter().map(|t| t.to_string()).collect());
}

/// Whether one of his windows is open that Sir means by `name`.
pub fn open(name: &str) -> bool {
    #[cfg(test)]
    return OPEN.with(|open| open.borrow().iter().any(|title| names(title, name)));
    #[cfg(not(test))]
    return find(name).is_some();
}

/// A rect (left, top, right, bottom, in pixels) as egui's.
fn px([l, t, r, b]: [i32; 4]) -> eframe::egui::Rect {
    eframe::egui::Rect::from_min_max(eframe::egui::pos2(l as f32, t as f32), eframe::egui::pos2(r as f32, b as f32))
}

/// An egui rect as x, y, w, h in pixels.
fn xywh(r: eframe::egui::Rect) -> [i32; 4] {
    [r.min.x.round() as i32, r.min.y.round() as i32, r.width().round() as i32, r.height().round() as i32]
}

/// Where (x, y, w, h) a window standing at `rect` (left, top, right, bottom) goes for a place
/// and a size in the work area `work`, the gap round it: the views' own geometry, in pixels.
/// Exactly where Sir sent it: his other windows make way (`make_room`).
fn within(work: [i32; 4], rect: [i32; 4], position: Option<Position>, size: Option<Size>) -> [i32; 4] {
    xywh(crate::window_commands::target_rect(px(work).shrink(GAP as f32), px(rect), position, size))
}

/// Where each of `windows` (handle, rect: left, top, right, bottom) that stands on `room` goes
/// to make way for it: the nearest spot of its monitor's work area (`work_of`) off `room` and
/// off the others, else where it covers them least (`holo_stage::settle`). (handle, from, to),
/// each x, y, w, h.
pub fn out_of_the_way(room: [i32; 4], windows: &[(isize, [i32; 4])], work_of: impl Fn([i32; 4]) -> [i32; 4]) -> Vec<(isize, [i32; 4], [i32; 4])> {
    let in_the_way = |rect: &[i32; 4]| px(*rect).intersects(px(room).shrink(1.0));
    let mut taken: Vec<_> = std::iter::once(px(room)).chain(windows.iter().filter(|(_, r)| !in_the_way(r)).map(|(_, r)| px(*r))).collect();
    windows
        .iter()
        .filter(|(_, r)| in_the_way(r))
        .map(|(hwnd, rect)| {
            let to = crate::holo_stage::settle(px(work_of(*rect)).shrink(GAP as f32), px(*rect), &taken, GAP as f32, crate::gui::WINDOW_TITLE_ROW);
            taken.push(to);
            (*hwnd, xywh(px(*rect)), xywh(to))
        })
        .collect()
}

/// His windows standing on `room` (left, top, right, bottom), all but `keep`, glide out of its
/// way, unless Sir said "overlap the windows". (Sir, 2026-11-26: "when i say \"move the earth
/// window to the middle of the screen\" and there are already windows there than he should
/// move those windows away".)
pub fn make_room(room: [i32; 4], keep: Option<isize>) {
    if crate::hud_dock::overlap_allowed() {
        return;
    }
    let monitors = crate::monitors::recent();
    let work_of = |[l, t, r, b]: [i32; 4]| monitors.iter().find(|m| m.contains((l + r) / 2, (t + b) / 2)).or_else(|| crate::monitors::primary(&monitors)).map_or([l, t, r, b], |m| m.work);
    let windows: Vec<(isize, [i32; 4])> = list().into_iter().filter(|w| Some(w.hwnd) != keep).map(|w| (w.hwnd, w.rect)).collect();
    for (hwnd, from, to) in out_of_the_way(room, &windows, work_of) {
        glide(hwnd, from, to);
    }
}

/// The window of his Sir means by `name` in front of every other program's window, without the
/// keyboard. Whether one stands there.
pub fn to_front(name: &str) -> bool {
    find(name).map(|w| desk::raise(w.hwnd)).is_some()
}

/// What a window is called in a sentence: its title without his name after it.
pub fn called(title: &str) -> &str {
    title.split('\u{2014}').next().unwrap_or(title).trim()
}

/// "Put the console on the left half", "make the timetable bigger": the window Sir names
/// glides to that part of the monitor it stands on, or to that size. Its title; None: none of
/// his windows is called that (the order is another program's).
pub fn arrange(name: &str, position: Option<Position>, size: Option<Size>) -> Option<String> {
    let window = find(name)?;
    let monitors = crate::monitors::recent();
    let [l, t, r, b] = window.rect;
    let on = monitors.iter().find(|m| m.contains((l + r) / 2, (t + b) / 2)).or_else(|| crate::monitors::primary(&monitors))?;
    let [x, y, w, h] = within(on.work, window.rect, position, size);
    // (A place Sir named is cleared for it; a window made larger where it stands pushes none.)
    if position.is_some() {
        make_room([x, y, x + w, y + h], Some(window.hwnd));
    }
    glide(window.hwnd, [l, t, r - l, b - t], [x, y, w, h]);
    desk::raise(window.hwnd);
    Some(window.title)
}

/// "Move the text console to another monitor": the window Sir names glides to the middle of
/// that monitor (`monitor`: 1, 2, "left", "next" …), at its own size, or the free spot nearest
/// it. None: none of his windows is called that.
pub fn to_monitor(name: &str, monitor: &serde_json::Value) -> Option<Result<String, String>> {
    let window = find(name)?;
    let monitors = crate::monitors::recent();
    let [l, t, r, b] = window.rect;
    let current = crate::monitors::monitor_at(&monitors, (l + r) / 2, (t + b) / 2);
    let Some(index) = crate::monitors::resolve(monitor, current, &monitors) else {
        return Some(Err(format!("There is no such monitor; Sir has {}.", monitors.len())));
    };
    let on = monitors.iter().find(|m| m.index == index)?;
    if current == Some(index) {
        return Some(Ok(format!("My window '{}' is already on Monitor {index}.", called(&window.title))));
    }
    let [x, y, w, h] = within(on.work, window.rect, Some(Position::Centre), None);
    make_room([x, y, x + w, y + h], Some(window.hwnd));
    glide(window.hwnd, [l, t, r - l, b - t], [x, y, w, h]);
    desk::raise(window.hwnd);
    Some(Ok(format!("My window '{}' is on Monitor {index} now.\nSpoken: On monitor {index}, Sir.", called(&window.title))))
}

// ------------------------------------------------------------ on top, and where they were ----

/// The titles of the windows Sir pinned on top, and where each window last stood by its title
/// (x, y, w, h), as the kit's store keeps them ("windows": "pinned", "places").
/// (The newest last, `PLACES_KEPT` at most: cards are titled by what they show.)
static PINNED: Mutex<Option<Vec<String>>> = Mutex::new(None);
static PLACES: Mutex<Option<Vec<(String, [i32; 4])>>> = Mutex::new(None);
const PLACES_KEPT: usize = 48;
/// Each window of his as it opened (`opening_place`), by title: its code's own position and
/// size for it then, and the place it opened at (none: it had none, an order placed it, or the
/// code has moved or sized it since). Let go once the window is gone (`look_after`).
static OPENED: Mutex<Vec<(String, [Option<[f32; 2]>; 2], Option<[i32; 4]>)>> = Mutex::new(Vec::new());
/// Where each stood at the last look, and the windows whose place is not theirs for now (grown
/// into the spotlight).
static LOOKED: Mutex<Vec<(String, [i32; 4])>> = Mutex::new(Vec::new());
static HELD: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Whether his windows' places and pins go to the kit's store and come from it: in Sir's own
/// sessions (a scripted session neither reads where his windows stood nor writes where its own
/// do).
fn stored() -> bool {
    !crate::platform::in_test_session()
}

/// Whether their places are kept at all: also in a scripted session that asks for it
/// (`JAI_DEMO=places`), for as long as it runs.
fn kept() -> bool {
    stored() || crate::platform::demo("places")
}

fn pinned_titles() -> Vec<String> {
    lock(&PINNED)
        .get_or_insert_with(|| if stored() { crate::kit::get("windows", "pinned").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default() } else { Vec::new() })
        .clone()
}

/// Whether the window titled `title` is pinned on top.
pub fn is_pinned(title: &str) -> bool {
    pinned_titles().iter().any(|t| t == title)
}

/// "Keep the console on top", "unpin it": the window Sir names stays above other programs'
/// windows, or no longer. None: none of his windows is called that.
pub fn pin(name: &str, on: bool) -> Option<String> {
    let window = find(name)?;
    pin_titled(&window.title, on);
    // (Pins go by title: every window by this one goes up, or down.)
    for same in list().iter().filter(|w| w.title == window.title) {
        desk::topmost(same.hwnd, on);
    }
    let called = called(&window.title);
    Some(if on { format!("My window '{called}' stays on top now.\nSpoken: {called} stays on top, Sir.") } else { format!("My window '{called}' no longer stays on top.\nSpoken: {called} is no longer on top, Sir.") })
}

/// The window titled `title` is pinned on top from now on, or no longer (its key in its bar).
pub fn pin_titled(title: &str, on: bool) {
    let mut titles = pinned_titles();
    titles.retain(|t| t != title);
    if on {
        titles.push(title.to_string());
    }
    if stored() {
        let _ = crate::kit::put("windows", "pinned", &serde_json::json!(titles));
    }
    *lock(&PINNED) = Some(titles);
}

/// The window titled `title` is somewhere it does not live (grown into the spotlight): where
/// it stands is not kept until it is let go again.
pub fn hold(title: &str, on: bool) {
    let mut held = lock(&HELD);
    held.retain(|t| t != title);
    if on {
        held.push(title.to_string());
    }
}

/// Where the window titled `title` last stood (x, y, w, h), when that is still on a monitor.
pub fn place_of(title: &str) -> Option<[i32; 4]> {
    let place = lock(&PLACES).get_or_insert_with(|| if stored() { crate::kit::get("windows", "places").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default() } else { Vec::new() }).iter().find(|(t, _)| t == title)?.1;
    on_a_monitor(place, &crate::monitors::recent()).then_some(place)
}

/// Where the window titled `title` stands (x, y, w, h) as it opens and after: where it last
/// stood, unless an order of Sir's placed it (`by_order`) or that place is not `free` (another
/// of his windows stands there, or comes up there with it), for as long as the code asks no
/// other place or size of it (`code`: its own position and size for it, as given each frame).
/// (Given anew each frame, the place kept every move and size of the code's from the window.)
pub fn opening_place(title: &str, code: [Option<[f32; 2]>; 2], by_order: bool, free: impl FnOnce([i32; 4]) -> bool) -> Option<[i32; 4]> {
    // (Decided once, as it first opens, and outside the lock: `free` looks at the others.)
    let first = !lock(&OPENED).iter().any(|(t, _, _)| t == title);
    let place = if first && !by_order { place_of(title).filter(|place| free(*place)) } else { None };
    opened_at(&mut lock(&OPENED), title, code, || place)
}

/// The pieces of the board that came up at the place they had before and are still up, by
/// title, with that place (x, y, w, h): before they have shown themselves, Windows does not
/// list them yet. (A view takes only its size from there: it opens in the middle.)
pub fn opening_now() -> Vec<(String, [i32; 4])> {
    lock(&OPENED).iter().filter(|(_, code, _)| code[0].is_some()).filter_map(|(title, _, place)| place.map(|place| (title.clone(), place))).collect()
}

fn opened_at(opened: &mut Vec<(String, [Option<[f32; 2]>; 2], Option<[i32; 4]>)>, title: &str, code: [Option<[f32; 2]>; 2], place: impl FnOnce() -> Option<[i32; 4]>) -> Option<[i32; 4]> {
    let at = match opened.iter().position(|(t, _, _)| t == title) {
        Some(at) => at,
        None => {
            opened.push((title.to_string(), code, place()));
            opened.len() - 1
        }
    };
    let (_, was, place) = &mut opened[at];
    if *was != code {
        *place = None;
    }
    *place
}

/// `moved` kept, each as the newest, and of all only the newest `PLACES_KEPT`.
fn keep(places: &mut Vec<(String, [i32; 4])>, moved: Vec<(String, [i32; 4])>) {
    for (title, rect) in moved {
        places.retain(|(t, _)| *t != title);
        places.push((title, rect));
    }
    let over = places.len().saturating_sub(PLACES_KEPT);
    places.drain(..over);
}

/// Whether the middle of the top edge of `place` (x, y, w, h), where a window is taken hold
/// of, is on one of the monitors.
fn on_a_monitor([x, y, w, h]: [i32; 4], monitors: &[crate::monitors::Monitor]) -> bool {
    w >= 120 && h >= 80 && monitors.iter().any(|m| m.contains(x + w / 2, y + 12))
}

/// What a look at his windows changes in what is kept: a window that stands where it stood at
/// the look before, and not where it is kept, is kept there. (One look is a window on its way.)
/// (Two of his windows by one title have no place of their own: neither is kept.)
fn settled(looked: &[(String, [i32; 4])], now: &[(String, [i32; 4])], kept: &[(String, [i32; 4])]) -> Vec<(String, [i32; 4])> {
    let at = |list: &[(String, [i32; 4])], title: &String, rect: &[i32; 4]| list.iter().any(|(t, r)| t == title && r == rect);
    now.iter().filter(|(title, rect)| now.iter().filter(|(t, _)| t == title).count() == 1 && at(looked, title, rect) && !at(kept, title, rect)).cloned().collect()
}

/// Looks after his windows, every couple of seconds from the Win32 worker: those pinned on top
/// stay there, and where each stands is kept by its title for the next time it opens. (Sir,
/// 2026-11-14: "Windows remember their place", "Pin on top".)
pub fn look_after() {
    let own = list();
    for window in own.iter().filter(|w| is_pinned(&w.title)) {
        desk::topmost(window.hwnd, true);
    }
    // (One that is gone opens anew, at its place again.)
    lock(&OPENED).retain(|(t, _, _)| own.iter().any(|w| w.title == *t));
    if !kept() {
        return;
    }
    let gliding: Vec<isize> = lock(&GLIDES).iter().map(|g| g.0).collect();
    let held = lock(&HELD).clone();
    let now: Vec<(String, [i32; 4])> = own.iter().filter(|w| !gliding.contains(&w.hwnd) && !held.contains(&w.title)).map(|w| (w.title.clone(), [w.rect[0], w.rect[1], w.rect[2] - w.rect[0], w.rect[3] - w.rect[1]])).collect();
    let looked = std::mem::replace(&mut *lock(&LOOKED), now.clone());
    let _ = place_of("");
    let saved = {
        let mut places = lock(&PLACES);
        let Some(places) = places.as_mut() else { return };
        let moved = settled(&looked, &now, places);
        if moved.is_empty() {
            return;
        }
        keep(places, moved);
        serde_json::json!(places)
    };
    // (Written with the lock let go: a window that opens meanwhile asks for its place.)
    if stored() {
        let _ = crate::kit::put("windows", "places", &saved);
    }
}

/// From the HUD's frame: his windows are looked after every couple of seconds, on the Win32
/// worker.
pub fn tend() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = lock(&LAST);
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
        return;
    }
    *last = Some(Instant::now());
    #[cfg(windows)]
    crate::win32::run_detached(look_after);
    #[cfg(not(windows))]
    crate::x11::run_detached(look_after);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-14, Sir's session: "Move the text console to another monitor" found "no open
    /// window" (his own windows were looked for among other programs'), and "close all
    /// windows" left the console standing. His windows answer to their names.
    #[test]
    fn his_windows_answer_to_their_names() {
        for (title, name) in [
            ("Text Console \u{2014} J.A.I.", "the text console"),
            ("Text Console \u{2014} J.A.I.", "text console window"),
            ("Text Console \u{2014} J.A.I.", "console"),
            ("WEBUNTIS // TIMETABLE \u{2014} J.A.I.", "the timetable"),
            ("Projectile \u{2014} J.A.I. Simulation Lab", "the simulation lab"),
            ("Changes \u{2014} J.A.I.", "Changes"),
            // (jai.log, 2026-11-22: "move the protocol window to my main monitor".)
            ("Protocols \u{2014} J.A.I.", "the protocol window"),
        ] {
            assert!(names(title, name), "{name:?} is {title:?}");
        }
        // (A word inside a name is not the name: "make the text bigger" is the HUD's text, and
        // "discord" Sir's program.)
        for (title, name) in [("Text Console \u{2014} J.A.I.", "discord"), ("Changes \u{2014} J.A.I.", "exchange"), ("Changes \u{2014} J.A.I.", ""), ("History \u{2014} J.A.I.", "the window"), ("Text Console \u{2014} J.A.I.", "console text"), ("Text Console \u{2014} J.A.I.", "the text"), ("Discord Audio \u{2014} J.A.I.", "discord"), ("News \u{2014} J.A.I.", "the new window")] {
            assert!(!names(title, name), "{name:?} is not {title:?}");
        }
    }

    #[test]
    fn a_window_is_sent_to_a_half_a_quarter_or_the_middle_of_its_monitor() {
        // A monitor to the right of the first, its taskbar under it; a window standing on it.
        let work = [2560, 0, 5120, 1392];
        let rect = [2700, 200, 3140, 840];
        assert_eq!(within(work, rect, Some(Position::Left), None), [2574, 14, 1266, 1364]);
        assert_eq!(within(work, rect, Some(Position::Right), None), [3840, 14, 1266, 1364]);
        assert_eq!(within(work, rect, Some(Position::TopRight), None), [3840, 14, 1266, 682]);
        assert_eq!(within(work, rect, Some(Position::BottomLeft), None), [2574, 696, 1266, 682]);
        assert_eq!(within(work, rect, None, Some(Size::Percent(100.0))), [2574, 14, 2532, 1364], "the whole screen");
        assert_eq!(within(work, rect, None, Some(Size::Bigger)), [2645, 120, 550, 800], "larger about its middle");
        // Wherever it is sent, at whatever size, it stays inside the work area.
        for position in Position::ALL {
            let [x, y, w, h] = within(work, [0, 0, 9000, 9000], Some(position), None);
            assert!(x >= work[0] && y >= work[1] && x + w <= work[2] && y + h <= work[3], "{position:?}: {x},{y} {w}x{h}");
        }
        // Sir, 2026-11-26: "move the earth window to the middle of the screen" where another of
        // his windows stands. Until then it landed beside that one ("The earth is in the middle,
        // Sir.", and it was not); now it goes exactly there, and only the window in its way
        // makes way, at its own size, onto neither it nor another.
        let middle = within(work, rect, Some(Position::Centre), None);
        assert_eq!(middle, [3620, 376, 440, 640], "in the middle at its own size");
        let room = [middle[0], middle[1], middle[0] + middle[2], middle[1] + middle[3]];
        let (there, aside) = ([3500, 300, 4300, 1000], [4400, 100, 4900, 500]);
        let moves = out_of_the_way(room, &[(1, there), (2, aside)], |_| work);
        assert_eq!(moves.len(), 1, "only the window in the way moves: {moves:?}");
        let (hwnd, from, [x, y, w, h]) = moves[0];
        assert_eq!((hwnd, from, w, h), (1, [3500, 300, 800, 700], 800, 700));
        for other in [room, aside] {
            assert!(!px([x, y, x + w, y + h]).intersects(px(other)), "{x},{y} on {other:?}");
        }
        assert!(x >= work[0] && x + w <= work[2] && y >= work[1] && y + h <= work[3], "off the monitor: {x},{y}");
        // Two in the way: neither lands on the other.
        let moves = out_of_the_way(room, &[(1, there), (3, [3600, 400, 4100, 900])], |_| work);
        assert_eq!(moves.len(), 2);
        let [a, b] = [moves[0].2, moves[1].2].map(|[x, y, w, h]| px([x, y, x + w, y + h]));
        assert!(!a.intersects(b) && !a.intersects(px(room)) && !b.intersects(px(room)), "{a:?} {b:?}");
    }

    #[test]
    fn where_a_window_stood_is_kept_once_it_has_stood_there_for_two_looks() {
        let at = |x: i32| [x, 100, 440, 640];
        let kept = vec![("Text Console \u{2014} J.A.I.".to_string(), at(100))];
        let console = |x: i32| vec![("Text Console \u{2014} J.A.I.".to_string(), at(x))];
        assert!(settled(&console(100), &console(100), &kept).is_empty(), "where it is kept already");
        assert!(settled(&console(100), &console(700), &kept).is_empty(), "on its way: one look is no place");
        assert_eq!(settled(&console(700), &console(700), &kept), console(700), "it stands: kept");
        assert_eq!(settled(&[], &console(700), &kept), vec![], "a window that has only just opened");
        // Two of his windows by one title (two simulations of a kind) have no place of their own.
        let two = vec![("Projectile \u{2014} J.A.I. Simulation Lab".to_string(), at(100)), ("Projectile \u{2014} J.A.I. Simulation Lab".to_string(), at(700))];
        assert!(settled(&two, &two, &kept).is_empty(), "neither is kept");
        // A place is good for the next opening only while a monitor shows it.
        let monitors = vec![crate::monitors::Monitor { index: 1, name: "one".into(), rect: [0, 0, 2560, 1440], work: [0, 0, 2560, 1392], primary: true }];
        assert!(on_a_monitor([100, 100, 440, 640], &monitors));
        assert!(!on_a_monitor([2600, 100, 440, 640], &monitors), "its monitor is gone");
        assert!(!on_a_monitor([100, 100, 40, 20], &monitors), "no size to speak of");
        // The places kept are those of the windows moved last, a few dozen at most (cards are
        // titled by what they show: their titles came without end).
        let mut places = Vec::new();
        keep(&mut places, (0..60).map(|k| (format!("CARD {k}"), at(k))).collect());
        assert_eq!((places.len(), places.last().map(|(t, _)| t.as_str())), (PLACES_KEPT, Some("CARD 59")));
        keep(&mut places, vec![("CARD 20".to_string(), at(5))]);
        assert_eq!(places.iter().filter(|(t, _)| t == "CARD 20").count(), 1, "one place to a title");
        assert_eq!(places.last(), Some(&("CARD 20".to_string(), at(5))), "moved again: the newest");
    }

    /// A window opens where it last stood, and then does what it is told: given anew each frame,
    /// the place kept every move and size the code asked of it from the window ("make the timer
    /// window bigger" answered "Updated", and nothing changed). An order's place stands.
    #[test]
    fn a_window_opens_at_its_place_and_then_does_as_it_is_told() {
        let mut opened = Vec::new();
        let place = Some([300, 200, 640, 480]);
        let code = [Some([10.0, 10.0]), Some([400.0, 300.0])];
        for _ in 0..3 {
            assert_eq!(opened_at(&mut opened, "Timer \u{2014} J.A.I.", code, || place), place, "while the code asks nothing else");
        }
        let bigger = [Some([10.0, 10.0]), Some([500.0, 375.0])];
        assert_eq!(opened_at(&mut opened, "Timer \u{2014} J.A.I.", bigger, || place), None, "the code sizes it: done");
        assert_eq!(opened_at(&mut opened, "Timer \u{2014} J.A.I.", code, || place), None, "and its place is not taken up again");
        assert_eq!(opened_at(&mut opened, "Radar \u{2014} J.A.I.", code, || None), None, "placed by an order, or never placed");
        // Closed, it opens at its place again.
        opened.clear();
        assert_eq!(opened_at(&mut opened, "Timer \u{2014} J.A.I.", bigger, || place), place);
    }

    /// Sir, 2026-11-19: "i asked jai to open 3 random windows and he did but the windows are
    /// all placed on top of each other". Each had last been open alone, in the first place
    /// beside the core, and each came up there again. Now only the first piece does; the others
    /// come up where the board puts them, off it and off each other.
    #[test]
    fn windows_coming_up_together_never_share_a_spot() {
        let _g = crate::hud_dock::test_lock();
        crate::monitors::pretend(Some(crate::visual_ui_verifier::desk()));
        let work = crate::monitors::recent()[0].work;
        let spot = [work[0] + 200, work[1] + 150, 640, 480];
        let titles = ["Spot probe one \u{2014} J.A.I.", "Spot probe two \u{2014} J.A.I.", "Spot probe three \u{2014} J.A.I."];
        lock(&PLACES).get_or_insert_with(Vec::new).extend(titles.map(|t| (t.to_string(), spot)));
        let came: Vec<eframe::egui::Rect> = titles
            .iter()
            .enumerate()
            .map(|(k, title)| {
                // (Pieces of the board: the board gives each its own place, well apart.)
                let board_puts = [work[0] as f32 + 200.0 + 900.0 * k as f32, work[1] as f32 + 150.0];
                let builder = crate::gui::for_test_session(eframe::egui::ViewportBuilder::default().with_title(*title).with_position(board_puts).with_inner_size([640.0, 480.0]));
                eframe::egui::Rect::from_min_size(builder.position.expect("placed"), builder.inner_size.expect("sized"))
            })
            .collect();
        let at_their_place: Vec<(String, [i32; 4])> = opening_now().into_iter().filter(|(t, _)| titles.contains(&t.as_str())).collect();
        if let Some(places) = lock(&PLACES).as_mut() {
            places.retain(|(t, _)| !titles.contains(&t.as_str()));
        }
        lock(&OPENED).retain(|(t, _, _)| !titles.contains(&t.as_str()));
        crate::monitors::pretend(None);
        assert_eq!(at_their_place, vec![(titles[0].to_string(), spot)], "only the first comes up where it stood");
        for (k, a) in came.iter().enumerate() {
            for b in &came[k + 1..] {
                assert!(!a.shrink(1.0).intersects(*b), "two on one spot: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn a_glide_starts_fast_ends_slow_and_arrives_exactly() {
        let (from, to) = ([100, 100, 520, 460], [1400, 300, 520, 460]);
        assert_eq!(between(from, to, 0.0), from);
        assert_eq!(between(from, to, 1.0), to);
        assert_eq!(between(from, to, 2.0), to, "never past it");
        let (quarter, half) = (between(from, to, 0.25), between(from, to, 0.5));
        assert!(quarter[0] - from[0] > to[0] - half[0], "the first quarter covers more than the second half");
        // No step of sixty a second is a jump.
        let mut last = from;
        for k in 1..=17 {
            let now = between(from, to, k as f32 / 17.0);
            assert!((now[0] - last[0]).abs() < 260, "{} px in one frame", now[0] - last[0]);
            assert!(now[0] >= last[0]);
            last = now;
        }
    }

    #[test]
    fn windows_line_up_in_a_grid_that_fills_the_monitor() {
        let work = [0, 0, 2560, 1392];
        assert!(cells(work, 0).is_empty());
        assert_eq!(cells(work, 1), vec![[14, 14, 2532, 1364]]);
        let four = cells(work, 4);
        assert_eq!(four.len(), 4);
        assert_eq!((four[0][0], four[0][1]), (14, 14));
        assert_eq!(four[1][0], four[0][0] + four[0][2] + 14, "a gap between two in a row");
        assert_eq!(four[2][1], four[0][1] + four[0][3] + 14, "and between two rows");
        // Five: three columns and two rows, none outside the monitor, none on another.
        let five = cells(work, 5);
        for (k, c) in five.iter().enumerate() {
            assert!(c[0] >= 0 && c[1] >= 0 && c[0] + c[2] <= 2560 && c[1] + c[3] <= 1392, "{c:?}");
            for o in &five[k + 1..] {
                assert!(c[0] + c[2] <= o[0] || o[0] + o[2] <= c[0] || c[1] + c[3] <= o[1] || o[1] + o[3] <= c[1], "{c:?} lies on {o:?}");
            }
        }
        // The upright monitor stacks them.
        let tall = cells([-1080, -290, 0, 1582], 3);
        assert!(tall.iter().all(|c| c[0] == -1066), "one column: {tall:?}");
    }

    #[test]
    fn lined_up_they_keep_their_order_and_their_size_where_it_fits() {
        let work = [0, 0, 2560, 1392];
        // (As they lie about: the second one stands top left.)
        let windows = [(1, [1500, 700, 2020, 1160]), (2, [60, 40, 700, 560]), (3, [900, 60, 2500, 1500])];
        let moves = lined_up(work, &windows);
        assert_eq!(moves.iter().map(|m| m.0).collect::<Vec<_>>(), vec![2, 3, 1], "in reading order");
        let (_, from, to) = moves[0];
        assert_eq!(from, [60, 40, 640, 520]);
        assert_eq!((to[0], to[1], to[2], to[3]), (14, 14, 640, 520), "its own size, in the first cell");
        // The one larger than a cell is made as small as its cell.
        let (_, _, big) = moves[1];
        let cell = cells(work, 3)[1];
        assert_eq!((big[2], big[3]), (cell[2], cell[3]));
    }
}
