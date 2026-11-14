//! Sir's monitors and what stands on them (2026-11-13): their real brightness by voice ("dim
//! the side monitors", "brightness 40 percent"), desk layouts ("save this layout as study",
//! "study layout": every window back on its monitor and place), and the dashboard for an
//! upright monitor (`desktop_backdrop` draws it).
//!
//! Brightness goes over DDC/CI, the monitors' own control channel (`dxva2`): it is what the
//! buttons on the monitor set, not a dark veil over the picture. A monitor that does not
//! answer on that channel says so. A test and a scripted session read at most, and set nothing.

use crate::kit::say;
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

// -------------------------------------------------------------------------- brightness ----

/// Which monitors an order is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    All,
    /// Every monitor but the main one.
    Side,
    Main,
    /// By its number (1 = the first in Windows' order, as everywhere in J.A.I.).
    Number(usize),
}

impl Which {
    fn of(text: &str) -> Which {
        let t = text.to_lowercase();
        let number = t.split(|c: char| !c.is_ascii_digit()).find_map(|d| d.parse::<usize>().ok()).filter(|n| (1..=8).contains(n));
        if let Some(n) = number {
            Which::Number(n)
        } else if ["side", "other", "second", "secondary", "neben", "seiten", "anderen"].iter().any(|w| t.contains(w)) {
            Which::Side
        } else if ["main", "primary", "middle", "haupt"].iter().any(|w| t.contains(w)) {
            Which::Main
        } else {
            Which::All
        }
    }

    fn name(self) -> String {
        match self {
            Which::All => "all".to_string(),
            Which::Side => "side".to_string(),
            Which::Main => "main".to_string(),
            Which::Number(n) => n.to_string(),
        }
    }

    fn takes(self, index: usize, primary: bool) -> bool {
        match self {
            Which::All => true,
            Which::Side => !primary,
            Which::Main => primary,
            Which::Number(n) => n == index,
        }
    }

    fn words(self) -> String {
        match self {
            Which::All => "your monitors".to_string(),
            Which::Side => "the side monitors".to_string(),
            Which::Main => "the main monitor".to_string(),
            Which::Number(n) => format!("monitor {n}"),
        }
    }
}

const SCREENS: &str = r"(?:(?:the\s+|my\s+|all\s+(?:the\s+|my\s+)?)?(?:side\s+|other\s+|second(?:ary)?\s+|main\s+|primary\s+|middle\s+)?(?:monitors?|screens?|displays?)(?:\s+\d)?|monitor\s+\d|(?:den\s+|die\s+|meine\s+)?(?:neben|seiten|haupt)?(?:bildschirme?|monitore?))";

static BRIGHT_SET: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:set\s+|put\s+|turn\s+|stell\s+)?(?:the\s+|my\s+|die\s+)?(?:monitor\s+|screen\s+|display\s+)?(?:brightness|helligkeit)(?:\s+(?:of|on|for|von)\s+(?P<which>{SCREENS}))?\s+(?:to\s+|at\s+|auf\s+)?(?P<n>\d{{1,3}})(?:\s*(?:percent|prozent|%))?$|^(?:set\s+|put\s+|turn\s+)?(?P<which2>{SCREENS})\s+(?:to|at|auf)\s+(?P<n2>\d{{1,3}})(?:\s*(?:percent|prozent|%))?(?:\s+brightness)?$"
    ))
});
static BRIGHT_DIM: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:(?P<dim>dim|darken|dimme?)|(?P<bright>brighten|light\s+up))\s+(?P<which>{SCREENS})$")));
static BRIGHT_STEP: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:make\s+|turn\s+|mach\s+)?(?P<which>{SCREENS})\s+(?:(?P<up>brighter|up|heller)|(?P<down>darker|dimmer|down|dunkler))$|^(?:(?:monitor|screen)\s+)?(?:brightness|helligkeit)\s+(?:(?P<up2>up|higher|hoch|rauf)|(?P<down2>down|lower|runter))$|^(?:turn\s+)?(?:(?P<up3>up)|(?P<down3>down))\s+the\s+brightness$"
    ))
});
static BRIGHT_FULL: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:full|maximum|max)\s+(?:monitor\s+|screen\s+)?brightness$|^volle\s+helligkeit$"));
static BRIGHT_STATUS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:what(?:s|\s+is)\s+(?:the\s+|my\s+)?)?(?:monitor\s+|screen\s+)?brightness$|^how\s+bright\s+(?:are|is)\s+(?:my\s+|the\s+)?(?:monitors?|screens?)$|^wie\s+hell\s+(?:sind|ist)\s+(?:meine\s+|der\s+|die\s+)?(?:bildschirme?|monitore?)$"));

/// "Brightness 40 percent", "dim the side monitors", "make the screens brighter", "how bright
/// are my monitors".
pub fn brightness_command(s: &str) -> Option<Value> {
    if BRIGHT_STATUS.is_match(s) {
        return Some(json!({"action": "status"}));
    }
    if BRIGHT_FULL.is_match(s) {
        return Some(json!({"action": "set", "level": 100, "monitors": "all"}));
    }
    if let Some(c) = BRIGHT_SET.captures(s) {
        let level: u64 = c.name("n").or(c.name("n2"))?.as_str().parse().ok()?;
        let which = c.name("which").or(c.name("which2")).map_or(Which::All, |m| Which::of(m.as_str()));
        return (level <= 100).then(|| json!({"action": "set", "level": level, "monitors": which.name()}));
    }
    if let Some(c) = BRIGHT_DIM.captures(s) {
        let which = Which::of(c.name("which")?.as_str());
        return Some(json!({"action": "set", "level": if c.name("dim").is_some() { 20 } else { 100 }, "monitors": which.name()}));
    }
    let c = BRIGHT_STEP.captures(s)?;
    let up = c.name("up").or(c.name("up2")).or(c.name("up3")).is_some();
    let which = c.name("which").map_or(Which::All, |m| Which::of(m.as_str()));
    Some(json!({"action": "step", "by": if up { 15 } else { -15 }, "monitors": which.name()}))
}

/// One monitor's brightness as its own control channel reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    /// Its number (1-based, Windows' order).
    pub index: usize,
    pub primary: bool,
    /// Its brightness in percent of its own range, or why it cannot be read.
    pub percent: Result<u32, String>,
}

#[cfg(windows)]
mod ddc {
    use super::Level;
    use windows::Win32::Foundation::{BOOL, HANDLE, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};

    /// `PHYSICAL_MONITOR` of physicalmonitorenumerationapi.h (packed to one byte there).
    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    struct PhysicalMonitor {
        handle: HANDLE,
        description: [u16; 128],
    }

    #[link(name = "dxva2")]
    extern "system" {
        fn GetNumberOfPhysicalMonitorsFromHMONITOR(monitor: HMONITOR, count: *mut u32) -> BOOL;
        fn GetPhysicalMonitorsFromHMONITOR(monitor: HMONITOR, count: u32, monitors: *mut PhysicalMonitor) -> BOOL;
        fn GetMonitorBrightness(monitor: HANDLE, min: *mut u32, current: *mut u32, max: *mut u32) -> BOOL;
        fn SetMonitorBrightness(monitor: HANDLE, level: u32) -> BOOL;
        fn DestroyPhysicalMonitors(count: u32, monitors: *const PhysicalMonitor) -> BOOL;
    }

    /// Every monitor's handle, where it stands and whether it is the main one, left to right as
    /// `monitors::list` numbers them.
    fn handles() -> Vec<(usize, bool, HMONITOR)> {
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the &mut Vec passed below and outlives EnumDisplayMonitors.
            let found = unsafe { &mut *(lparam.0 as *mut Vec<(HMONITOR, RECT, bool)>) };
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            // SAFETY: a plain query on the handle the enumeration gave.
            if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                found.push((monitor, info.rcMonitor, info.dwFlags & 1 != 0));
            }
            TRUE
        }
        let mut found: Vec<(HMONITOR, RECT, bool)> = Vec::new();
        // SAFETY: the callback only touches `found` through the pointer it is given.
        unsafe {
            let _ = EnumDisplayMonitors(HDC(0), None, Some(each), LPARAM(&mut found as *mut Vec<(HMONITOR, RECT, bool)> as isize));
        }
        let known = crate::monitors::list();
        found
            .into_iter()
            .enumerate()
            .map(|(k, (handle, rect, primary))| {
                let index = known.iter().find(|m| m.rect == [rect.left, rect.top, rect.right, rect.bottom]).map_or(k + 1, |m| m.index);
                (index, primary, handle)
            })
            .collect()
    }

    /// Runs `f` on the first physical monitor behind a display handle.
    fn with_physical<R>(monitor: HMONITOR, f: impl FnOnce(HANDLE) -> Result<R, String>) -> Result<R, String> {
        let mut count = 0u32;
        // SAFETY: `count` is a valid out pointer; the array below has exactly `count` entries
        // and is released with DestroyPhysicalMonitors.
        unsafe {
            if !GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count).as_bool() || count == 0 {
                return Err("it has no control channel (DDC/CI)".to_string());
            }
            let mut physical = vec![PhysicalMonitor { handle: HANDLE(0), description: [0; 128] }; count as usize];
            if !GetPhysicalMonitorsFromHMONITOR(monitor, count, physical.as_mut_ptr()).as_bool() {
                return Err("its control channel did not open".to_string());
            }
            let handle = physical[0].handle;
            let result = f(handle);
            let _ = DestroyPhysicalMonitors(count, physical.as_ptr());
            result
        }
    }

    fn range(handle: HANDLE) -> Result<(u32, u32, u32), String> {
        let (mut min, mut now, mut max) = (0u32, 0u32, 0u32);
        // SAFETY: three valid out pointers and a handle from GetPhysicalMonitorsFromHMONITOR.
        if unsafe { GetMonitorBrightness(handle, &mut min, &mut now, &mut max) }.as_bool() && max > min {
            Ok((min, now, max))
        } else {
            Err("it does not answer on its control channel (DDC/CI may be switched off in its menu)".to_string())
        }
    }

    pub fn read() -> Vec<Level> {
        handles()
            .into_iter()
            .map(|(index, primary, monitor)| Level { index, primary, percent: with_physical(monitor, |h| range(h).map(|(min, now, max)| ((now.saturating_sub(min)) * 100 + (max - min) / 2) / (max - min))) })
            .collect()
    }

    /// Sets the monitors `takes` picks to `to(their percent now)`; each one's percent before
    /// and after, or why not.
    pub fn set(takes: impl Fn(usize, bool) -> bool, to: impl Fn(u32) -> u32) -> Vec<(usize, Result<(u32, u32), String>)> {
        handles()
            .into_iter()
            .filter(|(index, primary, _)| takes(*index, *primary))
            .map(|(index, _, monitor)| {
                let done = with_physical(monitor, |h| {
                    let (min, now, max) = range(h)?;
                    let before = ((now.saturating_sub(min)) * 100 + (max - min) / 2) / (max - min);
                    let wanted = to(before).min(100);
                    let raw = min + (wanted * (max - min) + 50) / 100;
                    // SAFETY: the handle is the one `range` just read from.
                    if unsafe { SetMonitorBrightness(h, raw) }.as_bool() { Ok((before, wanted)) } else { Err("it refused the new brightness".to_string()) }
                });
                (index, done)
            })
            .collect()
    }
}

/// What the monitors stood at before the last change (for "undo that").
static BEFORE: Mutex<Vec<(usize, u32)>> = Mutex::new(Vec::new());

/// Puts the monitors back where they stood before the last brightness order. Whether there
/// was anything to put back.
pub async fn brightness_back() -> bool {
    let before: Vec<(usize, u32)> = std::mem::take(&mut *lock(&BEFORE));
    if before.is_empty() || crate::platform::hands_off() {
        return false;
    }
    #[cfg(windows)]
    for (index, level) in before {
        let _ = crate::win32::call(move || ddc::set(|i, _| i == index, |_| level)).await;
    }
    true
}

pub async fn brightness(args: &Value) -> Result<String, String> {
    let action = args["action"].as_str().unwrap_or("status");
    #[cfg(not(windows))]
    {
        let _ = action;
        Err("Monitor brightness is only built for Windows.".to_string())
    }
    #[cfg(windows)]
    {
        if action == "back" {
            return if brightness_back().await { Ok(say("The monitors are as bright as they were.", "As it was, Sir.")) } else { Err("There is no change of brightness to take back, Sir.".to_string()) };
        }
        if action == "status" {
            if cfg!(test) {
                return Err("A test reads no monitors.".to_string());
            }
            let levels = crate::win32::call(ddc::read).await.map_err(|e| e.to_string())?;
            let lines: Vec<String> = levels
                .iter()
                .map(|l| match &l.percent {
                    Ok(p) => format!("Monitor {}{}: {p} %", l.index, if l.primary { " (main)" } else { "" }),
                    Err(why) => format!("Monitor {}{}: {why}", l.index, if l.primary { " (main)" } else { "" }),
                })
                .collect();
            let known: Vec<String> = levels.iter().filter_map(|l| l.percent.as_ref().ok().map(|p| format!("monitor {} at {p} percent", l.index))).collect();
            let spoken = if known.is_empty() { "None of your monitors tells me its brightness, Sir. Their control channel may be switched off in their menus.".to_string() } else { format!("{}, Sir.", capitalised(&known.join(", "))) };
            return Ok(say(lines.join("\n"), spoken));
        }
        if crate::platform::hands_off() {
            return Err("A test session leaves Sir's monitors as they are.".to_string());
        }
        let which = match args["monitors"].as_str().unwrap_or("all") {
            "side" => Which::Side,
            "main" => Which::Main,
            "all" => Which::All,
            n => n.parse().map(Which::Number).unwrap_or(Which::All),
        };
        let (level, by) = (args["level"].as_u64().map(|l| l.min(100) as u32), args["by"].as_i64().unwrap_or(0));
        let done = crate::win32::call(move || {
            ddc::set(
                |index, primary| which.takes(index, primary),
                |now| match level {
                    Some(level) => level,
                    None => (now as i64 + by).clamp(5, 100) as u32,
                },
            )
        })
        .await
        .map_err(|e| e.to_string())?;
        if done.is_empty() {
            return Err(format!("I see no monitor that is {}, Sir.", which.words()));
        }
        let changed: Vec<(usize, u32, u32)> = done.iter().filter_map(|(i, r)| r.as_ref().ok().map(|(before, after)| (*i, *before, *after))).collect();
        let refused: Vec<String> = done.iter().filter_map(|(i, r)| r.as_ref().err().map(|why| format!("Monitor {i}: {why}"))).collect();
        if changed.is_empty() {
            return Err(format!("{} would not take it, Sir: {}.", capitalised(&which.words()), refused.join("; ")));
        }
        *lock(&BEFORE) = changed.iter().map(|(i, before, _)| (*i, *before)).collect();
        tracing::info!("[BRIGHTNESS] {}", changed.iter().map(|(i, b, a)| format!("monitor {i}: {b} % to {a} %")).collect::<Vec<_>>().join(", "));
        let to = changed[0].2;
        let mut lines: Vec<String> = changed.iter().map(|(i, b, a)| format!("Monitor {i}: {b} % → {a} %")).collect();
        lines.extend(refused);
        Ok(say(lines.join("\n"), format!("{} at {to} percent, Sir.", capitalised(&which.words()))))
    }
}

fn capitalised(text: &str) -> String {
    let mut c = text.chars();
    c.next().map_or(String::new(), |first| first.to_uppercase().chain(c).collect())
}

// ------------------------------------------------------------------------ desk layouts ----

const LAYOUT: &str = "layout";

static LAYOUT_RESTORE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:restore|load|apply|bring\s+back|set\s+up)\s+(?:the\s+|my\s+)?(?:layout|desk)\s+(.+)$|^(?:restore|load|apply|bring\s+back|set\s+up|give\s+me)\s+(?:the\s+|my\s+)?(.+?)\s+(?:layout|desk)$|^arrange\s+(?:my\s+)?windows\s+(?:like|as|for)\s+(.+)$|^stell\s+(?:das\s+)?layout\s+(.+?)\s+wieder\s+her$")
});
/// "Restore the desk layout standard": asked for as a desk layout in so many words.
static LAYOUT_DESK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:restore|load|apply|bring\s+back|set\s+up)\s+(?:the\s+|my\s+)?desk\s+layout\s+(.+)$"));
/// "Switch to my standard layout", "use the study layout": his once he has kept one of that name.
static LAYOUT_SWITCH: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:switch\s+to|go\s+to|change\s+to|use|activate)\s+(?:the\s+|my\s+)?(.+?)\s+(?:desk\s+)?layout$"));
static LAYOUT_BARE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:the\s+|my\s+)?(.+?)\s+layout$|^layout\s+(?P<first>.+)$"));
static LAYOUT_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:list|show)\s+(?:me\s+)?(?:my\s+|the\s+|all\s+)?(?:desk\s+|window\s+)?layouts$|^what\s+layouts\s+do\s+i\s+have$|^(?:zeig\s+(?:mir\s+)?)?meine\s+layouts$"));
static LAYOUT_DELETE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:delete|remove|forget)\s+(?:the\s+|my\s+)?(?:desk\s+)?layout\s+(.+)$|^(?:delete|remove|forget)\s+(?:the\s+|my\s+)?(.+?)\s+(?:desk\s+)?layout$|^lösche?\s+das\s+layout\s+(.+)$"));

fn first<'a>(c: &'a regex::Captures) -> Option<&'a str> {
    (1..c.len()).find_map(|k| c.get(k)).map(|m| m.as_str().trim())
}

fn layout_key(name: &str) -> String {
    crate::ear::words(name).join(" ")
}

/// A protocol that puts windows in their places is a desk layout.
fn is_layout(protocol: &crate::protocols::Protocol) -> bool {
    protocol.steps.iter().any(|s| s.tool == "place_application_window")
}

/// The names of Sir's desk layouts, kept where the fast path can see them ("study layout" is
/// only an order for one that exists; "compact layout" is the HUD's own). Called now and then
/// off the HUD's thread: a layout is saved as a protocol (`manage_stark_protocol`).
pub fn sync_layouts(tools: &ToolContext) {
    let names: Vec<String> = tools.protocols.list().iter().filter(|p| is_layout(p)).map(|p| layout_key(&p.name)).collect();
    let known: Vec<String> = crate::kit::list(LAYOUT).into_iter().map(|(key, _, _)| key).collect();
    for name in names.iter().filter(|n| !known.contains(n)) {
        let _ = crate::kit::put(LAYOUT, name, &json!({"name": name}));
    }
    for gone in known.iter().filter(|k| !names.contains(k)) {
        crate::kit::remove(LAYOUT, gone);
    }
}

/// "Study layout", "restore the study layout", "list my layouts", "delete the study layout".
/// (Saving one is the older order "save this layout as study": it captures the workspace.)
pub fn layout_command(s: &str) -> Option<Value> {
    if LAYOUT_LIST.is_match(s) {
        return Some(json!({"action": "list"}));
    }
    if let Some(name) = LAYOUT_DELETE.captures(s).as_ref().and_then(first).filter(|name| crate::kit::get(LAYOUT, &layout_key(name)).is_some()) {
        return Some(json!({"action": "delete", "name": name}));
    }
    // ("Restore the default layout", "give me the compact layout" are the HUD's own: a name
    // of its layouts is no desk layout unless Sir saved one under it.)
    const THE_HUDS: &[&str] = &["default", "compact", "full", "orb", "normal", "standard", "minimal", "zen", "previous", "last", "old", "original", "usual", "same"];
    // ("Apply the desk layout standard": he says whose layout it is, so the name is his own
    // whatever the HUD calls its layouts. Sir, 2026-11-13.)
    if let Some(name) = LAYOUT_DESK.captures(s).as_ref().and_then(first).filter(|name| layout_key(name).split(' ').count() <= 3) {
        return Some(json!({"action": "restore", "name": name}));
    }
    if let Some(name) = LAYOUT_RESTORE.captures(s).as_ref().and_then(first).filter(|name| !THE_HUDS.contains(&layout_key(name).as_str()) || crate::kit::get(LAYOUT, &layout_key(name)).is_some()) {
        return Some(json!({"action": "restore", "name": name}));
    }
    // "Switch to my standard layout": a layout he has kept under that name (until he has, it
    // is the HUD's own "standard" as it always was).
    if let Some(name) = LAYOUT_SWITCH.captures(s).as_ref().and_then(first).filter(|name| crate::kit::get(LAYOUT, &layout_key(name)).is_some()) {
        return Some(json!({"action": "restore", "name": name}));
    }
    // The bare "<name> layout": only for a layout that exists ("compact layout" is the HUD's).
    let c = LAYOUT_BARE.captures(s)?;
    let name = first(&c)?;
    if crate::kit::get(LAYOUT, &layout_key(name)).is_some() {
        return Some(json!({"action": "restore", "name": name}));
    }
    // "Layout standard", the name last (2026-11-13: the language model made a protocol called
    // "jay_layout_standard" of it): a desk layout by its name, kept or not; he says so when
    // there is none. The HUD's own layouts keep their meaning ("layout compact"), all but
    // "standard", which is what Sir calls his desk. (One word: "layout study laden" and
    // "layout of the page" are no names.)
    let key = layout_key(name);
    let the_huds_own = key != "standard" && (crate::interface::HudLayout::parse(&key).is_some() || THE_HUDS.contains(&key.as_str()));
    (c.name("first").is_some() && !the_huds_own && !key.contains(' ') && key.chars().count() >= 2).then(|| json!({"action": "restore", "name": name}))
}

pub async fn layout(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let name = args["name"].as_str().unwrap_or("").trim();
    let found = |name: &str| ctx.protocols.list().into_iter().find(|p| is_layout(p) && layout_key(&p.name) == layout_key(name));
    match args["action"].as_str().unwrap_or("list") {
        "restore" => {
            let Some(protocol) = found(name) else {
                return Err(format!("I have no layout called \"{name}\", Sir. Arrange your windows and say \"save this layout as {name}\"."));
            };
            let outcome = Box::pin(ctx.execute("manage_stark_protocol", &json!({"action": "execute_protocol", "protocol_name": protocol.name}))).await;
            if !outcome.success {
                return Err(outcome.output.trim_start_matches("Error: ").to_string());
            }
            let windows = protocol.steps.iter().filter(|s| s.tool == "place_application_window").count();
            Ok(say(format!("Layout \"{}\": {windows} windows put back. {}", protocol.name, outcome.output.lines().next().unwrap_or("")), format!("Your {} layout, Sir.", protocol.name)))
        }
        "delete" => {
            let Some(protocol) = found(name) else { return Err(format!("I have no layout called \"{name}\", Sir.")) };
            ctx.protocols.delete(&protocol.name);
            crate::kit::remove(LAYOUT, &layout_key(&protocol.name));
            Ok(say(format!("Layout \"{}\" deleted.", protocol.name), "Deleted, Sir."))
        }
        _ => {
            sync_layouts(ctx);
            let all: Vec<crate::protocols::Protocol> = ctx.protocols.list().into_iter().filter(is_layout).collect();
            if all.is_empty() {
                return Ok(say("No layouts yet. Arrange your windows and say \"save this layout as study\".", "You have no layouts yet, Sir. Arrange your windows and say: save this layout as study."));
            }
            let lines: Vec<String> = all.iter().map(|p| format!("{}  \u{00B7}  {} windows", p.name, p.steps.iter().filter(|s| s.tool == "place_application_window").count())).collect();
            crate::kit::show_text("kit_layouts", "Desk layouts // your windows, by name", &format!("{}\n\nSay \"<name> layout\" to put every window back, \"save this layout as <name>\" to keep a new one.", lines.join("\n")), [480.0, 300.0]);
            Ok(say(lines.join("\n"), format!("{} layout{}, Sir. On the board.", all.len(), if all.len() == 1 { "" } else { "s" })))
        }
    }
}

// ------------------------------------------------------------------ portrait dashboard ----

static DASH_ON: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:show|open|start|turn\s+on|switch\s+on)\s+)?(?:the\s+|my\s+)?(?:portrait|upright|vertical|side)\s+dashboard(?:\s+on)?$|^dashboard\s+on\s+(?:my\s+|the\s+)?(?:portrait|upright|vertical|third)\s+(?:monitor|screen)$|^(?:hochkant|seiten)\s*dashboard(?:\s+an)?$")
});
static DASH_OFF: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:(?:hide|close|stop|turn\s+off|switch\s+off)\s+)(?:the\s+|my\s+)?(?:portrait|upright|vertical|side)\s+dashboard$|^(?:portrait|upright|vertical|side)\s+dashboard\s+off$|^(?:hochkant|seiten)\s*dashboard\s+aus$"));

/// "Portrait dashboard", "dashboard on my upright monitor", "portrait dashboard off".
pub fn dashboard_command(s: &str) -> Option<Value> {
    if DASH_OFF.is_match(s) {
        return Some(json!({"on": false}));
    }
    DASH_ON.is_match(s).then(|| json!({"on": true}))
}

pub fn dashboard(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let on = args["on"].as_bool().unwrap_or(true);
    if on && !crate::monitors::recent().iter().any(|m| m.height() > m.width()) {
        return Err("None of your monitors stands upright, Sir. The dashboard is made for one that does.".to_string());
    }
    ctx.config.update(|c| c.interface.portrait_dashboard = on);
    crate::gui::refresh();
    Ok(if on {
        say("The dashboard stands on your upright monitor, behind your windows: the clock, what plays, your notes, the day so far.", "On your upright monitor, Sir, behind your windows.")
    } else {
        say("The dashboard is off.", "The dashboard is off, Sir.")
    })
}

/// What the dashboard shows beside the clock, gathered off the HUD's thread every few seconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dash {
    /// The song that plays ("Title — Artist"), if one does.
    pub playing: Option<String>,
    /// The newest notes, newest first.
    pub notes: Vec<String>,
    /// Today's screen time: the day's total and the apps that took most of it.
    pub day: Option<(String, Vec<(String, String)>)>,
    /// The reminders that come next.
    pub next: Vec<String>,
    /// The sleep timer, whom he answers to, and other short states.
    pub states: Vec<String>,
    /// The newest notifications.
    pub notices: Vec<String>,
}

static DASH: Mutex<Option<Dash>> = Mutex::new(None);

pub fn dash() -> Dash {
    lock(&DASH).clone().unwrap_or_default()
}

/// Whom he answers to right now, in a few words (the HUD's indicator and the dashboard).
pub fn lock_state(lock_on: bool, voice_known: bool) -> String {
    if !lock_on {
        "ANSWERS TO EVERYONE".to_string()
    } else if crate::speaker_id::guests_welcome() {
        match crate::speaker_id::guests_until() {
            Some(until) => format!("GUESTS UNTIL {until}"),
            None => "GUESTS WELCOME".to_string(),
        }
    } else if voice_known {
        "ANSWERS TO YOU ALONE".to_string()
    } else {
        "LEARNING YOUR VOICE".to_string()
    }
}

/// Gathers what the dashboard shows (called every few seconds from the kit's background task).
pub fn gather(tools: &ToolContext) {
    // (For the dashboard on the upright monitor, and for the day on the background.)
    if !tools.config.read(|c| c.interface.portrait_dashboard || c.interface.desktop_background) {
        *lock(&DASH) = None;
        return;
    }
    let playing = crate::music_lyrics::track().filter(|t| t.playing && !t.title.trim().is_empty()).map(|t| if t.artist.trim().is_empty() { t.title.clone() } else { format!("{} \u{2014} {}", t.title, t.artist) });
    let notes: Vec<String> = crate::kit::list("note").into_iter().rev().take(4).filter_map(|(_, v, _)| v["text"].as_str().map(str::to_string)).collect();
    let (apps, total) = tools.stark.features.bench.screen.today();
    let day = (total >= 60).then(|| (crate::screen_time::span(total), apps.iter().take(4).map(|(app, secs)| (app.clone(), crate::screen_time::span(*secs))).collect()));
    let mut reminders = tools.reminders.list();
    reminders.sort_by_key(|r| r.due_ms);
    let next: Vec<String> = reminders.iter().take(3).map(|r| format!("{}  {}", r.due().format("%a %H:%M"), r.text)).collect();
    let mut states = vec![lock_state(tools.config.read(|c| c.interface.voice_lock), crate::hud_feel::voice_known())];
    states.extend(crate::kit_notes::sleep_left().map(|left| left.to_uppercase()));
    if crate::kit_typing::dictating() {
        states.push("DICTATING".to_string());
    }
    let notices: Vec<String> = crate::hud_notify::recent(chrono::Local::now()).into_iter().take(3).map(|n| n.text).collect();
    *lock(&DASH) = Some(Dash { playing, notes, day, next, states, notices });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_is_asked_for_by_level_step_and_monitor() {
        for (said, want) in [
            ("brightness 40", json!({"action": "set", "level": 40, "monitors": "all"})),
            ("set the brightness to 40 percent", json!({"action": "set", "level": 40, "monitors": "all"})),
            ("brightness of monitor 2 to 30", json!({"action": "set", "level": 30, "monitors": "2"})),
            ("helligkeit auf 60", json!({"action": "set", "level": 60, "monitors": "all"})),
            ("set the side monitors to 25 percent", json!({"action": "set", "level": 25, "monitors": "side"})),
            ("dim the side monitors", json!({"action": "set", "level": 20, "monitors": "side"})),
            ("dim my screens", json!({"action": "set", "level": 20, "monitors": "all"})),
            ("brighten the main monitor", json!({"action": "set", "level": 100, "monitors": "main"})),
            ("full brightness", json!({"action": "set", "level": 100, "monitors": "all"})),
            ("make the screens brighter", json!({"action": "step", "by": 15, "monitors": "all"})),
            ("monitors darker", json!({"action": "step", "by": -15, "monitors": "all"})),
            ("brightness down", json!({"action": "step", "by": -15, "monitors": "all"})),
            ("how bright are my monitors", json!({"action": "status"})),
        ] {
            assert_eq!(brightness_command(said), Some(want), "{said}");
        }
        // His HUD's own darkness, the volume and plain talk are not the monitors' brightness.
        for said in ["darker", "make it darker", "turn it up", "brightness 400", "the screen is too bright today", "dim the lights", "brighter"] {
            assert_eq!(brightness_command(said), None, "{said}");
        }
        assert_eq!((Which::of("the side monitors"), Which::of("monitor 3"), Which::of("my screens"), Which::of("the main monitor")), (Which::Side, Which::Number(3), Which::All, Which::Main));
        assert!(Which::Side.takes(2, false) && !Which::Side.takes(1, true) && Which::Number(3).takes(3, false) && !Which::Number(3).takes(2, false));
    }

    #[test]
    fn a_layout_is_brought_back_by_name_and_the_bare_name_only_when_it_exists() {
        let _one = crate::kit::test_lock();
        // Saving one stays the older order (it captures the workspace as a protocol).
        assert_eq!(layout_command("save this layout as study"), None);
        assert!(matches!(crate::intents::parse("Save this layout as study"), Some(crate::intents::Intent::Tool { name: "manage_stark_protocol", .. })));
        assert_eq!(layout_command("list my layouts"), Some(json!({"action": "list"})));
        // Asked for in so many words: always (he says so when there is none of that name).
        assert_eq!(layout_command("restore my study layout"), Some(json!({"action": "restore", "name": "study"})));
        assert_eq!(layout_command("arrange my windows like study"), Some(json!({"action": "restore", "name": "study"})));
        // The bare name: "compact layout" is the HUD's own, "study layout" his once it exists.
        assert_eq!(layout_command("restore the default layout"), None);
        assert_eq!(layout_command("give me the compact layout"), None);
        assert_eq!(layout_command("compact layout"), None);
        assert_eq!(layout_command("study layout"), None);
        assert_eq!(layout_command("delete the study layout"), None);
        // Asked for as a desk layout in so many words, or with the name last ("Jay Layout
        // Standard", 2026-11-13): his by that name, kept or not. The HUD's own keep their meaning.
        assert_eq!(layout_command("restore the desk layout standard"), Some(json!({"action": "restore", "name": "standard"})));
        assert_eq!(layout_command("apply my desk layout work"), Some(json!({"action": "restore", "name": "work"})));
        assert_eq!(layout_command("layout standard"), Some(json!({"action": "restore", "name": "standard"})));
        assert_eq!(layout_command("layout gaming"), Some(json!({"action": "restore", "name": "gaming"})));
        for said in ["layout compact", "layout full", "layout orb", "layout default", "layout of the page", "layout study laden", "layout", "standard layout"] {
            assert_eq!(layout_command(said), None, "{said}");
        }
        crate::kit::put(LAYOUT, "study", &json!({"name": "study"})).unwrap();
        assert_eq!(layout_command("study layout"), Some(json!({"action": "restore", "name": "study"})));
        assert_eq!(layout_command("delete the study layout"), Some(json!({"action": "delete", "name": "study"})));
        assert_eq!(layout_command("compact layout"), None);
    }

    #[test]
    fn the_dashboard_is_asked_for_by_its_name_and_says_whom_he_answers_to() {
        assert_eq!(dashboard_command("portrait dashboard"), Some(json!({"on": true})));
        assert_eq!(dashboard_command("show the upright dashboard"), Some(json!({"on": true})));
        assert_eq!(dashboard_command("dashboard on my portrait monitor"), Some(json!({"on": true})));
        assert_eq!(dashboard_command("portrait dashboard off"), Some(json!({"on": false})));
        assert_eq!(dashboard_command("close the portrait dashboard"), Some(json!({"on": false})));
        assert_eq!(dashboard_command("open the dashboard"), None);
        crate::speaker_id::end_guests();
        assert_eq!(lock_state(false, true), "ANSWERS TO EVERYONE");
        assert_eq!(lock_state(true, true), "ANSWERS TO YOU ALONE");
        assert_eq!(lock_state(true, false), "LEARNING YOUR VOICE");
    }

    /// The monitors of this PC as they answer (read only: nothing is set).
    /// `cargo test --bin jai the_monitors_answer -- --ignored --nocapture`
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn the_monitors_answer_on_their_control_channel() {
        for level in ddc::read() {
            println!("monitor {}{}: {:?}", level.index, if level.primary { " (main)" } else { "" }, level.percent);
        }
    }
}
