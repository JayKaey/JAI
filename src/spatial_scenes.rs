//! Spatial mission profiles: complete multi-window layouts across all monitors, saved by name
//! in SQLite (`spatial_scenes`) and recalled with spring-animated glides ("load the deep
//! research matrix", "save this layout as late-night coding").
//!
//! Window positions are stored relative to the work area of a monitor *role* (primary, the
//! secondary landscape screen, the portrait screen), so a profile still makes sense when the
//! desk changes. Four Stark profiles are built in.

use crate::tools::ToolContext;
use crate::workspace_canvas::{ScenePlacement, SpatialWindowKind};
use eframe::egui::{pos2, vec2, Rect};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tracing::{info, warn};

/// One connected display (physical virtual-desktop pixels).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorInfo {
    /// 1-based, primary first (the numbering "monitor 2" uses everywhere in JAI).
    pub number: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Work area (without the taskbar): x, y, width, height.
    pub work: [i32; 4],
    pub is_portrait: bool,
    pub is_primary: bool,
    pub device: String,
}

impl MonitorInfo {
    pub fn work_rect(&self) -> Rect {
        let [x, y, w, h] = self.work;
        Rect::from_min_size(pos2(x as f32, y as f32), vec2(w as f32, h as f32))
    }
}

/// Every display via `EnumDisplayMonitors` + `GetMonitorInfoW`, primary first, then by
/// Windows' display number and position.
#[cfg(windows)]
pub fn detect_connected_monitors() -> Vec<MonitorInfo> {
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Vec passed below; it outlives EnumDisplayMonitors.
        let list = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: a monitor handle from the enumeration; cbSize names the extended struct.
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
            let (r, w) = (info.monitorInfo.rcMonitor, info.monitorInfo.rcWork);
            let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
            let (width, height) = (r.right - r.left, r.bottom - r.top);
            list.push(MonitorInfo {
                number: 0,
                x: r.left,
                y: r.top,
                width,
                height,
                work: [w.left, w.top, w.right - w.left, w.bottom - w.top],
                is_portrait: height > width,
                // MONITORINFOF_PRIMARY = 1
                is_primary: info.monitorInfo.dwFlags & 1 != 0,
                device: String::from_utf16_lossy(&info.szDevice[..len]),
            });
        }
        TRUE
    }
    let mut list: Vec<MonitorInfo> = Vec::new();
    // SAFETY: the callback only touches `list` through the pointer.
    unsafe {
        let _ = EnumDisplayMonitors(HDC(0), None, Some(collect), LPARAM(&mut list as *mut Vec<MonitorInfo> as isize));
    }
    sort_and_number(list)
}

/// Linux: the monitors as X11 tells them (`monitors::list`); one full-HD stand-in without X.
#[cfg(not(windows))]
pub fn detect_connected_monitors() -> Vec<MonitorInfo> {
    let list: Vec<MonitorInfo> = crate::monitors::list()
        .into_iter()
        .map(|m| {
            let (width, height) = (m.width(), m.height());
            let [l, t, r, b] = m.work;
            MonitorInfo { number: 0, x: m.rect[0], y: m.rect[1], width, height, work: [l, t, r - l, b - t], is_portrait: height > width, is_primary: m.primary, device: m.name }
        })
        .collect();
    if list.is_empty() {
        return sort_and_number(vec![MonitorInfo { number: 0, x: 0, y: 0, width: 1920, height: 1080, work: [0, 0, 1920, 1080], is_portrait: false, is_primary: true, device: "default".into() }]);
    }
    sort_and_number(list)
}

fn sort_and_number(mut list: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    let number = |m: &MonitorInfo| m.device.trim_start_matches("\\\\.\\DISPLAY").parse::<u32>().unwrap_or(u32::MAX);
    list.sort_by_key(|m| (!m.is_primary, number(m), m.x, m.y));
    for (i, m) in list.iter_mut().enumerate() {
        m.number = i + 1;
    }
    list
}

/// Where a window of a profile lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorRole {
    Primary,
    /// The first landscape screen that is not the primary.
    Secondary,
    /// The first portrait screen.
    Portrait,
    /// A fixed monitor number (1-based).
    Number(usize),
}

/// The monitor for a role, and whether it fell back to the primary.
pub fn resolve(role: MonitorRole, monitors: &[MonitorInfo]) -> Option<(MonitorInfo, bool)> {
    let primary = monitors.iter().find(|m| m.is_primary).or_else(|| monitors.first())?.clone();
    let others = || monitors.iter().filter(|m| !m.is_primary);
    let found = match role {
        MonitorRole::Primary => Some(primary.clone()),
        MonitorRole::Secondary => others().find(|m| !m.is_portrait).or_else(|| others().next()).cloned(),
        MonitorRole::Portrait => monitors.iter().find(|m| m.is_portrait && !m.is_primary).or_else(|| others().find(|m| !m.is_portrait)).cloned(),
        MonitorRole::Number(n) => monitors.iter().find(|m| m.number == n).cloned(),
    };
    Some(match found {
        Some(m) => (m, false),
        None => (primary, role != MonitorRole::Primary),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneWindow {
    pub kind: SpatialWindowKind,
    pub monitor: MonitorRole,
    /// x, y, width, height as fractions of the monitor's work area.
    pub rel: [f32; 4],
    #[serde(default)]
    pub ghost: bool,
    #[serde(default)]
    pub pinned: bool,
    /// Background alpha (0-255); default when absent.
    #[serde(default)]
    pub alpha: Option<u8>,
}

/// The HUD/orb part of a profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrbPlacement {
    pub orb_mode: bool,
    /// "center", "top-right", …
    pub anchor: String,
    pub monitor: MonitorRole,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialScene {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub windows: Vec<SceneWindow>,
    #[serde(default)]
    pub orb: Option<OrbPlacement>,
    /// Clears the GPU when loaded (gaming).
    #[serde(default)]
    pub flush_vram: bool,
}

fn w(kind: SpatialWindowKind, monitor: MonitorRole, rel: [f32; 4]) -> SceneWindow {
    SceneWindow { kind, monitor, rel, ghost: false, pinned: false, alpha: None }
}

/// The four Stark mission profiles.
pub fn builtin_scenes() -> Vec<SpatialScene> {
    use MonitorRole::*;
    use SpatialWindowKind::*;
    vec![
        SpatialScene {
            name: "Deep Research Matrix".into(),
            description: "The orb in the centre, crawler stream and knowledge graph on the left, dossier and inspector on the right.".into(),
            windows: vec![
                w(LiveCrawlerStream, Primary, [0.01, 0.02, 0.30, 0.47]),
                w(KnowledgeGraphPinboard, Primary, [0.01, 0.51, 0.30, 0.47]),
                w(ExecutiveDossier, Primary, [0.69, 0.02, 0.30, 0.47]),
                w(CodeAndSandboxInspector, Primary, [0.69, 0.51, 0.30, 0.47]),
            ],
            orb: Some(OrbPlacement { orb_mode: true, anchor: "center".into(), monitor: Primary }),
            flush_vram: false,
        },
        SpatialScene {
            name: "Coding & Engineering Deck".into(),
            description: "Hardware/VRAM HUD and the code inspector on the portrait (or secondary) monitor; the main monitor stays clear for the IDE.".into(),
            windows: vec![w(HardwareAndVramHud, Portrait, [0.03, 0.02, 0.94, 0.40]), w(CodeAndSandboxInspector, Portrait, [0.03, 0.44, 0.94, 0.54])],
            orb: None,
            flush_vram: false,
        },
        SpatialScene {
            name: "Gaming Stealth HUD".into(),
            description: "GPU cleared to 0 MB, the orb tucked into the top-right corner and a mini click-through hardware HUD over the game.".into(),
            windows: vec![SceneWindow { kind: HardwareAndVramHud, monitor: Primary, rel: [0.005, 0.01, 0.16, 0.17], ghost: true, pinned: true, alpha: Some(crate::window_physics::GHOST_ALPHA) }],
            orb: Some(OrbPlacement { orb_mode: true, anchor: "top-right".into(), monitor: Primary }),
            flush_vram: true,
        },
        SpatialScene {
            name: "Morning Briefing Cockpit".into(),
            description: "Timetable, calendar and protocols next to the dossier.".into(),
            windows: vec![w(ScheduleAndCommandDeck, Primary, [0.12, 0.08, 0.37, 0.84]), w(ExecutiveDossier, Primary, [0.51, 0.08, 0.37, 0.84])],
            orb: None,
            flush_vram: false,
        },
    ]
}

/// Physical rectangles for a profile on these monitors. A window whose monitor is missing
/// lands in a narrow strip on the right of the primary, so the main screen stays usable.
pub fn place(scene: &SpatialScene, monitors: &[MonitorInfo]) -> Vec<ScenePlacement> {
    scene
        .windows
        .iter()
        .filter_map(|sw| {
            let (m, fallback) = resolve(sw.monitor, monitors)?;
            let area = m.work_rect();
            let [mut x, y, mut wd, h] = sw.rel;
            if fallback {
                x = 0.72 + x * 0.27;
                wd *= 0.27;
            }
            let rect = Rect::from_min_size(
                pos2(area.left() + x * area.width(), area.top() + y * area.height()),
                vec2((wd * area.width()).max(260.0), (h * area.height()).max(150.0)),
            );
            Some(ScenePlacement { kind: sw.kind, rect, ghost: sw.ghost, pinned: sw.pinned, alpha: sw.alpha })
        })
        .collect()
}

static DB_PATH: OnceLock<String> = OnceLock::new();

fn open() -> Result<Connection, String> {
    let path = DB_PATH.get().ok_or("Spatial scenes are not initialised.")?;
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(3));
    Ok(conn)
}

/// Creates the table and (re)installs the built-in profiles.
pub fn init(db_path: &str) {
    let _ = DB_PATH.set(db_path.to_string());
    let result = open().and_then(|c| {
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS spatial_scenes (
                 name TEXT PRIMARY KEY COLLATE NOCASE,
                 scene_json TEXT NOT NULL,
                 builtin INTEGER NOT NULL DEFAULT 0,
                 saved_at TEXT NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;
        for scene in builtin_scenes() {
            let json = serde_json::to_string(&scene).map_err(|e| e.to_string())?;
            c.execute(
                "INSERT INTO spatial_scenes (name, scene_json, builtin, saved_at) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(name) DO UPDATE SET scene_json = excluded.scene_json WHERE spatial_scenes.builtin = 1",
                params![scene.name, json, chrono::Local::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    if let Err(e) = result {
        warn!("[SCENES] Spatial scenes unavailable: {e}");
    }
}

pub fn list_scenes() -> Vec<(String, bool, String)> {
    let Ok(c) = open() else { return Vec::new() };
    let Ok(mut st) = c.prepare("SELECT name, builtin, scene_json FROM spatial_scenes ORDER BY builtin DESC, name") else { return Vec::new() };
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0, r.get::<_, String>(2)?)))
        .map(|rows| {
            rows.flatten()
                .map(|(name, builtin, json)| {
                    let description = serde_json::from_str::<SpatialScene>(&json).map(|s| s.description).unwrap_or_default();
                    (name, builtin, description)
                })
                .collect()
        })
        .unwrap_or_default();
    rows
}

/// A profile by name (case-insensitive, also by a unique part: "research matrix").
pub fn find_scene(name: &str) -> Option<SpatialScene> {
    let c = open().ok()?;
    let exact: Option<String> = c.query_row("SELECT scene_json FROM spatial_scenes WHERE name = ?1", [name.trim()], |r| r.get(0)).optional().ok().flatten();
    let json = exact.or_else(|| {
        let wanted = name.trim().to_lowercase().replace("profile", "").replace("scene", "").replace("layout", "");
        let wanted = wanted.trim();
        list_scenes()
            .into_iter()
            .map(|(n, _, _)| n)
            .find(|n| {
                let low = n.to_lowercase();
                !wanted.is_empty() && (low.contains(wanted) || wanted.split_whitespace().filter(|w| w.len() > 2).all(|w| low.contains(w)))
            })
            .and_then(|n| c.query_row("SELECT scene_json FROM spatial_scenes WHERE name = ?1", [n], |r| r.get(0)).optional().ok().flatten())
    })?;
    serde_json::from_str(&json).ok()
}

/// Stores (or replaces) one of Sir's own profiles; a built-in is never overwritten.
pub fn store_scene(scene: &SpatialScene) -> Result<(), String> {
    let json = serde_json::to_string(scene).map_err(|e| e.to_string())?;
    open()?
        .execute(
            "INSERT INTO spatial_scenes (name, scene_json, builtin, saved_at) VALUES (?1, ?2, 0, ?3)
             ON CONFLICT(name) DO UPDATE SET scene_json = excluded.scene_json, saved_at = excluded.saved_at WHERE spatial_scenes.builtin = 0",
            params![scene.name, json, chrono::Local::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Saves the current arrangement of the workspace windows (and the orb mode) as `name`.
pub fn save_spatial_scene(tools: &ToolContext, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("What shall I call this layout, Sir?".into());
    }
    if builtin_scenes().iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
        return Err(format!("\"{name}\" is a built-in profile; please choose another name."));
    }
    let monitors = detect_connected_monitors();
    let first_portrait = monitors.iter().find(|m| m.is_portrait && !m.is_primary).map(|m| m.number);
    let first_secondary = monitors.iter().find(|m| !m.is_primary && !m.is_portrait).map(|m| m.number);
    let windows: Vec<SceneWindow> = crate::workspace_canvas::window_placements()
        .into_iter()
        .filter_map(|p| {
            let c = p.rect.center();
            let m = monitors.iter().find(|m| m.work_rect().contains(c)).or_else(|| monitors.first())?;
            let area = m.work_rect();
            let role = if m.is_primary {
                MonitorRole::Primary
            } else if Some(m.number) == first_portrait {
                MonitorRole::Portrait
            } else if Some(m.number) == first_secondary {
                MonitorRole::Secondary
            } else {
                MonitorRole::Number(m.number)
            };
            Some(SceneWindow {
                kind: p.kind,
                monitor: role,
                rel: [
                    (p.rect.left() - area.left()) / area.width(),
                    (p.rect.top() - area.top()) / area.height(),
                    p.rect.width() / area.width(),
                    p.rect.height() / area.height(),
                ],
                ghost: p.ghost,
                pinned: p.pinned,
                alpha: p.alpha,
            })
        })
        .collect();
    if windows.is_empty() {
        return Err("There are no workspace windows open to save, Sir.".into());
    }
    let iface = tools.config.read(|c| c.interface.clone());
    let orb = iface.orb_mode.then(|| OrbPlacement { orb_mode: true, anchor: iface.orb_anchor.clone(), monitor: MonitorRole::Number(iface.orb_monitor.max(1) as usize) });
    let scene = SpatialScene { name: name.to_string(), description: format!("{} windows, saved {}", windows.len(), chrono::Local::now().format("%d.%m.%Y %H:%M")), windows, orb, flush_vram: false };
    store_scene(&scene)?;
    info!("[SCENES] Saved \"{name}\" ({} windows)", scene.windows.len());
    Ok(format!("Layout \"{name}\" saved: {} windows across {} monitor(s).", scene.windows.len(), monitors.len()))
}

/// Recalls a profile: windows glide to their places (missing ones fan out of the orb, others
/// are pulled back into it), the orb moves, and gaming profiles clear the GPU.
pub fn load_spatial_scene(tools: &ToolContext, name: &str) -> Result<String, String> {
    let scene = find_scene(name).ok_or_else(|| {
        let known = list_scenes().into_iter().map(|(n, _, _)| n).collect::<Vec<_>>().join(", ");
        format!("I don't have a layout called \"{name}\", Sir. Saved: {known}.")
    })?;
    let monitors = detect_connected_monitors();
    if let Some(orb) = &scene.orb {
        let number = resolve(orb.monitor, &monitors).map(|(m, _)| m.number).unwrap_or(1);
        let args = json!({ "enabled": orb.orb_mode, "position": format!("{} of monitor {number}", orb.anchor) });
        let current = tools.config.read(|c| c.interface.clone());
        match crate::interface::control_orb_mode(&current, &args) {
            Ok(done) => {
                if done.settings != current {
                    tools.config.update(|c| c.interface = done.settings.clone());
                }
                crate::gui::show();
                crate::gui::refresh();
            }
            Err(e) => warn!("[SCENES] Orb placement: {e}"),
        }
    }
    let placements = place(&scene, &monitors);
    let count = placements.len();
    crate::workspace_canvas::apply_scene(placements, crate::deep_research_engine::latest_mission_id());
    if scene.flush_vram {
        crate::vram_guard::flush_with_reason(&format!("profile \"{}\"", scene.name));
    }
    info!("[SCENES] Loaded \"{}\" ({count} windows)", scene.name);
    Ok(format!(
        "{} engaged: {count} window(s){}{}.",
        scene.name,
        scene.orb.as_ref().map(|o| format!(", orb {}", o.anchor)).unwrap_or_default(),
        if scene.flush_vram { ", GPU cleared to 0 MB" } else { "" }
    ))
}

pub fn delete_scene(name: &str) -> Result<String, String> {
    let n = open()?.execute("DELETE FROM spatial_scenes WHERE name = ?1 AND builtin = 0", [name.trim()]).map_err(|e| e.to_string())?;
    if n == 0 {
        Err(format!("No saved (non-built-in) layout called \"{name}\"."))
    } else {
        Ok(format!("Layout \"{name}\" deleted."))
    }
}

pub fn is_tool(name: &str) -> bool {
    name == "manage_spatial_scene"
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": "manage_spatial_scene",
        "description": "Multi-monitor spatial mission profiles: save the current workspace window layout by name, load one (windows glide into place across all monitors), list or delete them, or list the monitors. Built in: 'Deep Research Matrix', 'Coding & Engineering Deck', 'Gaming Stealth HUD' (clears the GPU, click-through mini HUD), 'Morning Briefing Cockpit'.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["load", "save", "list", "delete", "monitors"]},
            "name": {"type": "string"}
        }, "required": ["action"]}
    }})]
}

pub fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let name = crate::features::str_arg(args, &["name", "scene", "profile", "layout"]).unwrap_or("");
    match crate::features::str_arg(args, &["action"]).unwrap_or("list").to_lowercase().as_str() {
        "load" | "recall" | "engage" | "open" | "apply" => load_spatial_scene(tools, name),
        "save" | "store" | "remember" => save_spatial_scene(tools, name),
        "delete" | "remove" | "forget" => delete_scene(name),
        "monitors" | "screens" | "displays" => Ok(detect_connected_monitors()
            .iter()
            .map(|m| {
                format!(
                    "Monitor {}{}: {}×{} at ({}, {}){}",
                    m.number,
                    if m.is_primary { " (primary)" } else { "" },
                    m.width,
                    m.height,
                    m.x,
                    m.y,
                    if m.is_portrait { ", portrait" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")),
        _ => Ok(list_scenes()
            .into_iter()
            .map(|(n, builtin, d)| format!("{n}{}: {d}", if builtin { " (built-in)" } else { "" }))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desk() -> Vec<MonitorInfo> {
        let m = |number, x, width, height, primary, device: &str| MonitorInfo {
            number,
            x,
            y: 0,
            width,
            height,
            work: [x, 0, width, height - 40],
            is_portrait: height > width,
            is_primary: primary,
            device: device.into(),
        };
        // Primary 1440p, secondary 1440p, left portrait 1080p.
        sort_and_number(vec![m(0, -1080, 1080, 1920, false, "\\\\.\\DISPLAY3"), m(0, 0, 2560, 1440, true, "\\\\.\\DISPLAY1"), m(0, 2560, 2560, 1440, false, "\\\\.\\DISPLAY2")])
    }

    #[test]
    fn roles_resolve_on_the_triple_monitor_desk() {
        let d = desk();
        assert_eq!(d[0].number, 1);
        assert!(d[0].is_primary);
        assert_eq!(resolve(MonitorRole::Secondary, &d).unwrap().0.x, 2560);
        assert_eq!(resolve(MonitorRole::Portrait, &d).unwrap().0.x, -1080);
        // A single screen: everything falls back to the primary.
        let single = vec![d[0].clone()];
        assert!(resolve(MonitorRole::Portrait, &single).unwrap().1);
    }

    #[test]
    fn scenes_persist_in_sqlite() {
        // The only test that sets the database path.
        let path = std::env::temp_dir().join(format!("jai_scenes_{}.db", uuid::Uuid::new_v4()));
        init(path.to_str().unwrap());
        let matrix = find_scene("deep research matrix").expect("built-in profile");
        assert_eq!(matrix.name, "Deep Research Matrix");
        assert_eq!(matrix.windows.len(), 4);
        assert!(matrix.orb.as_ref().is_some_and(|o| o.orb_mode && o.anchor == "center"));
        let stealth = find_scene("Gaming Stealth HUD").expect("built-in profile");
        assert!(stealth.flush_vram && stealth.windows.iter().all(|w| w.ghost && w.pinned));
        let mine = SpatialScene {
            name: "Late Night Coding".into(),
            description: "test".into(),
            windows: vec![w(SpatialWindowKind::CodeAndSandboxInspector, MonitorRole::Number(2), [0.1, 0.1, 0.5, 0.8])],
            orb: None,
            flush_vram: false,
        };
        store_scene(&mine).unwrap();
        assert_eq!(find_scene("late night coding"), Some(mine.clone()));
        assert!(list_scenes().iter().any(|(n, builtin, _)| n == "Late Night Coding" && !builtin));
        // A built-in can be neither overwritten nor deleted.
        store_scene(&SpatialScene { name: "Deep Research Matrix".into(), ..mine.clone() }).unwrap();
        assert_eq!(find_scene("Deep Research Matrix").unwrap().windows.len(), 4);
        assert!(delete_scene("Deep Research Matrix").is_err());
        assert!(delete_scene("Late Night Coding").is_ok());
        assert!(find_scene("Late Night Coding").is_none());
        // Re-initialising keeps the built-ins current, never duplicated.
        init(path.to_str().unwrap());
        assert_eq!(list_scenes().iter().filter(|(_, builtin, _)| *builtin).count(), 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn builtins_place_inside_their_monitors() {
        let d = desk();
        for scene in builtin_scenes() {
            let placed = place(&scene, &d);
            assert_eq!(placed.len(), scene.windows.len(), "{}", scene.name);
            for p in &placed {
                assert!(d.iter().any(|m| m.work_rect().expand(1.0).contains_rect(p.rect)), "{} {:?}", scene.name, p.rect);
            }
        }
        let coding = builtin_scenes().into_iter().find(|s| s.name.starts_with("Coding")).unwrap();
        assert!(place(&coding, &d).iter().all(|p| p.rect.left() < 0.0), "the coding deck goes to the portrait monitor");
        assert!(builtin_scenes().iter().any(|s| s.flush_vram && s.windows.iter().all(|w| w.ghost)));
    }
}
