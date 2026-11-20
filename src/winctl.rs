//! Window management: focus / minimise / maximise / restore / full screen / close / snap / show
//! desktop. Windows uses native Win32; Linux uses KDE Plasma's own window list and KWin's
//! shortcuts (`plasma`, `kde`), else Hyprland (`hyprctl`) or X11 (`wmctrl` + `xdotool`).
//!
//! JAI's own windows are never a target unless Sir names JAI: without a name an action goes
//! to the app the conversation is about or the window Sir was working in (see `focus`).

use crate::apps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Focus,
    Minimize,
    Maximize,
    Restore,
    /// True full screen: maximised, then the app's own full-screen hotkey (F11 for browsers).
    Fullscreen,
    ExitFullscreen,
    Close,
    /// Ctrl+W: the browser tab JAI just opened, not the whole window.
    CloseTab,
    SnapLeft,
    SnapRight,
    SnapTop,
    SnapBottom,
    /// "Make it bigger / smaller": the window grows or shrinks by a fifth around its centre.
    Bigger,
    Smaller,
    MinimizeAll,
    ShowDesktop,
}

impl WindowAction {
    pub fn parse(raw: &str) -> Option<Self> {
        let a = raw.trim().to_lowercase().replace([' ', '-'], "_");
        Some(match a.as_str() {
            "focus" | "activate" | "switch" | "switch_to" | "bring_to_front" | "fokus" => Self::Focus,
            "minimize" | "minimise" | "hide" | "minimieren" => Self::Minimize,
            "maximize" | "maximise" | "maximieren" => Self::Maximize,
            "fullscreen" | "full_screen" | "go_fullscreen" | "enter_fullscreen" | "vollbild" => Self::Fullscreen,
            "exit_fullscreen" | "leave_fullscreen" | "unfullscreen" | "exit_full_screen" | "end_fullscreen" | "vollbild_beenden" => {
                Self::ExitFullscreen
            }
            "restore" | "unminimize" | "wiederherstellen" => Self::Restore,
            "close" | "close_window" | "schliessen" | "schließen" => Self::Close,
            "close_tab" | "tab_schliessen" | "tab_schließen" => Self::CloseTab,
            "snap_left" | "left" | "links" => Self::SnapLeft,
            "snap_right" | "right" | "rechts" => Self::SnapRight,
            "snap_top" | "top" | "up" | "oben" => Self::SnapTop,
            "snap_bottom" | "bottom" | "down" | "unten" => Self::SnapBottom,
            "bigger" | "larger" | "grow" | "enlarge" | "make_bigger" | "größer" | "groesser" | "vergrößern" => Self::Bigger,
            "smaller" | "shrink" | "make_smaller" | "kleiner" | "verkleinern" => Self::Smaller,
            "minimize_all" | "minimise_all" | "minimize_all_windows" => Self::MinimizeAll,
            "show_desktop" | "desktop" | "toggle_desktop" => Self::ShowDesktop,
            _ => return None,
        })
    }

    /// Acts on the whole desktop, not on one window.
    pub fn is_global(self) -> bool {
        matches!(self, Self::MinimizeAll | Self::ShowDesktop)
    }

    pub fn past_tense(&self) -> &'static str {
        match self {
            Self::Focus => "focused",
            Self::Minimize => "minimised",
            Self::Maximize => "maximised",
            Self::Restore => "restored",
            Self::Fullscreen => "put in full screen",
            Self::ExitFullscreen => "took out of full screen",
            Self::Close => "closed",
            Self::CloseTab => "closed the tab in",
            Self::SnapLeft => "snapped to the left",
            Self::SnapRight => "snapped to the right",
            Self::SnapTop => "snapped to the top",
            Self::SnapBottom => "snapped to the bottom",
            Self::Bigger => "enlarged",
            Self::Smaller => "made smaller",
            Self::MinimizeAll => "minimised everything",
            Self::ShowDesktop => "toggled the desktop",
        }
    }
}

/// Which window an action is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowTarget {
    /// Sir named it ("minimise Spotify"): every matching window for minimise, restore and close.
    Named(String),
    /// The app the conversation is about ("full screen" right after "open Discord"): its top
    /// window only.
    Focus(String),
    /// A window captured earlier: the one Sir was working in right before he spoke or typed.
    /// `id` is the native handle (0 where there is none, e.g. Linux).
    Handle { id: isize, title: String, process: String },
    /// The window Sir is working in now (never one of JAI's own).
    Active,
}

impl WindowTarget {
    /// "Spotify", "the window 'YouTube - Google Chrome'", "the active window".
    pub fn describe(&self) -> String {
        match self {
            Self::Named(t) | Self::Focus(t) => apps::display_name(t),
            Self::Handle { title, .. } => format!("the window '{}'", short_title(title)),
            Self::Active => "the active window".into(),
        }
    }

    fn names_jai(&self) -> bool {
        matches!(self, Self::Named(t) | Self::Focus(t) if crate::tools::names_jai(t))
    }
}

fn short_title(title: &str) -> String {
    title.chars().take(60).collect()
}

fn matches_target(target: &str, process: &str, title: &str) -> bool {
    let t = apps::normalize(target);
    if t.is_empty() {
        return false;
    }
    let process = process.to_lowercase();
    if let Some(spec) = apps::find_spec(&t) {
        if spec.processes.iter().any(|p| process == *p) {
            return true;
        }
    }
    let compact = t.replace(' ', "");
    (compact.len() >= 3 && process.contains(&compact)) || title.to_lowercase().contains(&t)
}

/// The hotkey that toggles an app's own full screen: F11 for browsers, editors, file managers and
/// most apps; Discord uses Ctrl+Shift+F.
fn uses_ctrl_shift_f(process: &str) -> bool {
    matches!(process, "discord" | "discordcanary" | "discordptb")
}

#[cfg(windows)]
pub async fn manage(action: WindowAction, target: WindowTarget) -> Result<String, String> {
    use crate::win32::{self, Snap, WindowCmd};

    crate::win32::call(move || {
        match action {
            WindowAction::MinimizeAll => return win32::taskbar_command(419).map(|_| "Minimised all windows.".to_string()),
            WindowAction::ShowDesktop => return win32::taskbar_command(407).map(|_| "Toggled the desktop.".to_string()),
            _ => {}
        }
        let mut chosen = find_windows(&target)?;
        // A name Sir said covers all of its windows for these; everything else is the top one.
        let all = matches!(target, WindowTarget::Named(_)) && matches!(action, WindowAction::Minimize | WindowAction::Restore | WindowAction::Close);
        if !all {
            chosen.truncate(1);
        }
        let mut note = None;
        for w in &chosen {
            match action {
                WindowAction::Focus => win32::focus(w.hwnd),
                WindowAction::Minimize => win32::show_window(w.hwnd, WindowCmd::Minimize),
                WindowAction::Maximize => {
                    win32::show_window(w.hwnd, WindowCmd::Maximize);
                    win32::focus(w.hwnd);
                }
                WindowAction::Restore => win32::show_window(w.hwnd, WindowCmd::Restore),
                WindowAction::Fullscreen => note = Some(fullscreen(w)?),
                WindowAction::ExitFullscreen => note = Some(exit_fullscreen(w)?),
                WindowAction::Close => {
                    win32::close_window(w.hwnd);
                }
                WindowAction::CloseTab => {
                    bring_to_front(w)?;
                    win32::send_chord(&[win32::VK_CONTROL], win32::VK_W)?;
                }
                WindowAction::SnapLeft => win32::snap(w.hwnd, Snap::Left)?,
                WindowAction::SnapRight => win32::snap(w.hwnd, Snap::Right)?,
                WindowAction::SnapTop => win32::snap(w.hwnd, Snap::Top)?,
                WindowAction::SnapBottom => win32::snap(w.hwnd, Snap::Bottom)?,
                WindowAction::Bigger => win32::place(w.hwnd, win32::Placement::Scale(1.25))?,
                WindowAction::Smaller => win32::place(w.hwnd, win32::Placement::Scale(0.8))?,
                WindowAction::MinimizeAll | WindowAction::ShowDesktop => {}
            }
        }
        if let Some(note) = note {
            return Ok(note);
        }
        let title = short_title(&chosen[0].title);
        Ok(if chosen.len() > 1 {
            format!("{} {} windows (e.g. '{title}').", capitalize(action.past_tense()), chosen.len())
        } else {
            format!("{} '{title}'.", capitalize(action.past_tense()))
        })
    })
    .await?
}

/// The windows a target means, top of the Z-order first (JAI's own only when he is named).
/// Synchronous: call it on the Win32 worker thread.
#[cfg(windows)]
pub fn find_windows(target: &WindowTarget) -> Result<Vec<crate::win32::WindowInfo>, String> {
    use crate::win32;
    let allow_jai = target.names_jai();
    let candidates = || win32::app_windows().into_iter().filter(move |w| allow_jai || !win32::is_jai_window(w));
    let chosen: Vec<win32::WindowInfo> = match target {
        WindowTarget::Named(t) | WindowTarget::Focus(t) => candidates().filter(|w| matches_target(t, &w.process, &w.title)).collect(),
        WindowTarget::Handle { id, .. } if win32::is_window(*id) => candidates().filter(|w| w.hwnd == *id).collect(),
        WindowTarget::Handle { .. } => Vec::new(),
        WindowTarget::Active => win32::user_window().into_iter().collect(),
    };
    if chosen.is_empty() {
        return Err(none_open(target));
    }
    Ok(chosen)
}

/// What JAI says when no window is what `target` means.
fn none_open(target: &WindowTarget) -> String {
    match target {
        WindowTarget::Named(t) | WindowTarget::Focus(t) => format!("No open window matches '{}'.", apps::display_name(t)),
        WindowTarget::Handle { title, .. } => format!("The window '{}' is no longer open.", short_title(title)),
        WindowTarget::Active => "There is no window to act on (only JAI's own are open).".to_string(),
    }
}

/// Focuses a window and checks it really is in front before a key is sent to it, so a hotkey
/// can never land in another app (or in JAI's HUD).
#[cfg(windows)]
fn bring_to_front(w: &crate::win32::WindowInfo) -> Result<(), String> {
    use std::time::Duration;
    for wait in [120, 250] {
        crate::win32::focus(w.hwnd);
        std::thread::sleep(Duration::from_millis(wait));
        if crate::win32::is_foreground(w.hwnd) {
            return Ok(());
        }
    }
    Err(format!("Windows would not bring '{}' to the front, so I did not send it a key.", short_title(&w.title)))
}

#[cfg(windows)]
fn is_true_fullscreen(hwnd: isize) -> bool {
    let maximised = crate::win32::window_placement(hwnd).is_some_and(|(_, m)| m);
    crate::win32::covers_monitor(hwnd) && !maximised
}

#[cfg(windows)]
fn send_fullscreen_hotkey(w: &crate::win32::WindowInfo) -> Result<(), String> {
    use crate::win32::{send_chord, VK_CONTROL, VK_F, VK_F11, VK_SHIFT};
    if uses_ctrl_shift_f(&w.process) {
        send_chord(&[VK_CONTROL, VK_SHIFT], VK_F)
    } else {
        send_chord(&[], VK_F11)
    }
}

#[cfg(windows)]
fn fullscreen(w: &crate::win32::WindowInfo) -> Result<String, String> {
    use crate::win32::{self, WindowCmd};
    let title = short_title(&w.title);
    if is_true_fullscreen(w.hwnd) {
        win32::focus(w.hwnd);
        return Ok(format!("'{title}' is already in full screen."));
    }
    win32::show_window(w.hwnd, WindowCmd::Maximize);
    bring_to_front(w)?;
    send_fullscreen_hotkey(w)?;
    std::thread::sleep(std::time::Duration::from_millis(350));
    Ok(if win32::covers_monitor(w.hwnd) {
        format!("Put '{title}' in full screen.")
    } else {
        format!("Maximised '{title}' (it did not switch to full screen on its hotkey).")
    })
}

#[cfg(windows)]
fn exit_fullscreen(w: &crate::win32::WindowInfo) -> Result<String, String> {
    let title = short_title(&w.title);
    if !is_true_fullscreen(w.hwnd) {
        return Ok(format!("'{title}' is not in full screen."));
    }
    bring_to_front(w)?;
    send_fullscreen_hotkey(w)?;
    Ok(format!("Took '{title}' out of full screen."))
}

#[cfg(not(windows))]
pub async fn manage(action: WindowAction, target: WindowTarget) -> Result<String, String> {
    tokio::task::spawn_blocking(move || linux::manage(action, &target))
        .await
        .map_err(|e| e.to_string())?
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[cfg(not(windows))]
pub use linux::{kde_front, kde_windows, to_front};

#[cfg(not(windows))]
mod linux {
    use super::{capitalize, matches_target, none_open, short_title, uses_ctrl_shift_f, WindowAction, WindowTarget};
    use crate::apps::which;
    use crate::plasma::{self, state, Window};
    use std::process::Command;
    use std::time::Duration;

    /// The windows a target means under KDE Plasma, the topmost first (J.A.I.'s own only when he
    /// is named; panels and the desktop never).
    pub fn kde_windows(target: &WindowTarget) -> Result<Vec<Window>, String> {
        let me = std::process::id();
        let candidates = plasma::windows().into_iter().filter(|w| !w.is(state::SKIP_TASKBAR) && (w.pid != me || target.names_jai()));
        let chosen: Vec<Window> = match target {
            // (The window Sir works in: the active one, or while JAI's own has the focus the
            // topmost of the others.)
            WindowTarget::Active => candidates.filter(|w| !w.is(state::MINIMIZED)).min_by_key(|w| !w.is(state::ACTIVE)).into_iter().collect(),
            _ => candidates
                .filter(|w| {
                    let program = w.app_id.rsplit('.').next().unwrap_or_default().to_string();
                    [w.program(), program, w.resource_name.clone()].iter().any(|name| !name.is_empty() && picks(target, name, &w.title))
                })
                .collect(),
        };
        if chosen.is_empty() {
            return Err(none_open(target));
        }
        Ok(chosen)
    }

    /// The window Sir works in under KDE (never one of JAI's own), as `win32::user_window`.
    pub fn kde_front() -> Option<Window> {
        kde_windows(&WindowTarget::Active).ok()?.into_iter().next()
    }

    /// Brings a window to the front and checks it is there before a key is sent to it, so a key
    /// never lands in another program (or in JAI's own window).
    pub fn to_front(w: &Window) -> Result<(), String> {
        for wait in [120, 250] {
            if w.is(state::MINIMIZED) {
                plasma::set_state(&w.uuid, state::MINIMIZED, false)?;
            }
            plasma::set_state(&w.uuid, state::ACTIVE, true)?;
            std::thread::sleep(Duration::from_millis(wait));
            if plasma::active().is_some_and(|a| a.uuid == w.uuid) {
                return Ok(());
            }
        }
        Err(format!("KDE would not bring '{}' to the front, so I did not send it a key.", short_title(&w.title)))
    }

    /// Full screen on (`on`) or off: the program's own (a browser's has no tabs or bars), else
    /// KWin's.
    fn fullscreen(w: &Window, on: bool) -> Result<String, String> {
        let title = short_title(&w.title);
        if w.is(state::FULLSCREEN) == on {
            return Ok(if on { format!("'{title}' is already in full screen.") } else { format!("'{title}' is not in full screen.") });
        }
        to_front(w)?;
        plasma::press(if uses_ctrl_shift_f(&w.program().to_lowercase()) { "Ctrl+Shift+F" } else { "F11" })?;
        std::thread::sleep(Duration::from_millis(350));
        if plasma::windows().into_iter().find(|now| now.uuid == w.uuid).is_some_and(|now| now.is(state::FULLSCREEN) != on) {
            plasma::set_state(&w.uuid, state::FULLSCREEN, on)?;
        }
        Ok(if on { format!("Put '{title}' in full screen.") } else { format!("Took '{title}' out of full screen.") })
    }

    /// KDE Plasma: its own window list (Wayland's and X11's windows alike) and KWin's shortcuts.
    fn kde(action: WindowAction, target: &WindowTarget) -> Result<String, String> {
        match action {
            WindowAction::ShowDesktop => return crate::kde::kwin_shortcut("Show Desktop").map(|_| "Toggled the desktop.".to_string()),
            WindowAction::MinimizeAll => {
                for w in plasma::windows().iter().filter(|w| w.pid != std::process::id() && !w.is(state::SKIP_TASKBAR) && !w.is(state::MINIMIZED)) {
                    plasma::set_state(&w.uuid, state::MINIMIZED, true)?;
                }
                return Ok("Minimised all windows.".into());
            }
            _ => {}
        }
        let mut chosen = kde_windows(target)?;
        // A name Sir said covers all of its windows for these; everything else is the top one.
        if !(matches!(target, WindowTarget::Named(_)) && matches!(action, WindowAction::Minimize | WindowAction::Restore | WindowAction::Close)) {
            chosen.truncate(1);
        }
        let mut note = None;
        for w in &chosen {
            let set = |mask: u32, on: bool| plasma::set_state(&w.uuid, mask, on);
            match action {
                WindowAction::Focus => {
                    set(state::MINIMIZED, false)?;
                    set(state::ACTIVE, true)?;
                }
                WindowAction::Minimize => set(state::MINIMIZED, true)?,
                WindowAction::Maximize => {
                    set(state::MAXIMIZED, true)?;
                    set(state::ACTIVE, true)?;
                }
                WindowAction::Restore => {
                    set(if w.is(state::MINIMIZED) { state::MINIMIZED } else { state::MAXIMIZED }, false)?;
                    set(state::ACTIVE, true)?;
                }
                WindowAction::Fullscreen => note = Some(fullscreen(w, true)?),
                WindowAction::ExitFullscreen => note = Some(fullscreen(w, false)?),
                WindowAction::Close => plasma::close(&w.uuid)?,
                WindowAction::CloseTab => {
                    to_front(w)?;
                    plasma::press("Ctrl+W")?;
                }
                WindowAction::SnapLeft | WindowAction::SnapRight | WindowAction::SnapTop | WindowAction::SnapBottom => {
                    to_front(w)?;
                    let side = match action {
                        WindowAction::SnapLeft => "Left",
                        WindowAction::SnapRight => "Right",
                        WindowAction::SnapTop => "Top",
                        _ => "Bottom",
                    };
                    crate::kde::kwin_shortcut(&format!("Window Quick Tile {side}"))?;
                }
                WindowAction::Bigger | WindowAction::Smaller => {
                    to_front(w)?;
                    let how = if action == WindowAction::Bigger { "Grow" } else { "Shrink" };
                    for way in ["Horizontal", "Vertical"] {
                        crate::kde::kwin_shortcut(&format!("Window {how} {way}"))?;
                    }
                }
                WindowAction::MinimizeAll | WindowAction::ShowDesktop => {}
            }
        }
        if let Some(note) = note {
            return Ok(note);
        }
        let title = short_title(&chosen[0].title);
        Ok(if chosen.len() > 1 {
            format!("{} {} windows (e.g. '{title}').", capitalize(action.past_tense()), chosen.len())
        } else {
            format!("{} '{title}'.", capitalize(action.past_tense()))
        })
    }

    fn run(program: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    fn is_jai(class: &str, title: &str) -> bool {
        class.eq_ignore_ascii_case("jai") || title.contains("J.A.I.")
    }

    /// (process/class, title) a target has to match; `None` = the active window.
    fn wanted(target: &WindowTarget) -> Option<(String, String)> {
        match target {
            WindowTarget::Named(t) | WindowTarget::Focus(t) => Some((t.clone(), String::new())),
            WindowTarget::Handle { title, process, .. } => Some((process.clone(), title.clone())),
            WindowTarget::Active => None,
        }
    }

    fn picks(target: &WindowTarget, class: &str, title: &str) -> bool {
        if is_jai(class, title) && !target.names_jai() {
            return false;
        }
        match wanted(target) {
            Some((name, exact_title)) if !exact_title.is_empty() => title == exact_title || (!name.is_empty() && matches_target(&name, class, title)),
            Some((name, _)) => matches_target(&name, class, title),
            None => true,
        }
    }

    pub fn manage(action: WindowAction, target: &WindowTarget) -> Result<String, String> {
        if plasma::available() {
            return kde(action, target);
        }
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() && which("hyprctl").is_some() {
            return hyprland(action, target);
        }
        if std::env::var_os("DISPLAY").is_some() && which("wmctrl").is_some() {
            return x11(action, target);
        }
        Err("No supported window manager interface found. Install 'wmctrl' and 'xdotool' (X11) or run under Hyprland.".into())
    }

    fn hyprland(action: WindowAction, target: &WindowTarget) -> Result<String, String> {
        let dispatch = |args: &[&str]| run("hyprctl", &[&["dispatch"], args].concat()).map(|_| ());
        if action.is_global() {
            dispatch(&["togglespecialworkspace", "minimized"])?;
            return Ok("Toggled the minimised workspace.".into());
        }
        let clients: serde_json::Value = serde_json::from_str(&run("hyprctl", &["clients", "-j"])?).map_err(|e| e.to_string())?;
        let mut list: Vec<&serde_json::Value> = clients.as_array().map(|a| a.iter().collect()).unwrap_or_default();
        // Most recently focused first.
        list.sort_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
        let client = list
            .into_iter()
            .find(|c| picks(target, c["class"].as_str().unwrap_or(""), c["title"].as_str().unwrap_or("")))
            .ok_or_else(|| format!("No open window matches {}.", target.describe()))?;
        let address = client["address"].as_str().ok_or("The window has no address.")?;
        let is_fullscreen = client["fullscreen"].as_bool().unwrap_or(false) || client["fullscreen"].as_i64().unwrap_or(0) > 0;
        let sel = format!("address:{address}");
        match action {
            WindowAction::Focus | WindowAction::Restore => dispatch(&["focuswindow", &sel])?,
            WindowAction::Minimize => dispatch(&["movetoworkspacesilent", &format!("special:minimized,{sel}")])?,
            WindowAction::Maximize => {
                dispatch(&["focuswindow", &sel])?;
                dispatch(&["fullscreen", "1"])?;
            }
            WindowAction::Fullscreen | WindowAction::ExitFullscreen => {
                dispatch(&["focuswindow", &sel])?;
                if is_fullscreen == (action == WindowAction::ExitFullscreen) {
                    dispatch(&["fullscreen", "0"])?;
                }
            }
            WindowAction::Close => dispatch(&["closewindow", &sel])?,
            WindowAction::CloseTab => dispatch(&["sendshortcut", "CTRL,W", &sel])?,
            WindowAction::SnapLeft | WindowAction::SnapRight | WindowAction::SnapTop | WindowAction::SnapBottom => {
                dispatch(&["focuswindow", &sel])?;
                let dir = match action {
                    WindowAction::SnapLeft => "l",
                    WindowAction::SnapRight => "r",
                    WindowAction::SnapTop => "u",
                    _ => "d",
                };
                dispatch(&["movewindow", dir])?;
            }
            WindowAction::Bigger | WindowAction::Smaller => {
                dispatch(&["focuswindow", &sel])?;
                dispatch(&["resizeactive", if action == WindowAction::Bigger { "120 80" } else { "-120 -80" }])?;
            }
            WindowAction::MinimizeAll | WindowAction::ShowDesktop => {}
        }
        Ok(format!("{} '{}'.", capitalize(action.past_tense()), client["title"].as_str().unwrap_or("the window")))
    }

    fn x11(action: WindowAction, target: &WindowTarget) -> Result<String, String> {
        if action.is_global() {
            run("wmctrl", &["-k", "on"])?;
            return Ok("Showing the desktop.".into());
        }
        // `wmctrl -lx`: <id> <desktop> <wm_class> <host> <title...>
        let listing = run("wmctrl", &["-lx"])?;
        let windows: Vec<(String, String, String)> = listing
            .lines()
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                let id = parts.next()?.to_string();
                let _desktop = parts.next()?;
                let class = parts.next()?.split('.').next().unwrap_or("").to_string();
                let _host = parts.next()?;
                Some((id, class, parts.collect::<Vec<_>>().join(" ")))
            })
            .collect();
        let (id, _, title) = match target {
            WindowTarget::Active => {
                let active = run("xdotool", &["getactivewindow"])?;
                let id = format!("0x{:08x}", active.trim().parse::<u64>().map_err(|e| e.to_string())?);
                let found = windows.iter().find(|(w, _, _)| u64::from_str_radix(w.trim_start_matches("0x"), 16).ok() == u64::from_str_radix(id.trim_start_matches("0x"), 16).ok());
                match found {
                    Some((_, class, title)) if is_jai(class, title) => return Err("Only JAI's own window is active.".into()),
                    Some((_, class, title)) => (id, class.clone(), title.clone()),
                    None => (id, String::new(), String::new()),
                }
            }
            _ => windows
                .iter()
                .rev()
                .find(|(_, class, title)| picks(target, class, title))
                .cloned()
                .ok_or_else(|| format!("No open window matches {}.", target.describe()))?,
        };
        let geometry = || -> Result<(i64, i64), String> {
            let g = run("xdotool", &["getdisplaygeometry"])?;
            let mut it = g.split_whitespace().filter_map(|v| v.parse::<i64>().ok());
            Ok((it.next().unwrap_or(1920), it.next().unwrap_or(1080)))
        };
        match action {
            WindowAction::Focus => {
                run("wmctrl", &["-i", "-a", &id])?;
            }
            WindowAction::Minimize => {
                run("xdotool", &["windowminimize", &id])?;
            }
            WindowAction::Maximize => {
                run("wmctrl", &["-i", "-r", &id, "-b", "add,maximized_vert,maximized_horz"])?;
                run("wmctrl", &["-i", "-a", &id])?;
            }
            WindowAction::Fullscreen => {
                run("wmctrl", &["-i", "-r", &id, "-b", "add,fullscreen"])?;
                run("wmctrl", &["-i", "-a", &id])?;
            }
            WindowAction::ExitFullscreen => {
                run("wmctrl", &["-i", "-r", &id, "-b", "remove,fullscreen"])?;
            }
            WindowAction::Restore => {
                run("wmctrl", &["-i", "-r", &id, "-b", "remove,maximized_vert,maximized_horz"])?;
                run("wmctrl", &["-i", "-a", &id])?;
            }
            WindowAction::Close => {
                run("wmctrl", &["-i", "-c", &id])?;
            }
            WindowAction::CloseTab => {
                run("xdotool", &["windowactivate", "--sync", &id, "key", "ctrl+w"])?;
            }
            WindowAction::SnapLeft | WindowAction::SnapRight | WindowAction::SnapTop | WindowAction::SnapBottom => {
                let (w, h) = geometry()?;
                let rect = match action {
                    WindowAction::SnapLeft => (0, 0, w / 2, h),
                    WindowAction::SnapRight => (w / 2, 0, w - w / 2, h),
                    WindowAction::SnapTop => (0, 0, w, h / 2),
                    _ => (0, h / 2, w, h - h / 2),
                };
                run("wmctrl", &["-i", "-r", &id, "-b", "remove,maximized_vert,maximized_horz"])?;
                run("wmctrl", &["-i", "-r", &id, "-e", &format!("0,{},{},{},{}", rect.0, rect.1, rect.2, rect.3)])?;
                run("wmctrl", &["-i", "-a", &id])?;
            }
            WindowAction::Bigger | WindowAction::Smaller => {
                let g = run("xdotool", &["getwindowgeometry", "--shell", &id])?;
                let get = |k: &str| g.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
                let f = if action == WindowAction::Bigger { 1.25 } else { 0.8 };
                let (w, h) = ((get("WIDTH=") as f64 * f) as i64, (get("HEIGHT=") as f64 * f) as i64);
                run("xdotool", &["windowsize", &id, &w.to_string(), &h.to_string()])?;
            }
            WindowAction::MinimizeAll | WindowAction::ShowDesktop => {}
        }
        Ok(format!("{} '{}'.", capitalize(action.past_tense()), if title.is_empty() { "the window" } else { &title }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_action_synonyms() {
        assert_eq!(WindowAction::parse("Snap Left"), Some(WindowAction::SnapLeft));
        assert_eq!(WindowAction::parse("minimise"), Some(WindowAction::Minimize));
        assert_eq!(WindowAction::parse("show-desktop"), Some(WindowAction::ShowDesktop));
        assert_eq!(WindowAction::parse("fullscreen"), Some(WindowAction::Fullscreen), "full screen is not just maximise");
        assert_eq!(WindowAction::parse("maximize"), Some(WindowAction::Maximize));
        assert_eq!(WindowAction::parse("exit fullscreen"), Some(WindowAction::ExitFullscreen));
        assert_eq!(WindowAction::parse("close_tab"), Some(WindowAction::CloseTab));
        assert_eq!(WindowAction::parse("explode"), None);
    }

    #[test]
    fn target_matching_uses_process_aliases_and_titles() {
        assert!(matches_target("vs code", "code", "main.rs - jai - Visual Studio Code"));
        assert!(matches_target("spotify", "spotify", "Spotify Premium"));
        assert!(matches_target("browser", "chrome", "YouTube - Google Chrome"), "the browser alias covers every browser");
        assert!(matches_target("jai", "msedge", "J.A.I. – jai HUD"));
        assert!(!matches_target("discord", "chrome", "YouTube"));
        assert!(uses_ctrl_shift_f("discord") && !uses_ctrl_shift_f("chrome"));
    }

    #[test]
    fn jai_is_only_a_target_when_named() {
        assert!(WindowTarget::Named("yourself".into()).names_jai());
        assert!(WindowTarget::Focus("JAI".into()).names_jai());
        assert!(!WindowTarget::Focus("browser".into()).names_jai());
        assert!(!WindowTarget::Active.names_jai());
    }
}
