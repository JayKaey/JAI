//! The window tiler: Sir's open program windows arranged on a monitor in one go ("tile my
//! windows side by side", "put my windows in a grid on monitor 2", "main and stack").
//!
//! The layouts are plain geometry on the monitor's work area (the taskbar stays free): columns,
//! rows, a grid, or one main window with the others stacked beside it. Only ordinary program
//! windows that are open on that monitor are moved — never a minimised one, never JAI's own
//! HUD — front-most first, at most nine. A maximised window is restored first so that it takes
//! its cell.

use crate::features::{spoken, str_arg};
use serde_json::{json, Value};

/// The most windows one layout takes (more would be slivers).
pub const MAX_WINDOWS: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Columns of equal width.
    SideBySide,
    /// Rows of equal height.
    Rows,
    /// As square a grid as the count allows; a short last row is stretched.
    Grid,
    /// The first window on the left 60 %, the others stacked on the right.
    MainAndStack,
}

impl Layout {
    pub fn label(self) -> &'static str {
        match self {
            Layout::SideBySide => "side by side",
            Layout::Rows => "in rows",
            Layout::Grid => "in a grid",
            Layout::MainAndStack => "as main and stack",
        }
    }
}

/// The layout a word or phrase means (None: no layout named; the caller picks by count).
pub fn parse_layout(text: &str) -> Option<Layout> {
    let t = text.to_lowercase();
    if ["main", "stack", "focus", "haupt"].iter().any(|w| t.contains(w)) {
        Some(Layout::MainAndStack)
    } else if ["grid", "raster", "kachel", "quarters", "2x2"].iter().any(|w| t.contains(w)) {
        Some(Layout::Grid)
    } else if ["row", "stacked", "on top of each other", "untereinander", "übereinander", "zeilen"].iter().any(|w| t.contains(w)) {
        Some(Layout::Rows)
    } else if ["side by side", "side_by_side", "column", "next to each other", "nebeneinander", "spalten", "split"].iter().any(|w| t.contains(w)) {
        Some(Layout::SideBySide)
    } else {
        None
    }
}

/// The layout for `n` windows when none was named: two or three side by side, more in a grid.
pub fn default_layout(n: usize) -> Layout {
    if n <= 3 { Layout::SideBySide } else { Layout::Grid }
}

/// The cells of `layout` for `n` windows on the work area `[left, top, right, bottom]`, each as
/// `[x, y, width, height]`, in the order the windows take them. The cells cover the work area
/// exactly: no gap, no overlap.
pub fn cells(layout: Layout, [left, top, right, bottom]: [i32; 4], n: usize) -> Vec<[i32; 4]> {
    let (w, h) = ((right - left).max(1), (bottom - top).max(1));
    // `count` equal spans of `total`, the last taking the rounding remainder.
    let spans = |start: i32, total: i32, count: usize| -> Vec<(i32, i32)> {
        (0..count)
            .map(|i| {
                let a = start + (total as i64 * i as i64 / count as i64) as i32;
                let b = start + (total as i64 * (i as i64 + 1) / count as i64) as i32;
                (a, b - a)
            })
            .collect()
    };
    match (layout, n) {
        (_, 0) => Vec::new(),
        (_, 1) => vec![[left, top, w, h]],
        (Layout::SideBySide, _) => spans(left, w, n).into_iter().map(|(x, cw)| [x, top, cw, h]).collect(),
        (Layout::Rows, _) => spans(top, h, n).into_iter().map(|(y, ch)| [left, y, w, ch]).collect(),
        (Layout::Grid, _) => {
            let cols = (n as f64).sqrt().ceil() as usize;
            let rows = n.div_ceil(cols);
            let mut out = Vec::with_capacity(n);
            for (row, (y, ch)) in spans(top, h, rows).into_iter().enumerate() {
                // The last row holds what is left and fills the whole width.
                let in_row = if row + 1 == rows { n - cols * (rows - 1) } else { cols };
                out.extend(spans(left, w, in_row).into_iter().map(|(x, cw)| [x, y, cw, ch]));
            }
            out
        }
        (Layout::MainAndStack, _) => {
            let main = w * 3 / 5;
            let mut out = vec![[left, top, main, h]];
            out.extend(spans(top, h, n - 1).into_iter().map(|(y, ch)| [left + main, y, w - main, ch]));
            out
        }
    }
}

/// A window as the tiler sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub hwnd: isize,
    pub title: String,
    pub process: String,
    /// Visible bounds `[x, y, width, height]`.
    pub rect: [i32; 4],
    pub maximised: bool,
    pub minimised: bool,
    /// One of JAI's own windows.
    pub own: bool,
}

/// The windows to tile on the monitor `[left, top, right, bottom]`: open there (by their
/// centre), not minimised, not JAI's own, front-most first, at most `MAX_WINDOWS`. With
/// `only`, just the programs whose name or title carries one of those words.
pub fn pick(windows: &[Candidate], monitor: [i32; 4], only: &[String]) -> Vec<Candidate> {
    let wanted = |c: &Candidate| only.is_empty() || only.iter().any(|w| !w.trim().is_empty() && (c.process.to_lowercase().contains(&w.to_lowercase()) || c.title.to_lowercase().contains(&w.to_lowercase())));
    windows
        .iter()
        .filter(|c| !c.minimised && !c.own && c.rect[2] > 0 && c.rect[3] > 0)
        .filter(|c| {
            let (cx, cy) = (c.rect[0] + c.rect[2] / 2, c.rect[1] + c.rect[3] / 2);
            // A maximised window hangs a few pixels over its monitor; its centre does not.
            cx >= monitor[0] && cx < monitor[2] && cy >= monitor[1] && cy < monitor[3]
        })
        .filter(|c| wanted(c))
        .take(MAX_WINDOWS)
        .cloned()
        .collect()
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "tile_windows",
        "description": "Arranges Sir's open program windows on one monitor: layout side_by_side, rows, grid or main_and_stack (default: side by side for up to three, a grid for more); monitor (1, 2, 'left', 'right', 'primary'; default: the monitor of the window in front); apps (only these programs). JAI's own HUD and minimised windows are left alone.",
        "parameters": {"type": "object", "properties": {
            "layout": {"type": "string", "enum": ["side_by_side", "rows", "grid", "main_and_stack"]},
            "monitor": {"type": "string"},
            "apps": {"type": "array", "items": {"type": "string"}}
        }, "required": []}
    }})
}

/// The `tile_windows` tool: moves the windows and says what it did.
#[cfg(windows)]
pub async fn run_tool(args: &Value) -> Result<String, String> {
    let monitors = crate::monitors::list();
    if monitors.is_empty() {
        return Err("I can't see a monitor to tile on, Sir.".to_string());
    }
    let front = crate::win32::user_window();
    let state = front.as_ref().and_then(|w| crate::win32::window_state(w.hwnd));
    let current = state.and_then(|(r, _, _)| crate::monitors::monitor_at(&monitors, r[0] + r[2] / 2, r[1] + r[3] / 2));
    let target = match args.get("monitor").filter(|m| !m.is_null()) {
        Some(named) => crate::monitors::resolve(named, current, &monitors).ok_or_else(|| format!("I can't find monitor {named}, Sir."))?,
        None => current.or_else(|| monitors.iter().position(|m| m.primary)).unwrap_or(0),
    };
    let monitor = monitors.get(target).ok_or("That monitor is gone, Sir.")?.clone();
    let only: Vec<String> = args["apps"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let own = std::process::id();
    let layout_arg = str_arg(args, &["layout"]).and_then(parse_layout);
    let (moved, layout) = crate::win32::call(move || {
        let windows: Vec<Candidate> = crate::win32::app_windows()
            .into_iter()
            .filter_map(|w| {
                let (rect, maximised, minimised) = crate::win32::window_state(w.hwnd)?;
                Some(Candidate { hwnd: w.hwnd, title: w.title, process: w.process, rect, maximised, minimised, own: w.pid == own })
            })
            .collect();
        let chosen = pick(&windows, monitor.rect, &only);
        let layout = layout_arg.unwrap_or_else(|| default_layout(chosen.len()));
        let mut moved: Vec<String> = Vec::new();
        for (window, cell) in chosen.iter().zip(cells(layout, monitor.work, chosen.len())) {
            if window.maximised {
                crate::win32::show_window(window.hwnd, crate::win32::WindowCmd::Restore);
            }
            if crate::win32::set_window_rect(window.hwnd, cell).is_ok() {
                moved.push(crate::screen_time::app_name(&window.process));
            }
        }
        (moved, layout)
    })
    .await?;
    if moved.is_empty() {
        return Ok(spoken(format!("No windows to tile on monitor {}.", monitor.index), format!("There's nothing open to tile on monitor {}, Sir.", monitor.index)));
    }
    Ok(spoken(format!("Tiled {} window(s) {} on monitor {}: {}", moved.len(), layout.label(), monitor.index, moved.join(", ")), ""))
}

#[cfg(not(windows))]
pub async fn run_tool(_args: &Value) -> Result<String, String> {
    Err("Tiling windows needs Windows, Sir.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: [i32; 4] = [0, 0, 2560, 1400];

    fn area(cells: &[[i32; 4]]) -> i64 {
        cells.iter().map(|c| c[2] as i64 * c[3] as i64).sum()
    }

    fn overlap(a: &[i32; 4], b: &[i32; 4]) -> bool {
        a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
    }

    #[test]
    fn every_layout_covers_the_work_area_exactly_without_overlap() {
        // Also a portrait monitor left of the main one (negative coordinates) and odd sizes.
        for work in [WORK, [-1080, -290, 0, 1590], [2560, 0, 5120, 1399], [0, 0, 1001, 777]] {
            let total = (work[2] - work[0]) as i64 * (work[3] - work[1]) as i64;
            for layout in [Layout::SideBySide, Layout::Rows, Layout::Grid, Layout::MainAndStack] {
                for n in 1..=MAX_WINDOWS {
                    let got = cells(layout, work, n);
                    assert_eq!(got.len(), n, "{layout:?} {n}");
                    assert_eq!(area(&got), total, "{layout:?} {n} on {work:?}: {got:?}");
                    for (i, a) in got.iter().enumerate() {
                        assert!(a[0] >= work[0] && a[1] >= work[1] && a[0] + a[2] <= work[2] && a[1] + a[3] <= work[3] && a[2] > 0 && a[3] > 0, "{layout:?} {n}: {a:?}");
                        assert!(got[i + 1..].iter().all(|b| !overlap(a, b)), "{layout:?} {n}: {got:?}");
                    }
                }
            }
            assert!(cells(Layout::Grid, work, 0).is_empty());
        }
        assert_eq!(cells(Layout::SideBySide, WORK, 2), vec![[0, 0, 1280, 1400], [1280, 0, 1280, 1400]]);
        assert_eq!(cells(Layout::MainAndStack, WORK, 3), vec![[0, 0, 1536, 1400], [1536, 0, 1024, 700], [1536, 700, 1024, 700]]);
        // Five in a grid: three on top, two below filling the width.
        assert_eq!(cells(Layout::Grid, WORK, 5)[3..], [[0, 700, 1280, 700], [1280, 700, 1280, 700]]);
    }

    #[test]
    fn layouts_are_understood_in_english_and_german() {
        assert_eq!(parse_layout("side by side"), Some(Layout::SideBySide));
        assert_eq!(parse_layout("nebeneinander"), Some(Layout::SideBySide));
        assert_eq!(parse_layout("in a grid"), Some(Layout::Grid));
        assert_eq!(parse_layout("im Raster"), Some(Layout::Grid));
        assert_eq!(parse_layout("in rows"), Some(Layout::Rows));
        assert_eq!(parse_layout("untereinander"), Some(Layout::Rows));
        assert_eq!(parse_layout("main_and_stack"), Some(Layout::MainAndStack));
        assert_eq!(parse_layout("somehow"), None);
        assert_eq!((default_layout(2), default_layout(3), default_layout(4)), (Layout::SideBySide, Layout::SideBySide, Layout::Grid));
    }

    #[test]
    fn only_open_program_windows_on_that_monitor_are_picked() {
        let window = |hwnd: isize, process: &str, rect: [i32; 4]| Candidate { hwnd, title: format!("{process} window"), process: process.to_string(), rect, maximised: false, minimised: false, own: false };
        let main = [0, 0, 2560, 1440];
        let windows = vec![
            window(1, "chrome.exe", [100, 100, 1200, 800]),
            Candidate { minimised: true, ..window(2, "Discord.exe", [200, 200, 900, 700]) },
            Candidate { own: true, ..window(3, "JARVIS.exe", [0, 0, 2560, 1440]) },
            window(4, "Code.exe", [2600, 50, 1500, 900]),
            // Maximised on the main monitor: it hangs 8 px over every edge.
            Candidate { maximised: true, ..window(5, "Spotify.exe", [-8, -8, 2576, 1456]) },
        ];
        let picked: Vec<isize> = pick(&windows, main, &[]).iter().map(|c| c.hwnd).collect();
        assert_eq!(picked, [1, 5], "not the minimised one, not JAI, not the one on the other monitor");
        assert_eq!(pick(&windows, [2560, 0, 5120, 1440], &[]).iter().map(|c| c.hwnd).collect::<Vec<_>>(), [4]);
        assert_eq!(pick(&windows, main, &["spotify".to_string()]).iter().map(|c| c.hwnd).collect::<Vec<_>>(), [5]);
        let many: Vec<Candidate> = (0..14).map(|n| window(n, "notepad.exe", [10, 10, 400, 300])).collect();
        assert_eq!(pick(&many, main, &[]).len(), MAX_WINDOWS);
    }
}
