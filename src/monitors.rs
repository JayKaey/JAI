//! Multi-monitor routing: every connected display, numbered the way Sir sees them (Monitor 1 =
//! the primary, then Windows' own display numbers), "left"/"right"/"next" resolution by
//! position, and moving
//! any application window — or Jarvis himself — from one monitor to another.
//!
//! Windows: `EnumDisplayMonitors` + `GetMonitorInfoW`, windows moved with `SetWindowPos`
//! (restored first and maximised again when they were maximised). Linux: `hyprctl monitors -j`
//! or `xrandr --listmonitors`, windows moved with `hyprctl dispatch movewindow` / `wmctrl`.

use serde_json::{json, Value};

/// One display. Rectangles are (left, top, right, bottom) in physical pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// 1-based, as Sir counts them.
    pub index: usize,
    pub name: String,
    pub rect: [i32; 4],
    pub work: [i32; 4],
    pub primary: bool,
}

impl Monitor {
    pub fn width(&self) -> i32 {
        self.rect[2] - self.rect[0]
    }

    pub fn height(&self) -> i32 {
        self.rect[3] - self.rect[1]
    }

    /// "Monitor 2 (1920×1080, right of the primary)".
    pub fn describe(&self) -> String {
        format!("Monitor {}{} ({}×{} at {},{})", self.index, if self.primary { " [primary]" } else { "" }, self.width(), self.height(), self.rect[0], self.rect[1])
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.rect[0] && x < self.rect[2] && y >= self.rect[1] && y < self.rect[3]
    }
}

thread_local! {
    /// A monitor setup this thread sees instead of the real one (`pretend`).
    static PRETEND: std::cell::RefCell<Option<Vec<Monitor>>> = const { std::cell::RefCell::new(None) };
}

/// Lets this thread see `setup` as the connected monitors (None: the real ones again). The
/// offscreen HUD and the tests lay a spanned board out on setups this machine does not have.
pub fn pretend(setup: Option<Vec<Monitor>>) {
    PRETEND.with(|p| *p.borrow_mut() = setup);
}

/// The monitors as they were at most a second ago: the HUD asks in every frame.
pub fn recent() -> Vec<Monitor> {
    static SEEN: std::sync::Mutex<Option<(std::time::Instant, Vec<Monitor>)>> = std::sync::Mutex::new(None);
    if let Some(setup) = PRETEND.with(|p| p.borrow().clone()) {
        return setup;
    }
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    match seen.as_ref() {
        Some((at, known)) if at.elapsed() < std::time::Duration::from_secs(1) => known.clone(),
        _ => {
            let now = list();
            *seen = Some((std::time::Instant::now(), now.clone()));
            now
        }
    }
}

/// The monitor the HUD lives on when it is told nothing else: the primary one.
pub fn primary(monitors: &[Monitor]) -> Option<&Monitor> {
    monitors.iter().find(|m| m.primary).or_else(|| monitors.first())
}

/// All monitors, numbered: the primary is Monitor 1, the others follow Windows' numbering.
#[cfg(windows)]
pub fn list() -> Vec<Monitor> {
    if let Some(setup) = PRETEND.with(|p| p.borrow().clone()) {
        return setup;
    }
    crate::win32::displays()
        .into_iter()
        .enumerate()
        .map(|(i, d)| Monitor { index: i + 1, name: d.device.trim_start_matches("\\\\.\\").to_string(), rect: d.rect, work: d.work, primary: d.primary })
        .collect()
}

/// The highest refresh rate (Hz) among the connected monitors, as Windows has it set right
/// now; None when it is not known.
pub fn fastest_refresh_hz() -> Option<u32> {
    #[cfg(windows)]
    return crate::win32::fastest_refresh_hz();
    #[cfg(not(windows))]
    None
}

#[cfg(not(windows))]
pub fn list() -> Vec<Monitor> {
    if let Some(setup) = PRETEND.with(|p| p.borrow().clone()) {
        return setup;
    }
    let run = |p: &str, a: &[&str]| {
        std::process::Command::new(p).args(a).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    };
    let mut raw: Vec<(String, [i32; 4], [i32; 4], bool)> = Vec::new();
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        if let Some(v) = run("hyprctl", &["monitors", "-j"]).and_then(|j| serde_json::from_str::<Value>(&j).ok()) {
            for m in v.as_array().into_iter().flatten() {
                let n = |k: &str| m[k].as_i64().unwrap_or(0) as i32;
                let (x, y, w, h) = (n("x"), n("y"), n("width"), n("height"));
                // `reserved` = [left, top, right, bottom] taken by bars.
                let r: Vec<i32> = m["reserved"].as_array().map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0) as i32).collect()).unwrap_or_default();
                let res = |i: usize| r.get(i).copied().unwrap_or(0);
                raw.push((
                    m["name"].as_str().unwrap_or("").to_string(),
                    [x, y, x + w, y + h],
                    [x + res(0), y + res(1), x + w - res(2), y + h - res(3)],
                    m["id"].as_i64() == Some(0),
                ));
            }
        }
    } else if let Some(out) = run("xrandr", &["--listmonitors"]) {
        // " 0: +*DP-1 2560/597x1440/336+0+0  DP-1"
        for line in out.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let (Some(flags), Some(geom), Some(name)) = (parts.get(1), parts.get(2), parts.last()) else { continue };
            let nums: Vec<i32> = geom.split(['x', '+', '/']).filter_map(|s| s.parse().ok()).collect();
            if nums.len() >= 6 {
                let (w, h, x, y) = (nums[0], nums[2], nums[4], nums[5]);
                raw.push((name.to_string(), [x, y, x + w, y + h], [x, y, x + w, y + h], flags.contains('*')));
            }
        }
    }
    raw.sort_by_key(|(_, r, _, primary)| (!*primary, r[0], r[1]));
    raw.into_iter().enumerate().map(|(i, (name, rect, work, primary))| Monitor { index: i + 1, name, rect, work, primary }).collect()
}

/// The monitor a phrase or number means: 1, "2", "monitor 3", "second", "left", "right",
/// "primary"/"main", "next"/"other" (relative to `current`, the monitor the window is on now).
pub fn resolve(target: &Value, current: Option<usize>, monitors: &[Monitor]) -> Option<usize> {
    if monitors.is_empty() {
        return None;
    }
    let count = monitors.len();
    if let Some(n) = target.as_u64().or_else(|| target.as_f64().map(|f| f as u64)) {
        return (1..=count as u64).contains(&n).then_some(n as usize);
    }
    let raw = target.as_str()?.trim().to_lowercase();
    let words: String = raw.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let words: Vec<&str> = words.split_whitespace().filter(|w| !matches!(*w, "monitor" | "screen" | "display" | "bildschirm" | "the" | "my" | "to" | "on" | "den" | "meinen" | "mein")).collect();
    let word = words.join(" ");
    if let Ok(n) = word.parse::<usize>() {
        return (1..=count).contains(&n).then_some(n);
    }
    let by_x = |leftmost: bool| {
        let mut sorted: Vec<&Monitor> = monitors.iter().collect();
        sorted.sort_by_key(|m| m.rect[0]);
        if leftmost { sorted.first().map(|m| m.index) } else { sorted.last().map(|m| m.index) }
    };
    let ordinal = |w: &str| -> Option<usize> {
        Some(match w {
            "first" | "1st" | "one" | "erste" | "ersten" | "eins" => 1,
            "second" | "2nd" | "two" | "zweite" | "zweiten" | "zwei" => 2,
            "third" | "3rd" | "three" | "dritte" | "dritten" | "drei" => 3,
            "fourth" | "4th" | "four" | "vierte" | "vierten" | "vier" => 4,
            _ => return None,
        })
    };
    match word.as_str() {
        "primary" | "main" | "haupt" | "hauptbildschirm" | "primären" | "primary one" => monitors.iter().find(|m| m.primary).map(|m| m.index).or(Some(1)),
        "left" | "left one" | "linken" | "linke" | "links" => by_x(true),
        "right" | "right one" | "rechten" | "rechte" | "rechts" => by_x(false),
        "next" | "other" | "another" | "anderen" | "andere" | "nächsten" | "naechsten" => {
            let cur = current.unwrap_or(1);
            Some(cur % count + 1)
        }
        "previous" | "vorherigen" => {
            let cur = current.unwrap_or(1);
            Some(if cur <= 1 { count } else { cur - 1 })
        }
        w => {
            let n = w.split_whitespace().find_map(ordinal).or_else(|| w.split_whitespace().find_map(|x| x.parse().ok()))?;
            (1..=count).contains(&n).then_some(n)
        }
    }
}

/// The live-context line: "Monitors: 1 [primary] 2560×1440 at 0,0; 2 1920×1080 at 2560,0".
pub fn context_line(monitors: &[Monitor]) -> String {
    if monitors.is_empty() {
        return "Monitors: unknown".into();
    }
    format!(
        "Monitors: {}",
        monitors
            .iter()
            .map(|m| format!("{}{} {}×{} at {},{}", m.index, if m.primary { " [primary]" } else { "" }, m.width(), m.height(), m.rect[0], m.rect[1]))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// The virtual desktop: the box (x, y, width, height) around every monitor ("use all monitors").
pub fn virtual_bounds(monitors: &[Monitor]) -> Option<[i32; 4]> {
    let left = monitors.iter().map(|m| m.rect[0]).min()?;
    let top = monitors.iter().map(|m| m.rect[1]).min()?;
    let right = monitors.iter().map(|m| m.rect[2]).max()?;
    let bottom = monitors.iter().map(|m| m.rect[3]).max()?;
    Some([left, top, right - left, bottom - top])
}

/// Which monitor a point is on (1-based), if any.
pub fn monitor_at(monitors: &[Monitor], x: i32, y: i32) -> Option<usize> {
    monitors.iter().find(|m| m.contains(x, y)).map(|m| m.index)
}

/// Moves the top window of an application (named, or the one Sir is working in) to a monitor.
#[cfg(windows)]
pub async fn move_app(target: crate::winctl::WindowTarget, monitor: Value) -> Result<String, String> {
    use crate::win32;
    crate::win32::call(move || {
        let monitors = list();
        if monitors.is_empty() {
            return Err("Windows reported no monitors.".to_string());
        }
        let window = crate::winctl::find_windows(&target)?.into_iter().next().ok_or_else(|| format!("No open window matches {}.", target.describe()))?;
        let current = win32::window_display_rect(window.hwnd).and_then(|r| monitor_at(&monitors, (r[0] + r[2]) / 2, (r[1] + r[3]) / 2));
        let index = resolve(&monitor, current, &monitors).ok_or_else(|| {
            format!("There is no monitor '{}'; Sir has {} monitor(s): {}.", monitor_label(&monitor), monitors.len(), monitors.iter().map(Monitor::describe).collect::<Vec<_>>().join(", "))
        })?;
        let m = &monitors[index - 1];
        if current == Some(index) {
            win32::focus(window.hwnd);
            return Ok(format!("'{}' is already on Monitor {index}.", short(&window.title)));
        }
        let display = win32::DisplayInfo { rect: m.rect, work: m.work, primary: m.primary, device: m.name.clone() };
        win32::move_to_display(window.hwnd, &display)?;
        Ok(format!("Moved '{}' ({}) to Monitor {index}.", short(&window.title), window.process))
    })
    .await?
}

#[cfg(not(windows))]
pub async fn move_app(target: crate::winctl::WindowTarget, monitor: Value) -> Result<String, String> {
    tokio::task::spawn_blocking(move || linux_move(&target, &monitor)).await.map_err(|e| e.to_string())?
}

#[cfg(not(windows))]
fn linux_move(target: &crate::winctl::WindowTarget, monitor: &Value) -> Result<String, String> {
    let monitors = list();
    let index = resolve(monitor, None, &monitors).ok_or_else(|| format!("There is no monitor '{}'.", monitor_label(monitor)))?;
    let m = &monitors[index - 1];
    let run = |p: &str, a: &[&str]| -> Result<String, String> {
        let out = std::process::Command::new(p).args(a).output().map_err(|e| format!("{p}: {e}"))?;
        if out.status.success() { Ok(String::from_utf8_lossy(&out.stdout).to_string()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    };
    let name = match target {
        crate::winctl::WindowTarget::Named(t) | crate::winctl::WindowTarget::Focus(t) => t.clone(),
        crate::winctl::WindowTarget::Handle { title, .. } => title.clone(),
        crate::winctl::WindowTarget::Active => String::new(),
    };
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let clients: Value = serde_json::from_str(&run("hyprctl", &["clients", "-j"])?).map_err(|e| e.to_string())?;
        let client = clients
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| {
                name.is_empty()
                    || c["class"].as_str().unwrap_or("").to_lowercase().contains(&name.to_lowercase())
                    || c["title"].as_str().unwrap_or("").to_lowercase().contains(&name.to_lowercase())
            })
            .ok_or_else(|| format!("No open window matches '{name}'."))?;
        let address = client["address"].as_str().unwrap_or("");
        run("hyprctl", &["dispatch", "focuswindow", &format!("address:{address}")])?;
        run("hyprctl", &["dispatch", "movewindow", &format!("mon:{}", m.name)])?;
        return Ok(format!("Moved '{}' to Monitor {index}.", client["title"].as_str().unwrap_or(&name)));
    }
    let listing = run("wmctrl", &["-lx"])?;
    let line = listing
        .lines()
        .find(|l| name.is_empty() || l.to_lowercase().contains(&name.to_lowercase()))
        .ok_or_else(|| format!("No open window matches '{name}'."))?;
    let id = line.split_whitespace().next().unwrap_or("");
    run("wmctrl", &["-i", "-r", id, "-b", "remove,maximized_vert,maximized_horz"])?;
    run("wmctrl", &["-i", "-r", id, "-e", &format!("0,{},{},-1,-1", m.work[0] + 40, m.work[1] + 40)])?;
    run("wmctrl", &["-i", "-a", id])?;
    Ok(format!("Moved the window to Monitor {index}."))
}

fn monitor_label(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn short(title: &str) -> String {
    title.chars().take(60).collect()
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "move_window_to_monitor",
        "description": "Move any application window (Discord, Spotify, Sonora, Chrome, VS Code, a game …) or one of YOUR windows (an id/title from the live context, e.g. pc_stats, drive_space) to another monitor. app_or_window_name \"it\"/\"that\" = the app or Jarvis window just discussed; \"yourself\" = your HUD. target_monitor: 1, 2, 3, \"left\", \"right\", \"primary\", \"next\".",
        "parameters": {"type": "object", "properties": {
            "app_or_window_name": {"type": "string"},
            "target_monitor": {"type": "string", "description": "1, 2, 3, left, right, primary or next"}
        }, "required": ["app_or_window_name", "target_monitor"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Vec<Monitor> {
        vec![
            Monitor { index: 1, name: "DISPLAY1".into(), rect: [0, 0, 2560, 1440], work: [0, 0, 2560, 1400], primary: true },
            Monitor { index: 2, name: "DISPLAY2".into(), rect: [-1920, 0, 0, 1080], work: [-1920, 0, 0, 1040], primary: false },
            Monitor { index: 3, name: "DISPLAY3".into(), rect: [2560, 0, 4480, 1080], work: [2560, 0, 4480, 1040], primary: false },
        ]
    }

    #[test]
    fn monitor_phrases_resolve() {
        let m = setup();
        assert_eq!(resolve(&json!(2), None, &m), Some(2));
        assert_eq!(resolve(&json!("3"), None, &m), Some(3));
        assert_eq!(resolve(&json!("monitor 2"), None, &m), Some(2));
        assert_eq!(resolve(&json!("second screen"), None, &m), Some(2));
        assert_eq!(resolve(&json!("my second monitor"), None, &m), Some(2));
        assert_eq!(resolve(&json!("left"), None, &m), Some(2));
        assert_eq!(resolve(&json!("right"), None, &m), Some(3));
        assert_eq!(resolve(&json!("primary"), Some(3), &m), Some(1));
        assert_eq!(resolve(&json!("next"), Some(3), &m), Some(1));
        assert_eq!(resolve(&json!("next"), Some(1), &m), Some(2));
        assert_eq!(resolve(&json!(5), None, &m), None);
        assert_eq!(resolve(&json!("zweiten bildschirm"), None, &m), Some(2));
        assert_eq!(monitor_at(&m, -100, 500), Some(2));
        assert!(context_line(&m).starts_with("Monitors: 1 [primary] 2560×1440"));
    }

    #[test]
    fn spanning_covers_the_box_around_every_monitor() {
        let m = setup();
        // DISPLAY2 sits left of the primary, DISPLAY3 right of it; the tallest is 1440.
        assert_eq!(virtual_bounds(&m), Some([-1920, 0, 6400, 1440]));
        assert_eq!(virtual_bounds(&m[..1]), Some([0, 0, 2560, 1440]));
        assert_eq!(virtual_bounds(&[]), None);
        assert_eq!(resolve(&json!("3rd monitor"), None, &m), Some(3));
    }
}
