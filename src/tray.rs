//! Notification-area (tray) icon: shows that JAI is running in the background and offers
//! Open HUD, a microphone toggle and Quit. On Windows it runs its own Win32 message loop on a
//! dedicated thread, which also receives the "show your HUD" request of a second `JAI.exe`; on
//! Linux it is a StatusNotifierItem with its menu on the session bus (KDE Plasma's tray), where
//! J.A.I.'s own bus name takes that request.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenHud,
    ToggleMic,
    /// Leave the zero-VRAM gaming standby (models load again).
    WakeJai,
    /// Enter it: the models leave the graphics memory.
    Standby,
    /// Obey only Sir's voice, on or off.
    VoiceLock(bool),
    /// Guests may speak for half an hour, or no longer.
    Guests(bool),
    /// Night mode now, or day.
    Night(bool),
    /// Game mode on (the model to RAM) or off.
    GameMode(bool),
    Quit,
}

/// What the tray menu ticks, read when the menu opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrayState {
    pub mic_on: bool,
    pub voice_lock: bool,
    pub guests: bool,
    pub night: bool,
    pub dormant: bool,
    pub game: bool,
    pub thinking: bool,
}

#[cfg(windows)]
pub use win::{ask_test_session_to_give_way, remove, show_running_instance, spawn};

#[cfg(not(windows))]
pub use sni::{ask_test_session_to_give_way, own_bus_name, release_bus_name, remove, show_running_instance, spawn, test_session_running, INSTANCE, TEST_SESSION};

const CMD_OPEN: usize = 1;
const CMD_MIC: usize = 2;
const CMD_QUIT: usize = 3;
const CMD_WAKE: usize = 4;
const CMD_LOCK: usize = 5;
const CMD_GUESTS: usize = 6;
const CMD_NIGHT: usize = 7;
const CMD_STANDBY: usize = 8;
const CMD_GAME: usize = 9;

/// When he started ("Fri 06:40"), for the menu's first line.
static SINCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The menu's lines from the top: (command, text, its tick, enabled). An empty text is a
/// separator; command 0 does nothing.
fn menu_lines(now: TrayState) -> Vec<(usize, String, Option<bool>, bool)> {
    // The first line says who is running and since when (greyed: it is no command).
    let since = SINCE.get().map_or("a moment ago", String::as_str);
    let line = |command: usize, text: &str, tick: Option<bool>| (command, text.to_string(), tick, true);
    let mut lines = vec![
        (0, format!("J.A.I. {} \u{2014} running since {since}", env!("CARGO_PKG_VERSION")), None, false),
        line(CMD_OPEN, "Open HUD", None),
        line(0, "", None),
        line(CMD_MIC, "Microphone on", Some(now.mic_on)),
        line(CMD_LOCK, "Obey only my voice", Some(now.voice_lock)),
        line(CMD_GUESTS, "Guests may speak (30 minutes)", Some(now.guests)),
        line(CMD_NIGHT, "Night mode", Some(now.night)),
        line(0, "", None),
        line(CMD_GAME, "Game mode (brain in RAM, GPU free)", Some(now.game)),
    ];
    // One line for the standby, saying what a click does from where he stands. (In game mode
    // the GPU is free already: no standby line.)
    if !now.game {
        lines.push(if now.dormant { line(CMD_WAKE, "Wake JAI (load models)", None) } else { line(CMD_STANDBY, "Standby (free the graphics memory)", None) });
    }
    lines.push(line(0, "", None));
    lines.push(line(CMD_QUIT, "Quit J.A.I.", None));
    lines
}

/// What picking the menu's line `command` asks for.
fn picked(command: usize, now: TrayState) -> Option<TrayAction> {
    Some(match command {
        CMD_OPEN => TrayAction::OpenHud,
        CMD_MIC => TrayAction::ToggleMic,
        CMD_WAKE => TrayAction::WakeJai,
        CMD_STANDBY => TrayAction::Standby,
        CMD_LOCK => TrayAction::VoiceLock(!now.voice_lock),
        CMD_GUESTS => TrayAction::Guests(!now.guests),
        CMD_NIGHT => TrayAction::Night(!now.night),
        CMD_GAME => TrayAction::GameMode(!now.game),
        CMD_QUIT => TrayAction::Quit,
        _ => return None,
    })
}

/// The colour of the dot the icon shows (0xRRGGBB): violet in game mode, grey in standby or
/// with the microphone off, amber while he thinks, green while he listens.
fn dot_colour(s: TrayState) -> u32 {
    if s.game {
        0x9B6BFF
    } else if s.dormant || !s.mic_on {
        0x8A96A3
    } else if s.thinking {
        0xFFA530
    } else {
        0x3EE07A
    }
}

/// Pixel `i` (row by row) of a `size`-square icon that wears a dot of `rgb` in its lower right
/// corner: the dot's colour or its dark rim (0xAARRGGBB), None off the dot.
fn dot_pixel(size: i32, i: usize, rgb: u32) -> Option<u32> {
    let r = size as f32 * 0.22;
    let c = size as f32 - r - 0.5;
    let (x, y) = ((i as i32 % size) as f32 + 0.5, (i as i32 / size) as f32 + 0.5);
    let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
    if d <= r {
        Some(0xFF00_0000 | rgb)
    } else if d <= r + 1.2 {
        Some(0xFF10_1418)
    } else {
        None
    }
}

#[cfg(windows)]
mod win {
    use super::{picked, TrayAction, TrayState, SINCE};
    use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
    use std::sync::OnceLock;
    use tokio::sync::mpsc::UnboundedSender;
    use tracing::{info, warn};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadIconW, LoadImageW, PostMessageW,
        PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, TrackPopupMenu,
        TranslateMessage, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
        MF_UNCHECKED, MSG, SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
        WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP,
        WNDCLASSW, WS_OVERLAPPED,
    };

    const WM_TRAY: u32 = WM_APP + 1;
    const CLASS: PCWSTR = w!("JaiTrayWindow");
    /// Posted by a second `JAI.exe` so the running instance shows its HUD.
    const SHOW_HUD_MESSAGE: PCWSTR = w!("JAI_SHOW_HUD");
    /// Posted by the `JAI.exe` Sir started while a test session holds the place: it ends.
    const GIVE_WAY_MESSAGE: PCWSTR = w!("JAI_GIVE_WAY");
    const ICON_ID: u32 = 1;

    struct Tray {
        actions: UnboundedSender<TrayAction>,
        state: Box<dyn Fn() -> TrayState + Send + Sync>,
    }

    static TRAY: OnceLock<Tray> = OnceLock::new();
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static SHOW_HUD: AtomicU32 = AtomicU32::new(0);
    static GIVE_WAY: AtomicU32 = AtomicU32::new(0);

    pub fn ask_test_session_to_give_way() -> bool {
        // SAFETY: window lookup by class name and a posted message; no pointers are kept.
        unsafe {
            let hwnd = FindWindowW(CLASS, PCWSTR::null());
            hwnd.0 != 0 && PostMessageW(hwnd, RegisterWindowMessageW(GIVE_WAY_MESSAGE), WPARAM(0), LPARAM(0)).is_ok()
        }
    }

    pub fn show_running_instance() -> bool {
        // SAFETY: window lookup by class name and a posted message; no pointers are kept.
        unsafe {
            let hwnd = FindWindowW(CLASS, PCWSTR::null());
            if hwnd.0 == 0 {
                return false;
            }
            // We were just started by Sir, so we may hand the foreground to the running instance.
            let _ = AllowSetForegroundWindow(u32::MAX); // ASFW_ANY
            PostMessageW(hwnd, RegisterWindowMessageW(SHOW_HUD_MESSAGE), WPARAM(0), LPARAM(0)).is_ok()
        }
    }

    pub fn spawn(actions: UnboundedSender<TrayAction>, state: impl Fn() -> TrayState + Send + Sync + 'static) {
        if TRAY.set(Tray { actions, state: Box::new(state) }).is_err() {
            return;
        }
        let _ = SINCE.set(chrono::Local::now().format("%a %H:%M").to_string());
        if let Err(e) = std::thread::Builder::new().name("jai-tray".into()).spawn(run) {
            warn!("[TRAY] Could not start the tray icon: {e}");
        }
    }

    /// Removes the icon; without this a ghost icon stays in the tray until hovered.
    pub fn remove() {
        let hwnd = WINDOW.swap(0, Ordering::AcqRel);
        if hwnd != 0 {
            let data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: HWND(hwnd),
                uID: ICON_ID,
                ..Default::default()
            };
            // SAFETY: identifies our own icon by window handle + id.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
        }
    }

    fn send(action: TrayAction) {
        if let Some(tray) = TRAY.get() {
            let _ = tray.actions.send(action);
        }
    }

    fn run() {
        // SAFETY: window-class registration, a hidden window and its message loop, all on this thread.
        unsafe {
            let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
            let instance = HINSTANCE(module.0);
            let class = CLASS;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                warn!("[TRAY] Could not register the tray window class");
                return;
            }
            // A hidden top-level window (not message-only) so it receives "TaskbarCreated".
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("J.A.I."),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                HWND(0),
                HMENU(0),
                instance,
                None,
            );
            if hwnd.0 == 0 {
                warn!("[TRAY] Could not create the tray window");
                return;
            }
            WINDOW.store(hwnd.0, Ordering::Release);
            TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Release);
            SHOW_HUD.store(RegisterWindowMessageW(SHOW_HUD_MESSAGE), Ordering::Release);
            GIVE_WAY.store(RegisterWindowMessageW(GIVE_WAY_MESSAGE), Ordering::Release);
            if add_icon(hwnd) {
                info!("[TRAY] Running in the notification area (click: HUD, right-click: menu)");
            } else {
                warn!("[TRAY] Could not add the tray icon");
            }
            // (The icon's dot follows his state: `refresh_icon`.)
            windows::Win32::UI::WindowsAndMessaging::SetTimer(hwnd, 1, 1000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe fn add_icon(hwnd: HWND) -> bool {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: load_icon(),
            ..Default::default()
        };
        let tip = if crate::platform::in_test_session() { "J.A.I. — test run (starting him ends it)" } else { "J.A.I. — online" };
        let tip: Vec<u16> = tip.encode_utf16().take(data.szTip.len() - 1).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &data).as_bool()
    }

    /// Icon resource 1 (assets/jai_icon.ico, embedded by build.rs) at the small-icon size.
    unsafe fn load_icon() -> HICON {
        if let Ok(module) = GetModuleHandleW(PCWSTR::null()) {
            let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            // MAKEINTRESOURCE(1): an integer id in a pointer, never dereferenced.
            let resource = PCWSTR(std::ptr::without_provenance(1));
            if let Ok(handle) = LoadImageW(HINSTANCE(module.0), resource, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) {
                if !handle.is_invalid() {
                    return HICON(handle.0);
                }
            }
        }
        LoadIconW(HINSTANCE(0), IDI_APPLICATION).unwrap_or_default()
    }

    /// The colour of the dot the icon shows (0xRRGGBB, 0: none yet).
    static DOT: AtomicU32 = AtomicU32::new(0);
    static DOTTED: AtomicIsize = AtomicIsize::new(0);

    /// Once a second: a new icon when his state's colour has changed.
    unsafe fn refresh_icon(hwnd: HWND) {
        let Some(tray) = TRAY.get() else { return };
        let colour = super::dot_colour((tray.state)());
        if DOT.load(Ordering::Acquire) == colour {
            return;
        }
        let Some(icon) = dotted_icon(colour) else { return };
        let data = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, uFlags: NIF_ICON, hIcon: icon, ..Default::default() };
        // (Kept only once Windows took it: a refused icon is tried again next second.)
        if !Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_MODIFY, &data).as_bool() {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon);
            return;
        }
        DOT.store(colour, Ordering::Release);
        let old = DOTTED.swap(icon.0, Ordering::AcqRel);
        if old != 0 {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(HICON(old));
        }
    }

    /// The app icon with a dot of `rgb` in its lower right corner (a dark rim keeps it apart).
    unsafe fn dotted_icon(rgb: u32) -> Option<HICON> {
        use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
        use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, GetIconInfo, ICONINFO};
        let base = load_icon();
        let mut info = ICONINFO::default();
        let got = GetIconInfo(base, &mut info);
        let _ = DestroyIcon(base);
        got.ok()?;
        let size = GetSystemMetrics(SM_CXSMICON);
        let header = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() };
        let mut bmi = BITMAPINFO { bmiHeader: header, ..Default::default() };
        let mut px = vec![0u32; (size * size) as usize];
        let dc = GetDC(HWND(0));
        let read = GetDIBits(dc, info.hbmColor, 0, size as u32, Some(px.as_mut_ptr().cast()), &mut bmi, DIB_RGB_COLORS);
        ReleaseDC(HWND(0), dc);
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
        if read != size {
            return None;
        }
        for (i, p) in px.iter_mut().enumerate() {
            if let Some(dot) = super::dot_pixel(size, i, rgb) {
                *p = dot;
            }
        }
        let colour = CreateBitmap(size, size, 1, 32, Some(px.as_ptr().cast()));
        let mask = CreateBitmap(size, size, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO { fIcon: true.into(), hbmMask: mask, hbmColor: colour, ..Default::default() });
        DeleteObject(colour);
        DeleteObject(mask);
        icon.ok()
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let now = TRAY.get().map(|t| (t.state)()).unwrap_or(TrayState { mic_on: true, ..TrayState::default() });
        for (command, text, tick, enabled) in super::menu_lines(now) {
            if text.is_empty() {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                continue;
            }
            let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let flags = match tick {
                Some(true) => MF_STRING | MF_CHECKED,
                Some(false) => MF_STRING | MF_UNCHECKED,
                None => MF_STRING,
            };
            let _ = AppendMenuW(menu, if enabled { flags } else { flags | MF_GRAYED }, command, PCWSTR(text.as_ptr()));
        }
        let _ = SetMenuDefaultItem(menu, super::CMD_OPEN as u32, 0);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if let Some(action) = picked(chosen.0 as usize, now) {
            send(action);
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => send(TrayAction::OpenHud),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    // Windows is logging off / shutting down: save state before the process ends.
                    send(TrayAction::Quit);
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                remove();
                PostQuitMessage(0);
                LRESULT(0)
            }
            windows::Win32::UI::WindowsAndMessaging::WM_TIMER => {
                refresh_icon(hwnd);
                LRESULT(0)
            }
            _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Acquire) => {
                // Explorer restarted: the icon has to be added again (and its dot).
                add_icon(hwnd);
                DOT.store(0, Ordering::Release);
                LRESULT(0)
            }
            _ if msg != 0 && msg == SHOW_HUD.load(Ordering::Acquire) => {
                send(TrayAction::OpenHud);
                LRESULT(0)
            }
            _ if msg != 0 && msg == GIVE_WAY.load(Ordering::Acquire) => {
                info!("[TRAY] Sir started JAI: this test session gives way");
                send(TrayAction::Quit);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// KDE Plasma's tray: a StatusNotifierItem and its menu (DBusMenu) on the session bus. The same
/// bus carries J.A.I.'s own name: held by the J.A.I. that runs (a second one calls it to have the
/// HUD shown), and a scripted session's, which Sir's own J.A.I. asks to give way.
#[cfg(not(windows))]
mod sni {
    use super::{dot_colour, dot_pixel, menu_lines, picked, TrayAction, TrayState, SINCE};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;
    use tokio::sync::mpsc::UnboundedSender;
    use tracing::{info, warn};
    use zbus::zvariant::{ObjectPath, OwnedValue, StructureBuilder, Value};

    /// The name the J.A.I. that runs holds, and the one a scripted session holds besides.
    pub const INSTANCE: &str = "org.jai.JAI";
    pub const TEST_SESSION: &str = "org.jai.TestSession";
    const PATH: &str = "/org/jai/JAI";
    const ITEM: &str = "/StatusNotifierItem";
    const MENU: &str = "/MenuBar";
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    const BUS: &str = "org.freedesktop.DBus";

    struct Tray {
        actions: UnboundedSender<TrayAction>,
        state: Box<dyn Fn() -> TrayState + Send + Sync>,
    }

    static TRAY: OnceLock<Tray> = OnceLock::new();
    /// The colour of the icon's dot (0: none yet), and the menu's revision.
    static DOT: AtomicU32 = AtomicU32::new(0);
    static REVISION: AtomicU32 = AtomicU32::new(1);

    fn send(action: TrayAction) {
        if let Some(tray) = TRAY.get() {
            let _ = tray.actions.send(action);
        }
    }

    fn now() -> TrayState {
        TRAY.get().map(|t| (t.state)()).unwrap_or(TrayState { mic_on: true, ..TrayState::default() })
    }

    /// What other J.A.I.s may ask of this one.
    struct Instance;

    #[zbus::interface(name = "org.jai.JAI")]
    impl Instance {
        /// A second J.A.I. was started: the HUD comes up.
        fn show(&self) {
            send(TrayAction::OpenHud);
        }

        /// The setup updates J.A.I.: he quits, so his program can be replaced.
        fn quit(&self) {
            info!("[TRAY] The setup updates JAI: he quits");
            send(TrayAction::Quit);
        }

        /// Sir started J.A.I. while this scripted session runs: it ends.
        fn give_way(&self) {
            if crate::platform::in_test_session() {
                info!("[TRAY] Sir started JAI: this test session gives way");
                send(TrayAction::Quit);
            }
        }
    }

    /// Takes `name` on the session bus, with the object other J.A.I.s call. Some(false): another
    /// process holds it; None: there is no bus to ask.
    pub fn own_bus_name(name: &str) -> Option<bool> {
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        let bus = crate::kde::bus().ok()?;
        let _ = bus.object_server().at(PATH, Instance);
        match bus.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into()) {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Some(true),
            Ok(_) => Some(false),
            Err(e) => {
                warn!("[TRAY] Could not take the bus name {name}: {e}");
                None
            }
        }
    }

    pub fn release_bus_name(name: &str) {
        if let Ok(bus) = crate::kde::bus() {
            let _ = bus.release_name(name);
        }
    }

    pub fn show_running_instance() -> bool {
        crate::kde::call::<_, ()>(INSTANCE, PATH, INSTANCE, "Show", &()).is_ok()
    }

    pub fn ask_test_session_to_give_way() -> bool {
        crate::kde::call::<_, ()>(TEST_SESSION, PATH, INSTANCE, "GiveWay", &()).is_ok()
    }

    pub fn test_session_running() -> bool {
        crate::kde::call::<_, bool>(BUS, "/org/freedesktop/DBus", BUS, "NameHasOwner", &(TEST_SESSION,)).unwrap_or(false)
    }

    pub fn spawn(actions: UnboundedSender<TrayAction>, state: impl Fn() -> TrayState + Send + Sync + 'static) {
        if TRAY.set(Tray { actions, state: Box::new(state) }).is_err() {
            return;
        }
        let _ = SINCE.set(chrono::Local::now().format("%a %H:%M").to_string());
        if let Err(e) = std::thread::Builder::new().name("jai-tray".into()).spawn(run) {
            warn!("[TRAY] Could not start the tray icon: {e}");
        }
    }

    /// The icon leaves the tray (the item's name is given up).
    pub fn remove() {
        release_bus_name(&item_name());
    }

    fn item_name() -> String {
        format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
    }

    fn run() {
        let Ok(bus) = crate::kde::bus() else {
            warn!("[TRAY] No session bus: no tray icon");
            return;
        };
        let name = item_name();
        let served = bus.object_server().at(ITEM, Item).and_then(|_| bus.object_server().at(MENU, Menu)).and_then(|_| bus.request_name(name.as_str()));
        if let Err(e) = served {
            warn!("[TRAY] Could not offer the tray icon: {e}");
            return;
        }
        let (mut watcher, mut shown) = (String::new(), None);
        loop {
            // The tray's host (Plasma's panel) may start again: each new one is told of the icon.
            let owner = crate::kde::call::<_, String>(BUS, "/org/freedesktop/DBus", BUS, "GetNameOwner", &(WATCHER,)).unwrap_or_default();
            if owner != watcher {
                match crate::kde::call::<_, ()>(WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierItem", &(name.as_str(),)) {
                    Ok(()) => info!("[TRAY] Running in the notification area (click: HUD, right-click: menu)"),
                    Err(e) if !owner.is_empty() => warn!("[TRAY] The tray did not take the icon: {e}"),
                    Err(_) => {}
                }
                watcher = owner;
            }
            // (The icon's dot and the menu's ticks follow his state.)
            let state = now();
            if shown != Some(state) {
                shown = Some(state);
                DOT.store(dot_colour(state), Ordering::Release);
                REVISION.fetch_add(1, Ordering::AcqRel);
                let _ = bus.emit_signal(None::<&str>, ITEM, "org.kde.StatusNotifierItem", "NewIcon", &());
                let _ = bus.emit_signal(None::<&str>, MENU, "com.canonical.dbusmenu", "LayoutUpdated", &(REVISION.load(Ordering::Acquire), 0i32));
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// The app icon at a few sizes, with its dot, as the tray takes it: ARGB in network order.
    fn icon(rgb: u32) -> Vec<(i32, i32, Vec<u8>)> {
        static BASE: OnceLock<Vec<(i32, Vec<u8>)>> = OnceLock::new();
        let base = BASE.get_or_init(|| [22, 32, 64].into_iter().map(|size| (size, crate::app_icon::render_icon_rgba(size as u32))).collect());
        base.iter()
            .map(|(size, rgba)| {
                let argb = rgba
                    .chunks_exact(4)
                    .enumerate()
                    .flat_map(|(i, p)| dot_pixel(*size, i, rgb).filter(|_| rgb != 0).unwrap_or(u32::from_be_bytes([p[3], p[0], p[1], p[2]])).to_be_bytes())
                    .collect();
                (*size, *size, argb)
            })
            .collect()
    }

    struct Item;

    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl Item {
        fn activate(&self, _x: i32, _y: i32) {
            send(TrayAction::OpenHud);
        }

        fn secondary_activate(&self, _x: i32, _y: i32) {}

        fn context_menu(&self, _x: i32, _y: i32) {}

        fn scroll(&self, _delta: i32, _orientation: String) {}

        #[zbus(property)]
        fn category(&self) -> &str {
            "ApplicationStatus"
        }

        #[zbus(property)]
        fn id(&self) -> &str {
            "jai"
        }

        #[zbus(property)]
        fn title(&self) -> &str {
            "J.A.I."
        }

        #[zbus(property)]
        fn status(&self) -> &str {
            "Active"
        }

        #[zbus(property)]
        fn window_id(&self) -> i32 {
            0
        }

        #[zbus(property)]
        fn icon_name(&self) -> &str {
            ""
        }

        #[zbus(property)]
        fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            icon(DOT.load(Ordering::Acquire))
        }

        #[zbus(property)]
        fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
            let tip = if crate::platform::in_test_session() { "test run (starting him ends it)" } else { "online" };
            (String::new(), Vec::new(), "J.A.I.".into(), tip.into())
        }

        #[zbus(property)]
        fn item_is_menu(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn menu(&self) -> ObjectPath<'static> {
            ObjectPath::from_static_str_unchecked(MENU)
        }
    }

    type Props = HashMap<String, OwnedValue>;

    /// A DBusMenu entry: (id, properties, children).
    #[derive(serde::Serialize, zbus::zvariant::Type)]
    struct Layout {
        id: i32,
        properties: Props,
        children: Vec<OwnedValue>,
    }

    fn owned(value: Value<'_>) -> Option<OwnedValue> {
        OwnedValue::try_from(value).ok()
    }

    /// The menu's entries: a command's id is the command, the others count from 100.
    fn entries(state: TrayState) -> Vec<(i32, Props)> {
        menu_lines(state)
            .into_iter()
            .enumerate()
            .map(|(i, (command, text, tick, enabled))| {
                let mut props = Props::new();
                let mut put = |key: &str, value: Value<'_>| {
                    if let Some(value) = owned(value) {
                        props.insert(key.into(), value);
                    }
                };
                if text.is_empty() {
                    put("type", Value::from("separator"));
                } else {
                    put("label", Value::from(text.as_str()));
                    put("enabled", Value::from(enabled));
                    if let Some(on) = tick {
                        put("toggle-type", Value::from("checkmark"));
                        put("toggle-state", Value::from(i32::from(on)));
                    }
                }
                (if command == 0 { 100 + i as i32 } else { command as i32 }, props)
            })
            .collect()
    }

    struct Menu;

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl Menu {
        fn get_layout(&self, parent_id: i32, _recursion_depth: i32, _property_names: Vec<String>) -> (u32, Layout) {
            let entries = entries(now());
            let layout = if parent_id == 0 {
                let children = entries
                    .into_iter()
                    .filter_map(|(id, props)| owned(Value::from(StructureBuilder::new().add_field(id).add_field(props).add_field(Vec::<OwnedValue>::new()).build())))
                    .collect();
                let root = owned(Value::from("submenu")).map(|v| Props::from([("children-display".to_string(), v)])).unwrap_or_default();
                Layout { id: 0, properties: root, children }
            } else {
                let properties = entries.into_iter().find(|(id, _)| *id == parent_id).map(|(_, props)| props).unwrap_or_default();
                Layout { id: parent_id, properties, children: Vec::new() }
            };
            (REVISION.load(Ordering::Acquire), layout)
        }

        fn get_group_properties(&self, ids: Vec<i32>, _property_names: Vec<String>) -> Vec<(i32, Props)> {
            entries(now()).into_iter().filter(|(id, _)| ids.is_empty() || ids.contains(id)).collect()
        }

        fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
            entries(now()).into_iter().find(|(entry, _)| *entry == id).and_then(|(_, mut props)| props.remove(&name)).ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} on {id}")))
        }

        fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
            if event_id == "clicked" {
                let state = now();
                if let Some(action) = usize::try_from(id).ok().and_then(|command| picked(command, state)) {
                    send(action);
                }
            }
        }

        fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
            for (id, event_id, data, timestamp) in events {
                self.event(id, event_id, data, timestamp);
            }
            Vec::new()
        }

        fn about_to_show(&self, _id: i32) -> bool {
            false
        }

        fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
            (Vec::new(), Vec::new())
        }

        #[zbus(property)]
        fn version(&self) -> u32 {
            3
        }

        #[zbus(property)]
        fn text_direction(&self) -> &str {
            "ltr"
        }

        #[zbus(property)]
        fn status(&self) -> &str {
            "normal"
        }

        #[zbus(property)]
        fn icon_theme_path(&self) -> Vec<String> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_menu_offers_the_standby_its_ticks_and_what_each_line_does() {
        let lines = |state| menu_lines(state).into_iter().map(|(command, text, tick, _)| (command, text, tick)).collect::<Vec<_>>();
        let awake = lines(TrayState { mic_on: true, ..TrayState::default() });
        assert!(awake.iter().any(|(c, _, _)| *c == CMD_STANDBY), "awake: the standby line");
        assert!(awake.iter().any(|(c, _, tick)| *c == CMD_MIC && *tick == Some(true)), "the microphone is ticked");
        let dormant = lines(TrayState { dormant: true, ..TrayState::default() });
        assert!(dormant.iter().any(|(c, _, _)| *c == CMD_WAKE) && !dormant.iter().any(|(c, _, _)| *c == CMD_STANDBY));
        let game = lines(TrayState { game: true, ..TrayState::default() });
        assert!(!game.iter().any(|(c, _, _)| *c == CMD_WAKE || *c == CMD_STANDBY), "in game mode the GPU is free already");
        assert_eq!(game.last().map(|l| l.0), Some(CMD_QUIT));
        let state = TrayState { night: true, ..TrayState::default() };
        assert_eq!(picked(CMD_NIGHT, state), Some(TrayAction::Night(false)), "a ticked line is unticked");
        assert_eq!(picked(0, state), None, "the first line and the separators do nothing");
    }

    #[test]
    fn the_icons_dot_sits_in_its_lower_right_corner_with_a_dark_rim() {
        let size = 32;
        let at = |x: i32, y: i32| dot_pixel(size, (y * size + x) as usize, 0x3EE07A);
        assert_eq!(at(26, 26), Some(0xFF3E_E07A), "inside the dot");
        assert_eq!(at(2, 2), None, "the icon's own pixels elsewhere");
        assert!((0..size).flat_map(|y| (0..size).map(move |x| (x, y))).any(|(x, y)| at(x, y) == Some(0xFF10_1418)), "a rim round it");
        assert_eq!(dot_colour(TrayState { game: true, ..TrayState::default() }), 0x9B6BFF);
    }
}
