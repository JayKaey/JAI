//! The session-bus side of J.A.I.OS's shell: it is the notification server
//! (org.freedesktop.Notifications), the tray's host (it reads the StatusNotifierItems kded's
//! watcher knows, their pictures and their menus), it answers for Plasma's shell where other
//! parts of KDE call it (the on-screen display of the volume and brightness keys, the Meta
//! key's menu), and it talks to the assistant (`os_link`). Everything that arrives becomes an
//! `Ev` for the shell's own thread; what the shell asks for goes out on a thread of its own, so
//! a program that does not answer never holds the taskbar up.

use crate::kde;
use crate::os_notes::{Note, Osd};
use crate::os_shell::{ListRow, Power, TrayClick};
use eframe::egui::ColorImage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use zbus::zvariant::{OwnedObjectPath, OwnedStructure, OwnedValue, Value};

/// The shell's own name on the bus, with the one method KWin calls for the Meta key.
pub const SHELL: &str = "org.jaios.Shell";
const SHELL_PATH: &str = "/Shell";
const NOTES: &str = "org.freedesktop.Notifications";
const NOTES_PATH: &str = "/org/freedesktop/Notifications";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const DBUS: &str = "org.freedesktop.DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// One item of the tray as its program describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrayEntry {
    pub service: String,
    pub path: String,
    /// Where its menu is served (empty: it shows its own).
    pub menu: String,
    pub title: String,
    pub attention: bool,
    pub icon_name: String,
    /// A folder of its own the icon may lie in.
    pub theme_path: String,
    /// Its picture, when it sends one instead of a name.
    pub pixmap: Option<Arc<ColorImage>>,
}

/// What reaches the shell's thread.
pub enum Ev {
    ToggleMenu,
    /// A notification with the time it asked for (milliseconds) and its urgency.
    Note(Note, i32, u8),
    CloseNote(u32),
    Osd(Osd),
    Tray(Vec<TrayEntry>),
    /// What the assistant is doing; empty when he is not there.
    State(String),
    /// A tray item's menu arrived: the service and path it is served at, and its rows.
    TrayMenu(String, String, Vec<ListRow>),
    /// A picture was read for the key it was asked under.
    Icon(String, Arc<ColorImage>),
}

/// The way to the shell's thread: the event, then a byte into the pipe its loop waits on.
#[derive(Clone)]
pub struct Hub {
    tx: Sender<Ev>,
    wake: i32,
}

impl Hub {
    pub fn send(&self, ev: Ev) {
        if self.tx.send(ev).is_ok() {
            // SAFETY: one byte from a live buffer into the pipe's write end, which stays open
            // for as long as the process runs.
            unsafe { libc::write(self.wake, [1u8].as_ptr().cast(), 1) };
        }
    }
}

struct ShellBus(Hub);

#[zbus::interface(name = "org.jaios.Shell")]
impl ShellBus {
    /// The [J] menu opens, or closes when it is open (KWin calls this for the Meta key).
    fn toggle_menu(&self) {
        self.0.send(Ev::ToggleMenu);
    }
}

struct Notifications {
    hub: Hub,
    next: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(&self, app_name: String, replaces_id: u32, app_icon: String, summary: String, body: String, actions: Vec<String>, hints: HashMap<String, OwnedValue>, expire_timeout: i32) -> u32 {
        let id = if replaces_id != 0 { replaces_id } else { self.next.fetch_add(1, Ordering::Relaxed) };
        let hint = |name: &str| hints.get(name).map(|v| &**v);
        let urgency = match hint("urgency") {
            Some(Value::U8(u)) => *u,
            _ => 1,
        };
        let icon = match hint("image-path").or(hint("image_path")) {
            Some(Value::Str(path)) if app_icon.is_empty() => path.as_str().trim_start_matches("file://").to_string(),
            _ => app_icon,
        };
        let note = Note { id, app: app_name, icon, summary, body: crate::os_notes::plain(&body), actions: crate::os_notes::actions(&actions), until: None };
        self.hub.send(Ev::Note(note, expire_timeout, urgency));
        id
    }

    fn close_notification(&self, id: u32) {
        self.hub.send(Ev::CloseNote(id));
    }

    fn get_capabilities(&self) -> Vec<String> {
        ["actions", "body", "body-markup", "icon-static", "persistence"].map(String::from).to_vec()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("J.A.I.".into(), "J.A.I.OS".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }
}

/// Takes the shell's names and starts everything that listens. Err when another shell holds
/// the name (one desktop has one shell) or there is no bus.
pub fn start() -> Result<(Hub, Receiver<Ev>, i32), String> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let bus = kde::bus()?;
    let mut pipe = [0i32; 2];
    // SAFETY: pipe2 fills the two descriptors of the array it is given.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err("no pipe to wake the shell with".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let hub = Hub { tx, wake: pipe[1] };
    bus.object_server().at(SHELL_PATH, ShellBus(hub.clone())).map_err(|e| e.to_string())?;
    match bus.request_name_with_flags(SHELL, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(_) => return Err("another J.A.I.OS shell runs on this desktop".into()),
        Err(e) => return Err(format!("the name {SHELL} could not be taken: {e}")),
    }
    // The notifications are his when no other server has them (in safe mode Plasma's own has).
    let served = bus.object_server().at(NOTES_PATH, Notifications { hub: hub.clone(), next: AtomicU32::new(1) }).is_ok();
    if !served || !matches!(bus.request_name_with_flags(NOTES, RequestNameFlags::DoNotQueue.into()), Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner)) {
        warn!("[SHELL] Another program shows the notifications: his cards stay away");
    }
    // Plasma's shell is not there: its name is answered for, so the volume and brightness
    // keys still find an on-screen display, and KWin's stock Meta key the menu.
    let _ = bus.request_name_with_flags("org.kde.plasmashell", RequestNameFlags::DoNotQueue.into());
    for (name, work) in [("jai-shell-calls", plasma_calls as fn(Hub)), ("jai-shell-tray", tray as fn(Hub)), ("jai-shell-link", link as fn(Hub))] {
        let hub = hub.clone();
        std::thread::Builder::new().name(name.into()).spawn(move || work(hub)).map_err(|e| e.to_string())?;
    }
    Ok((hub, rx, pipe[0]))
}

/// The calls other parts of KDE make on Plasma's shell: the on-screen display's
/// (`org.kde.osdService`, whose methods differ between Plasma versions, so each call is read
/// by what it carries) and the launcher menu's.
fn plasma_calls(hub: Hub) {
    let Ok(bus) = kde::bus() else { return };
    for message in zbus::blocking::MessageIterator::from(bus).flatten() {
        let header = message.header();
        if header.message_type() != zbus::message::Type::MethodCall {
            continue;
        }
        let (interface, member) = (header.interface().map(|i| i.as_str()).unwrap_or_default(), header.member().map(|m| m.as_str()).unwrap_or_default());
        match interface {
            "org.kde.PlasmaShell" if member == "activateLauncherMenu" => hub.send(Ev::ToggleMenu),
            "org.kde.osdService" => {
                let body = message.body();
                let fields = body.deserialize::<OwnedStructure>().ok();
                let fields = fields.as_ref().map(|s| s.0.fields()).unwrap_or_default();
                let number = fields.iter().find_map(|f| if let Value::I32(n) = f { Some(*n) } else { None });
                let flag = fields.iter().find_map(|f| if let Value::Bool(b) = f { Some(*b) } else { None });
                let words = fields.iter().rev().find_map(|f| if let Value::Str(s) = f { Some(s.as_str()) } else { None }).unwrap_or_default();
                if let Some(osd) = crate::os_notes::osd_of(member, number, flag, words) {
                    hub.send(Ev::Osd(osd));
                }
            }
            _ => {}
        }
    }
}

fn name_owner(name: &str) -> String {
    kde::call::<_, String>(DBUS, "/org/freedesktop/DBus", DBUS, "GetNameOwner", &(name,)).unwrap_or_default()
}

/// One property, asked for directly (no proxy with its cache for a single read).
fn property<T: TryFrom<OwnedValue>>(service: &str, path: &str, interface: &str, name: &str) -> Option<T> {
    kde::call::<_, OwnedValue>(service, path, PROPERTIES, "Get", &(interface, name)).ok().and_then(|v| T::try_from(v).ok())
}

/// The item a watcher's entry names: "service/path", or a service alone at the usual path.
fn item_address(entry: &str) -> (String, String) {
    match entry.find('/') {
        Some(at) => (entry[..at].to_string(), entry[at..].to_string()),
        None => (entry.to_string(), "/StatusNotifierItem".to_string()),
    }
}

fn tray_entry(entry: &str) -> Option<TrayEntry> {
    let (service, path) = item_address(entry);
    let text = |name: &str| property::<String>(&service, &path, ITEM, name).unwrap_or_default();
    // (An item that does not even say its id is gone or not one.)
    let id = property::<String>(&service, &path, ITEM, "Id")?;
    let status = text("Status");
    let title = Some(text("Title")).filter(|t| !t.is_empty()).unwrap_or(id);
    let attention = status == "NeedsAttention";
    let icon_name = Some(text("AttentionIconName")).filter(|n| attention && !n.is_empty()).unwrap_or_else(|| text("IconName"));
    let pixmap = if icon_name.is_empty() {
        property::<Vec<(i32, i32, Vec<u8>)>>(&service, &path, ITEM, "IconPixmap").and_then(|all| all.into_iter().max_by_key(|(w, _, _)| *w)).and_then(|(w, h, argb)| crate::os_shell::pixmap_image(w, h, &argb)).map(Arc::new)
    } else {
        None
    };
    let menu = property::<OwnedObjectPath>(&service, &path, ITEM, "Menu").map(|p| p.as_str().to_string()).filter(|p| p != "/NO_DBUSMENU").unwrap_or_default();
    Some(TrayEntry { theme_path: text("IconThemePath"), service, path, menu, title, attention, icon_name, pixmap })
}

/// The tray's host: tells kded's watcher it is there (again whenever the watcher starts
/// anew), and reads the items when the watcher's list or one of them changes.
fn tray(hub: Hub) {
    let Ok(bus) = kde::bus() else { return };
    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    let _ = bus.request_name(host.as_str());
    let changed = Arc::new(AtomicBool::new(true));
    let (seen, of_bus) = (changed.clone(), bus.clone());
    let listening = std::thread::Builder::new().name("jai-shell-tray-signals".into()).spawn(move || {
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(ITEM).map(|r| r.build());
        let Ok(signals) = rule.and_then(|rule| zbus::blocking::MessageIterator::for_match_rule(rule, &of_bus, Some(64))) else { return };
        for _ in signals.flatten() {
            seen.store(true, Ordering::Release);
        }
    });
    if listening.is_err() {
        warn!("[SHELL] The tray's items are only read as they register");
    }
    let (mut watcher, mut names, mut shown) = (String::new(), Vec::new(), Vec::new());
    for round in 0u32.. {
        let owner = name_owner(WATCHER);
        // The watcher is a part of kded that Plasma's own tray asks for: so does his.
        if owner.is_empty() && round % 15 == 0 {
            let _ = kde::call::<_, bool>("org.kde.kded6", "/kded", "org.kde.kded6", "loadModule", &("statusnotifierwatcher",));
        }
        if owner != watcher {
            if !owner.is_empty() {
                match kde::call::<_, ()>(WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierHost", &(host.as_str(),)) {
                    Ok(()) => info!("[SHELL] The tray's watcher knows the taskbar"),
                    Err(e) => warn!("[SHELL] The tray's watcher did not take the taskbar: {e}"),
                }
            }
            watcher = owner;
        }
        let now = property::<Vec<String>>(WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisteredStatusNotifierItems").unwrap_or_default();
        if now != names || changed.swap(false, Ordering::AcqRel) {
            names = now;
            let entries: Vec<TrayEntry> = names.iter().filter_map(|name| tray_entry(name)).collect();
            if entries != shown {
                shown = entries.clone();
                hub.send(Ev::Tray(entries));
            }
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

/// Plasma's start-up picture stays until the desktop says it stands: his does now.
pub fn desktop_stands() {
    if crate::platform::hands_off() {
        return;
    }
    std::thread::spawn(|| {
        let _ = kde::call::<_, ()>("org.kde.KSplash", "/KSplash", "org.kde.KSplash", "setStage", &("desktop",));
    });
}

/// What Sir did to a tray item goes to its program; its menu comes back as rows.
pub fn tray_click(hub: &Hub, entry: &TrayEntry, click: TrayClick) {
    if crate::platform::hands_off() {
        return;
    }
    let (hub, entry) = (hub.clone(), entry.clone());
    std::thread::spawn(move || {
        let call = |method: &str| kde::call::<_, ()>(&entry.service, &entry.path, ITEM, method, &(0i32, 0i32));
        let menu = |entry: &TrayEntry| {
            if entry.menu.is_empty() {
                return call("ContextMenu").is_ok();
            }
            let _ = kde::call::<_, bool>(&entry.service, &entry.menu, MENU, "AboutToShow", &(0i32,));
            let layout = kde::call::<_, OwnedStructure>(&entry.service, &entry.menu, MENU, "GetLayout", &(0i32, -1i32, Vec::<String>::new()));
            let rows = layout.ok().and_then(|l| l.0.fields().get(1).map(menu_rows)).unwrap_or_default();
            if !rows.is_empty() {
                hub.send(Ev::TrayMenu(entry.service.clone(), entry.menu.clone(), rows));
            }
            true
        };
        let done = match click {
            // (An item that is only a menu takes no activation: its menu opens instead.)
            TrayClick::Activate => call("Activate").is_ok() || menu(&entry),
            TrayClick::Secondary => call("SecondaryActivate").is_ok(),
            TrayClick::Menu => menu(&entry),
        };
        if !done {
            info!("[SHELL] {} took no click", entry.title);
        }
    });
}

fn unwrapped<'v, 'a>(value: &'v Value<'a>) -> &'v Value<'a> {
    match value {
        Value::Value(inner) => unwrapped(inner),
        other => other,
    }
}

/// The rows of a dbusmenu layout (`(ia{sv}av)`, each child a variant of the same): what is
/// visible, a sub-menu's rows under its name and one step in.
pub fn menu_rows(layout: &Value<'_>) -> Vec<ListRow> {
    fn walk(item: &Value<'_>, depth: u8, out: &mut Vec<ListRow>) {
        let Value::Structure(item) = unwrapped(item) else { return };
        let [Value::I32(id), Value::Dict(props), Value::Array(children)] = item.fields() else { return };
        let prop = |name: &str| props.iter().find(|(key, _)| matches!(key, Value::Str(key) if key.as_str() == name)).map(|(_, value)| unwrapped(value));
        let text = |name: &str| if let Some(Value::Str(s)) = prop(name) { s.as_str().to_string() } else { String::new() };
        let flag = |name: &str| !matches!(prop(name), Some(Value::Bool(false)));
        let mut inner = depth;
        if *id != 0 {
            if !flag("visible") {
                return;
            }
            let label = if text("type") == "separator" { String::new() } else { text("label").replace('_', "") };
            out.push(ListRow { id: *id, label, enabled: flag("enabled") && children.inner().is_empty(), ticked: matches!(prop("toggle-state"), Some(Value::I32(1))), depth });
            inner += 1;
        }
        for child in children.inner() {
            walk(child, inner, out);
        }
    }
    let mut rows = Vec::new();
    walk(layout, 0, &mut rows);
    // (No rule at either end, and never two in a row.)
    rows.dedup_by(|a, b| a.label.is_empty() && b.label.is_empty());
    while rows.first().is_some_and(|r| r.label.is_empty()) {
        rows.remove(0);
    }
    while rows.last().is_some_and(|r| r.label.is_empty()) {
        rows.pop();
    }
    rows
}

/// Sir chose row `id` of the menu served at `service` `menu`.
pub fn menu_pick(service: String, menu: String, id: i32) {
    if crate::platform::hands_off() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = kde::call::<_, ()>(&service, &menu, MENU, "Event", &(id, "clicked", Value::from(0i32), 0u32)) {
            warn!("[SHELL] The menu did not take the choice: {e}");
        }
    });
}

/// Tells the program whose notification `id` was that it closed, and why (`os_notes::EXPIRED` …).
pub fn note_closed(id: u32, reason: u32) {
    if let Ok(bus) = kde::bus() {
        let _ = bus.emit_signal(None::<&str>, NOTES_PATH, NOTES, "NotificationClosed", &(id, reason));
    }
}

/// Tells the program that Sir pressed the key `action` of its notification.
pub fn action_invoked(id: u32, action: &str) {
    if let Ok(bus) = kde::bus() {
        let _ = bus.emit_signal(None::<&str>, NOTES_PATH, NOTES, "ActionInvoked", &(id, action));
    }
}

/// Follows the assistant: what he is doing, from his property and its change signal; "" while
/// he is not on the bus.
fn link(hub: Hub) {
    use crate::os_link::{JAI, JAI_PATH};
    let Ok(bus) = kde::bus() else { return };
    let (of_hub, of_bus) = (hub.clone(), bus.clone());
    let _ = std::thread::Builder::new().name("jai-shell-link-signals".into()).spawn(move || {
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(PROPERTIES).and_then(|r| r.member("PropertiesChanged")).and_then(|r| r.path(JAI_PATH)).map(|r| r.build());
        let Ok(signals) = rule.and_then(|rule| zbus::blocking::MessageIterator::for_match_rule(rule, &of_bus, Some(16))) else { return };
        for signal in signals.flatten() {
            if let Ok((_, changed, _)) = signal.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() {
                if let Some(Value::Str(state)) = changed.get("State").map(|v| &**v) {
                    of_hub.send(Ev::State(state.as_str().to_string()));
                }
            }
        }
    });
    let mut owner = String::new();
    loop {
        let now = name_owner(JAI);
        if now != owner {
            hub.send(Ev::State(if now.is_empty() { String::new() } else { property::<String>(JAI, JAI_PATH, JAI, "State").unwrap_or_else(|| "idle".into()) }));
            owner = now;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// What was typed into the menu's search line is an order for him.
pub fn order(text: String) {
    if crate::platform::hands_off() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = kde::call::<_, ()>(crate::os_link::JAI, crate::os_link::JAI_PATH, crate::os_link::JAI, "Order", &(text.as_str(),)) {
            warn!("[SHELL] J.A.I. did not take the order: {e}");
            let _ = kde::notify("J.A.I. is not running", "The order was not given: start him from the menu, then say or type it again.");
        }
    });
}

/// The pins of the menu are his to keep (they live in his settings).
pub fn keep_favourites(ids: Vec<String>) {
    if crate::platform::hands_off() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = kde::call::<_, ()>(crate::os_link::JAI, crate::os_link::JAI_PATH, crate::os_link::JAI, "SetFavourites", &(ids,)) {
            warn!("[SHELL] The pins are kept until the shell ends only (J.A.I. is not running): {e}");
        }
    });
}

/// His HUD comes up (the lamp was clicked).
pub fn show_hud() {
    if crate::platform::hands_off() {
        return;
    }
    std::thread::spawn(|| {
        let _ = kde::call::<_, ()>("org.jai.JAI", "/org/jai/JAI", "org.jai.JAI", "Show", &());
    });
}

/// A power key of the menu: the lock and sleep go the assistant's own way; what ends the
/// session goes through KDE's session manager (programs are asked to save), and straight to
/// logind where that is not there.
pub fn power(power: Power) {
    if crate::platform::hands_off() {
        return;
    }
    std::thread::spawn(move || {
        let session = |method: &str| kde::call::<_, ()>("org.kde.Shutdown", "/Shutdown", "org.kde.Shutdown", method, &());
        let done = match power {
            Power::Lock => crate::system_power::lock_pc(),
            Power::Sleep => crate::system_power::sleep_pc(),
            Power::LogOut => session("logout"),
            Power::Restart => session("logoutAndReboot").or_else(|_| crate::system_power::logind("Reboot")),
            Power::ShutDown => session("logoutAndShutdown").or_else(|_| crate::system_power::logind("PowerOff")),
        };
        if let Err(e) = done {
            warn!("[SHELL] {power:?} did not happen: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Structure;

    fn item(id: i32, props: &[(&str, Value<'static>)], children: Vec<Value<'static>>) -> Value<'static> {
        let props: HashMap<String, Value<'static>> = props.iter().map(|(k, v)| (k.to_string(), Value::Value(Box::new(v.try_clone().unwrap())))).collect();
        let children: Vec<Value<'static>> = children.into_iter().map(|c| Value::Value(Box::new(c))).collect();
        Value::Structure(Structure::from((id, props, children)))
    }

    #[test]
    fn a_dbusmenu_layout_becomes_rows() {
        let layout = item(
            0,
            &[("children-display", Value::from("submenu"))],
            vec![
                item(1, &[("type", Value::from("separator"))], vec![]),
                item(2, &[("label", Value::from("_Open Discord"))], vec![]),
                item(3, &[("label", Value::from("Mute")), ("toggle-type", Value::from("checkmark")), ("toggle-state", Value::from(1i32))], vec![]),
                item(4, &[("type", Value::from("separator"))], vec![]),
                item(5, &[("type", Value::from("separator"))], vec![]),
                item(6, &[("label", Value::from("Status")), ("children-display", Value::from("submenu"))], vec![item(7, &[("label", Value::from("Online"))], vec![]), item(8, &[("label", Value::from("Hidden")), ("visible", Value::from(false))], vec![])]),
                item(9, &[("label", Value::from("Quit")), ("enabled", Value::from(false))], vec![]),
                item(10, &[("type", Value::from("separator"))], vec![]),
            ],
        );
        let rows = menu_rows(&layout);
        let seen: Vec<(i32, &str, bool, bool, u8)> = rows.iter().map(|r| (r.id, r.label.as_str(), r.enabled, r.ticked, r.depth)).collect();
        assert_eq!(seen, [(2, "Open Discord", true, false, 0), (3, "Mute", true, true, 0), (4, "", true, false, 0), (6, "Status", false, false, 0), (7, "Online", true, false, 1), (9, "Quit", false, false, 0)]);
        assert_eq!(menu_rows(&Value::from(3i32)), Vec::new());
    }

    /// KWin lends the window list and the keys to the program a menu entry names: the entry
    /// J.A.I.OS ships is the one the installer writes for the program's place there.
    #[test]
    fn the_shipped_menu_entry_grants_what_the_installers_does() {
        assert_eq!(include_str!("../os/settings/usr/share/applications/jai.desktop"), crate::installer::menu_entry(std::path::Path::new("/usr/lib/jai/jai")));
        let unit = include_str!("../os/settings/usr/lib/systemd/user/jai-shell.service");
        for line in ["ExecStart=/usr/lib/jai/jai --shell", "Restart=on-failure", "StartLimitBurst=3", "StartLimitIntervalSec=60", "OnFailure=jai-safe-desktop.service"] {
            assert!(unit.lines().any(|l| l == line), "jai-shell.service lacks {line}");
        }
        // (An empty unit file in /etc masks Plasma's own shell.)
        assert!(include_str!("../os/settings/etc/systemd/user/plasma-plasmashell.service").is_empty());
    }

    #[test]
    fn a_tray_item_is_found_by_the_watchers_entry() {
        assert_eq!(item_address(":1.42/StatusNotifierItem"), (":1.42".to_string(), "/StatusNotifierItem".to_string()));
        assert_eq!(item_address("org.kde.StatusNotifierItem-4242-1"), ("org.kde.StatusNotifierItem-4242-1".to_string(), "/StatusNotifierItem".to_string()));
        assert_eq!(item_address(":1.7/org/ayatana/NotificationItem/discord"), (":1.7".to_string(), "/org/ayatana/NotificationItem/discord".to_string()));
    }
}
