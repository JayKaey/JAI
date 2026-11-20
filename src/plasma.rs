//! Other programs' windows under KDE Plasma, through Plasma's own window-management protocol (what
//! its task manager uses: every window with its title, program, process, state and place), keys
//! pressed for Sir through KWin's fake input, and how long Sir has not touched keyboard or mouse
//! (ext-idle-notify). One Wayland connection on a thread of its own keeps them current; requests
//! go out from any thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_output, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{self, ExtIdleNotificationV1};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
use wayland_protocols_plasma::plasma_window_management::client::org_kde_plasma_window::{self, OrgKdePlasmaWindow};
use wayland_protocols_plasma::plasma_window_management::client::org_kde_plasma_window_management::{self, OrgKdePlasmaWindowManagement};

/// The state bits of a window (`org_kde_plasma_window_management.state`).
pub mod state {
    pub const ACTIVE: u32 = 0x1;
    pub const MINIMIZED: u32 = 0x2;
    pub const MAXIMIZED: u32 = 0x4;
    pub const FULLSCREEN: u32 = 0x8;
    pub const KEEP_BELOW: u32 = 0x20;
    pub const SKIP_TASKBAR: u32 = 0x1000;
}

/// One window as KWin describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub uuid: String,
    pub title: String,
    /// The program's id ("org.kde.dolphin", "firefox", "steam_app_1245620").
    pub app_id: String,
    /// X11 programs' resource name.
    pub resource_name: String,
    pub pid: u32,
    pub state: u32,
    /// x, y, width, height in the workspace.
    pub geometry: [i32; 4],
    ready: bool,
}

impl Window {
    pub fn is(&self, bit: u32) -> bool {
        self.state & bit != 0
    }

    /// Its program as `/proc` names it ("firefox", "Discord"; empty once it is gone).
    pub fn program(&self) -> String {
        std::fs::read_to_string(format!("/proc/{}/comm", self.pid)).map(|c| c.trim().to_string()).unwrap_or_default()
    }
}

#[derive(Default)]
struct Inner {
    windows: HashMap<String, (OrgKdePlasmaWindow, Window)>,
    /// uuids from the bottom up.
    stacking: Vec<String>,
    outputs: Vec<(wl_output::WlOutput, String)>,
    manager: Option<OrgKdePlasmaWindowManagement>,
    fake: Option<OrgKdeKwinFakeInput>,
    seat: Option<wl_seat::WlSeat>,
    /// The idle notifier and its version; whether its notification is asked for; since when
    /// Sir is idle (None: he is not).
    notifier: Option<(ExtIdleNotifierV1, u32)>,
    watching_idle: bool,
    idle_since: Option<Instant>,
}

/// How long without input before KWin calls Sir idle (the precision of `idle_seconds`).
const IDLE_AFTER: Duration = Duration::from_secs(1);

struct Plasma {
    conn: Connection,
    inner: Arc<Mutex<Inner>>,
}

struct Watch(Arc<Mutex<Inner>>);

impl Watch {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn plasma() -> Option<&'static Plasma> {
    static PLASMA: OnceLock<Option<Plasma>> = OnceLock::new();
    PLASMA
        .get_or_init(|| {
            if cfg!(test) && std::env::var_os("JAI_REAL_PLASMA").is_none() {
                return None;
            }
            let conn = Connection::connect_to_env().ok()?;
            let inner = Arc::new(Mutex::new(Inner::default()));
            let mut watch = Watch(inner.clone());
            let mut queue = conn.new_event_queue();
            conn.display().get_registry(&queue.handle(), ());
            // The globals, then the windows there are, then each one's first description.
            for _ in 0..3 {
                queue.roundtrip(&mut watch).ok()?;
            }
            {
                let mut inner = watch.inner();
                let watching = if let (Some((notifier, version)), Some(seat)) = (&inner.notifier, &inner.seat) {
                    // (Input alone counts, as on Windows: a playing video's inhibitor does not.)
                    let after = IDLE_AFTER.as_millis() as u32;
                    let _ = if *version >= 2 { notifier.get_input_idle_notification(after, seat, &queue.handle(), ()) } else { notifier.get_idle_notification(after, seat, &queue.handle(), ()) };
                    true
                } else {
                    false
                };
                inner.watching_idle = watching;
            }
            std::thread::Builder::new()
                .name("jai-plasma".into())
                .spawn(move || while queue.blocking_dispatch(&mut watch).is_ok() {})
                .ok()?;
            Some(Plasma { conn, inner })
        })
        .as_ref()
}

fn with<R>(f: impl FnOnce(&Inner) -> R) -> Result<R, String> {
    let plasma = plasma().ok_or("KDE Plasma's window list is not here (no Plasma Wayland session)")?;
    let out = f(&plasma.inner.lock().unwrap_or_else(|e| e.into_inner()));
    plasma.conn.flush().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Whether Plasma's window list is reachable.
pub fn available() -> bool {
    plasma().is_some_and(|p| p.inner.lock().unwrap_or_else(|e| e.into_inner()).manager.is_some())
}

/// Every window KWin manages, the topmost first (J.A.I.'s own among them).
pub fn windows() -> Vec<Window> {
    with(|inner| {
        let mut list: Vec<Window> = inner.windows.values().map(|(_, w)| w.clone()).filter(|w| w.ready).collect();
        let height = |w: &Window| inner.stacking.iter().position(|u| *u == w.uuid).unwrap_or(0);
        list.sort_by_key(|w| std::cmp::Reverse(height(w)));
        list
    })
    .unwrap_or_default()
}

/// The window in front.
pub fn active() -> Option<Window> {
    windows().into_iter().find(|w| w.is(state::ACTIVE))
}

fn window(inner: &Inner, uuid: &str) -> Result<OrgKdePlasmaWindow, String> {
    inner.windows.get(uuid).map(|(proxy, _)| proxy.clone()).ok_or_else(|| "That window is gone.".to_string())
}

/// Sets (`on`) or clears the state bits `mask` of a window: `state::ACTIVE` brings it to the
/// front, `MINIMIZED` / `MAXIMIZED` / `FULLSCREEN` … what they say.
pub fn set_state(uuid: &str, mask: u32, on: bool) -> Result<(), String> {
    with(|inner| window(inner, uuid).map(|w| w.set_state(mask, if on { mask } else { 0 })))?
}

/// Asks a window to close, as its close button would.
pub fn close(uuid: &str) -> Result<(), String> {
    with(|inner| window(inner, uuid).map(|w| w.close()))?
}

/// The monitors by their output names ("DP-1", "HDMI-A-1"), in the order KWin announced them.
pub fn outputs() -> Vec<String> {
    with(|inner| inner.outputs.iter().map(|(_, name)| name.clone()).collect()).unwrap_or_default()
}

/// Moves a window onto the monitor with the output name `output`.
pub fn send_to_output(uuid: &str, output: &str) -> Result<(), String> {
    with(|inner| {
        let screen = inner.outputs.iter().find(|(_, name)| name == output).map(|(o, _)| o.clone()).ok_or_else(|| format!("There is no monitor {output}."))?;
        window(inner, uuid).map(|w| w.send_to_output(&screen))
    })?
}

/// Seconds since Sir last touched the keyboard or the mouse, as KWin tells (None without it).
pub fn idle_seconds() -> Option<u64> {
    with(|inner| inner.watching_idle.then(|| inner.idle_since.map_or(0, |since| since.elapsed().as_secs()))).ok().flatten()
}

/// Presses (`down`) or lets go of a key by its Linux key code (`linux/input-event-codes.h`), as
/// if on Sir's keyboard.
pub fn key(code: u32, down: bool) -> Result<(), String> {
    with(|inner| inner.fake.as_ref().map(|fake| fake.keyboard_key(code, u32::from(down))).ok_or_else(|| "KWin lets J.A.I. press no keys here.".to_string()))?
}

impl Dispatch<wl_registry::WlRegistry, ()> for Watch {
    fn event(watch: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        let mut inner = watch.inner();
        match interface.as_str() {
            // (16: the last version with the stacking order as one event.)
            "org_kde_plasma_window_management" => inner.manager = Some(registry.bind(name, version.min(16), qh, ())),
            "org_kde_kwin_fake_input" if version >= 4 => {
                let fake: OrgKdeKwinFakeInput = registry.bind(name, 4, qh, ());
                fake.authenticate("J.A.I.".into(), "Keys Sir asks him to press".into());
                inner.fake = Some(fake);
            }
            "wl_output" if version >= 4 => {
                let output = registry.bind(name, 4, qh, ());
                inner.outputs.push((output, String::new()));
            }
            "wl_seat" if inner.seat.is_none() => inner.seat = Some(registry.bind(name, 1, qh, ())),
            "ext_idle_notifier_v1" => inner.notifier = Some((registry.bind(name, version.min(2), qh, ()), version.min(2))),
            _ => {}
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for Watch {
    fn event(watch: &mut Self, _: &ExtIdleNotificationV1, event: ext_idle_notification_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // (KWin calls him idle once IDLE_AFTER has passed without input: it began that long ago.)
        watch.inner().idle_since = matches!(event, ext_idle_notification_v1::Event::Idled).then(|| Instant::now().checked_sub(IDLE_AFTER).unwrap_or_else(Instant::now));
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for Watch {
    fn event(_: &mut Self, _: &ExtIdleNotifierV1, _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for Watch {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for Watch {
    fn event(watch: &mut Self, manager: &OrgKdePlasmaWindowManagement, event: org_kde_plasma_window_management::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        use org_kde_plasma_window_management::Event;
        match event {
            Event::WindowWithUuid { uuid, .. } => {
                let proxy = manager.get_window_by_uuid(uuid.clone(), qh, uuid.clone());
                watch.inner().windows.insert(uuid.clone(), (proxy, Window { uuid, ..Window::default() }));
            }
            Event::StackingOrderUuidChanged { uuids } => watch.inner().stacking = uuids.split(';').filter(|u| !u.is_empty()).map(str::to_string).collect(),
            _ => {}
        }
    }
}

impl Dispatch<OrgKdePlasmaWindow, String> for Watch {
    fn event(watch: &mut Self, proxy: &OrgKdePlasmaWindow, event: org_kde_plasma_window::Event, uuid: &String, _: &Connection, _: &QueueHandle<Self>) {
        use org_kde_plasma_window::Event;
        let mut inner = watch.inner();
        if matches!(event, Event::Unmapped) {
            inner.windows.remove(uuid);
            proxy.destroy();
            return;
        }
        let Some((_, w)) = inner.windows.get_mut(uuid) else { return };
        match event {
            Event::TitleChanged { title } => w.title = title,
            Event::AppIdChanged { app_id } => w.app_id = app_id,
            Event::ResourceNameChanged { resource_name } => w.resource_name = resource_name,
            Event::PidChanged { pid } => w.pid = pid,
            Event::StateChanged { flags } => w.state = flags,
            Event::Geometry { x, y, width, height } => w.geometry = [x, y, width as i32, height as i32],
            Event::InitialState => w.ready = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Watch {
    fn event(watch: &mut Self, output: &wl_output::WlOutput, event: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Name { name } = event {
            if let Some(slot) = watch.inner().outputs.iter_mut().find(|(o, _)| o == output) {
                slot.1 = name;
            }
        }
    }
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for Watch {
    fn event(_: &mut Self, _: &OrgKdeKwinFakeInput, _: <OrgKdeKwinFakeInput as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

// -------------------------------------------------------------------------------- keys ----

/// The Linux key code (`KEY_…`) of a key by name: "enter", "tab", "f5", "left", "a" …; letters
/// follow Sir's keyboard layout (a German keyboard has Y and Z swapped, a French one A and Q,
/// Z and W).
pub fn key_code(name: &str, layout: &str) -> Option<u32> {
    let name = name.to_ascii_lowercase();
    let named = match name.as_str() {
        "esc" | "escape" => 1,
        "backspace" => 14,
        "tab" => 15,
        "enter" | "return" => 28,
        "ctrl" | "control" => 29,
        "shift" => 42,
        "alt" => 56,
        "space" => 57,
        "home" => 102,
        "up" => 103,
        "pageup" | "page up" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pagedown" | "page down" => 109,
        "insert" => 110,
        "delete" | "del" => 111,
        "super" | "meta" | "win" => 125,
        f if f.len() > 1 && f.starts_with('f') => match f[1..].parse::<u32>().ok()? {
            n @ 1..=10 => 58 + n,
            11 => 87,
            12 => 88,
            _ => return None,
        },
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return None };
    let c = match (layout, c) {
        ("de" | "at" | "ch", 'y') => 'z',
        ("de" | "at" | "ch", 'z') => 'y',
        ("fr" | "be", 'a') => 'q',
        ("fr" | "be", 'q') => 'a',
        ("fr" | "be", 'z') => 'w',
        ("fr" | "be", 'w') => 'z',
        _ => c,
    };
    const ROWS: [(&str, u32); 4] = [("1234567890", 2), ("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)];
    ROWS.iter().find_map(|(row, first)| row.find(c).map(|i| first + i as u32))
}

/// Sir's first keyboard layout as KDE keeps it ("de", "us"), "us" when none is set.
pub fn layout() -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let text = std::fs::read_to_string(home.join(".config/kxkbrc")).unwrap_or_default();
    text.lines().find_map(|l| l.strip_prefix("LayoutList=")).and_then(|l| l.split(',').next()).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| "us".into())
}

/// Presses keys named as people write them: "F11", "Ctrl+W", "Ctrl+Shift+F" (the last one is
/// the key, the others are held).
pub fn press(keys: &str) -> Result<(), String> {
    let layout = layout();
    let codes = keys.split('+').map(|k| key_code(k.trim(), &layout).ok_or_else(|| format!("I know no key called '{}'.", k.trim()))).collect::<Result<Vec<u32>, String>>()?;
    let (key, with) = codes.split_last().ok_or("There is no key to press.")?;
    chord(with, *key)
}

/// Presses `key` with the modifiers `with` (codes from `key_code`), then lets all go again.
pub fn chord(with: &[u32], key: u32) -> Result<(), String> {
    for m in with {
        self::key(*m, true)?;
    }
    let pressed = self::key(key, true).and_then(|_| self::key(key, false));
    for m in with.iter().rev() {
        let _ = self::key(*m, false);
    }
    pressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_found_by_name_and_letters_follow_the_layout() {
        assert_eq!(key_code("Enter", "us"), Some(28));
        assert_eq!(key_code("f5", "us"), Some(63));
        assert_eq!(key_code("F11", "us"), Some(87));
        assert_eq!(key_code("a", "us"), Some(30));
        assert_eq!(key_code("v", "us"), Some(47));
        assert_eq!(key_code("z", "us"), Some(44));
        assert_eq!(key_code("z", "de"), Some(21), "a German Z sits where the American Y does");
        assert_eq!(key_code("y", "de"), Some(44));
        assert_eq!(key_code("a", "fr"), Some(16), "a French A sits where the American Q does");
        assert_eq!(key_code("0", "us"), Some(11));
        assert_eq!(key_code("banana", "us"), None);
        assert_eq!(key_code("f13", "us"), None);
    }
}
