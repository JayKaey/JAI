//! `jai --shell`: J.A.I.OS's desktop shell as a Wayland client of KWin, a light process of its
//! own (no models, no voice). Every piece `os_shell` and `os_notes` draw is a layer-shell
//! surface (zwlr_layer_shell_v1, which winit cannot open): per monitor the desktop on the
//! background layer, his core on the bottom layer over its middle, and the taskbar on the top
//! layer with an exclusive zone; the [J] menu, a tray menu, the notification cards and the
//! on-screen display come and go above them.
//!
//! Drawn on the CPU into wl_shm buffers, all of them: the same egui frames the tests take
//! pictures of, rasterised by `ui_snapshot`. These surfaces change when something happens (a
//! window opens, the minute turns, the pointer crosses a key), not sixty times a second, and
//! shared memory is the one path that is the same on NVIDIA's own driver, AMD and Intel: a
//! taskbar must come up on every card, and in safe situations where EGL does not. The core is
//! the one piece that moves; it has a small surface of its own, drawn once a second while he
//! rests (his clock ring steps) and faster while he listens, thinks or speaks.
//! shortcut: the core is drawn on the CPU too, about fifteen times a second while he is busy;
//! give its surface EGL (glow) when it should move as smoothly as the HUD's.
//!
//! Sizes are in points; a surface's buffer is its size times the scale KWin prefers for it
//! (wp_fractional_scale_v1, shown through wp_viewporter; whole scales from wl_output without
//! them). Monitors come and go with their surfaces.

use crate::holo_core::{Core, CoreStyle, Feed, Mood, Pose};
use crate::os_apps::{self, App};
use crate::os_bus::{self, Ev, Hub, TrayEntry};
use crate::os_notes::{self, NoteAction, Queue};
use crate::os_shell::{self, Bar, BarAction, DeskItem, Group, Icons, ListRow, Menu, MenuAction, Task, TrayItem};
use crate::plasma;
use crate::ui_snapshot::Snapshot;
use eframe::egui::{self, pos2, vec2, Pos2, Rect};
use std::collections::HashSet;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};
use wayland_client::protocol::{wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{self, WpFractionalScaleV1};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Desktop,
    Core,
    Bar,
    Menu,
    /// A tray item's menu.
    List,
    Notes,
    Osd,
}

/// One layer-shell surface with the egui context that draws it.
struct Surf {
    id: u32,
    kind: Kind,
    /// The registry name of its monitor.
    output: u32,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    /// The scale in 120ths.
    scale120: u32,
    /// Its size in points as KWin configured it (0: not yet).
    size: (u32, u32),
    snap: Option<Snapshot>,
    events: Vec<egui::Event>,
    pointer: Pos2,
    dirty: bool,
    /// Since when a frame callback is awaited (KWin calls when the last picture was shown).
    waiting: Option<Instant>,
    /// When egui wants the next picture (something on it moves).
    next: Option<Instant>,
    drawn: bool,
}

struct Output {
    name: u32,
    output: wl_output::WlOutput,
    scale: i32,
    /// Its surfaces were opened.
    used: bool,
}

/// The keyboard's map (xkbcommon, loaded as the window toolkit loads it): which key gives
/// which sign in Sir's layout.
struct Xkb {
    lib: &'static xkbcommon_dl::XkbCommon,
    context: *mut xkbcommon_dl::xkb_context,
    keymap: *mut xkbcommon_dl::xkb_keymap,
    state: *mut xkbcommon_dl::xkb_state,
}

impl Xkb {
    fn new() -> Option<Self> {
        let lib = xkbcommon_dl::xkbcommon_option()?;
        // SAFETY: a plain constructor of libxkbcommon; null is checked.
        let context = unsafe { (lib.xkb_context_new)(xkbcommon_dl::xkb_context_flags::XKB_CONTEXT_NO_FLAGS) };
        (!context.is_null()).then_some(Self { lib, context, keymap: std::ptr::null_mut(), state: std::ptr::null_mut() })
    }

    /// The map KWin sends: a file of `size` bytes holding its text.
    fn load(&mut self, fd: OwnedFd, size: u32) {
        // SAFETY: the descriptor is mapped read-only for exactly the size KWin names and
        // unmapped before it closes; xkbcommon copies what it reads (a terminated text, as
        // the protocol says). The old map and state are released once the new ones stand.
        unsafe {
            let map = libc::mmap(std::ptr::null_mut(), size as usize, libc::PROT_READ, libc::MAP_PRIVATE, fd.as_raw_fd(), 0);
            if map == libc::MAP_FAILED {
                return;
            }
            let keymap = (self.lib.xkb_keymap_new_from_string)(self.context, map.cast(), xkbcommon_dl::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1, xkbcommon_dl::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS);
            libc::munmap(map, size as usize);
            if keymap.is_null() {
                return;
            }
            let state = (self.lib.xkb_state_new)(keymap);
            if state.is_null() {
                (self.lib.xkb_keymap_unref)(keymap);
                return;
            }
            if !self.state.is_null() {
                (self.lib.xkb_state_unref)(self.state);
                (self.lib.xkb_keymap_unref)(self.keymap);
            }
            (self.keymap, self.state) = (keymap, state);
        }
    }

    fn modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if !self.state.is_null() {
            // SAFETY: the state is live (checked) and owned by this struct.
            unsafe { (self.lib.xkb_state_update_mask)(self.state, depressed, latched, locked, 0, 0, group) };
        }
    }

    /// The key symbol and the text of the key with the evdev code `key`.
    fn key(&self, key: u32) -> (u32, String) {
        if self.state.is_null() {
            return (0, String::new());
        }
        let mut text = [0u8; 16];
        // SAFETY: the state is live; the buffer's length is passed, and xkbcommon terminates
        // what it writes within it.
        let (sym, written) = unsafe { ((self.lib.xkb_state_key_get_one_sym)(self.state, key + 8), (self.lib.xkb_state_key_get_utf8)(self.state, key + 8, text.as_mut_ptr().cast(), text.len())) };
        let written = (written.max(0) as usize).min(text.len() - 1);
        (sym, String::from_utf8_lossy(&text[..written]).into_owned())
    }
}

/// The egui key of an X key symbol, for the keys the menu uses.
fn egui_key(sym: u32) -> Option<egui::Key> {
    Some(match sym {
        0xff1b => egui::Key::Escape,
        0xff0d | 0xff8d => egui::Key::Enter,
        0xff08 => egui::Key::Backspace,
        0xffff => egui::Key::Delete,
        0xff09 => egui::Key::Tab,
        0xff50 => egui::Key::Home,
        0xff57 => egui::Key::End,
        0xff51 => egui::Key::ArrowLeft,
        0xff52 => egui::Key::ArrowUp,
        0xff53 => egui::Key::ArrowRight,
        0xff54 => egui::Key::ArrowDown,
        _ => return None,
    })
}

/// What the shell shows, whatever the surface.
struct Model {
    apps: Vec<App>,
    apps_read: Option<Instant>,
    icons: Icons,
    /// The pictures asked for (each once).
    asked: HashSet<String>,
    icon_work: std::sync::mpsc::Sender<(String, String, u32, String)>,
    tasks: Vec<Task>,
    order: Vec<String>,
    groups: Vec<Group>,
    tray: Vec<TrayEntry>,
    tray_items: Vec<TrayItem>,
    state: String,
    clock: (String, String),
    menu: Option<Menu>,
    pinned: Vec<String>,
    desk_dir: PathBuf,
    desk: Vec<DeskItem>,
    desk_seen: Option<SystemTime>,
    selected: Option<usize>,
    notes: Queue,
    osd: Option<(os_notes::Osd, Instant)>,
    /// The open tray menu: where it is served, its rows, and when it closes untouched.
    list: Option<(String, String, Vec<ListRow>, Instant)>,
    core: Core,
    settings_seen: Option<SystemTime>,
    lang: String,
}

struct Shell {
    qh: QueueHandle<Shell>,
    hub: Hub,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<(ZwlrLayerShellV1, u32)>,
    viewporter: Option<WpViewporter>,
    fractional: Option<WpFractionalScaleManagerV1>,
    cursors: Option<WpCursorShapeManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<(wl_pointer::WlPointer, Option<WpCursorShapeDeviceV1>)>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    outputs: Vec<Output>,
    surfs: Vec<Surf>,
    next_id: u32,
    pointer_on: Option<u32>,
    keys_on: Option<u32>,
    /// The monitor the pointer was last on: where the menu and the cards open.
    last_output: Option<u32>,
    xkb: Option<Xkb>,
    modifiers: egui::Modifiers,
    /// The key held down and when it repeats next.
    repeat: Option<(u32, Instant)>,
    start: Instant,
    ticked: Instant,
    polled: Instant,
    /// The first taskbar was drawn (Plasma's start-up picture was told).
    stands: bool,
    model: Model,
}

/// The folders the menu entries and the icons lie in, as the XDG base directories name them.
fn data_dirs(sub: &str) -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let own = std::env::var("XDG_DATA_HOME").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| format!("{home}/.local/share"));
    let shared = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(own).chain(shared.split(':').map(str::to_string)).map(|d| Path::new(&d).join(sub)).collect()
}

/// Reads pictures off the shell's thread: (the key it is asked under, the icon's name or
/// file, its size in pixels, a folder of the program's own). Each comes back as `Ev::Icon`.
fn icon_worker(hub: Hub) -> std::sync::mpsc::Sender<(String, String, u32, String)> {
    let (tx, rx) = std::sync::mpsc::channel::<(String, String, u32, String)>();
    let work = move || {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut bases = vec![PathBuf::from(&home).join(".icons")];
        bases.extend(data_dirs("icons"));
        bases.push("/usr/share/pixmaps".into());
        // The theme KDE is set to; J.A.I.OS's own default is Breeze's dark one, on his black.
        let theme = std::fs::read_to_string(format!("{home}/.config/kdeglobals")).ok().and_then(|t| t.split("[Icons]").nth(1).and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("Theme=").map(str::to_string)))).unwrap_or_else(|| "breeze-dark".into());
        for (key, name, px, own) in rx {
            let mut bases = bases.clone();
            if !own.is_empty() {
                bases.insert(0, own.into());
            }
            if let Some(image) = os_apps::icon_file(&name, px / 2, 2, &theme, &bases).and_then(|file| os_apps::icon_image(&file, px)) {
                hub.send(Ev::Icon(key, std::sync::Arc::new(image)));
            }
        }
    };
    if let Err(e) = std::thread::Builder::new().name("jai-shell-icons".into()).spawn(work) {
        warn!("[SHELL] No thread for the icons: {e}");
    }
    tx
}

/// Starts a program outside the shell's own service: in a scope of its own, as KDE starts
/// programs, so one that crashes the shell, or the shell's restart, takes no program with it.
fn launch(argv: Vec<String>, terminal: bool) {
    if crate::platform::hands_off() || argv.is_empty() {
        return;
    }
    let argv: Vec<String> = if terminal { ["konsole".to_string(), "-e".to_string()].into_iter().chain(argv).collect() } else { argv };
    let quiet = |command: &mut std::process::Command| {
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if let Ok(home) = std::env::var("HOME") {
            command.current_dir(home);
        }
        command.spawn().map(drop)
    };
    let scoped = quiet(std::process::Command::new("systemd-run").args(["--user", "--scope", "--quiet", "--collect", "--slice=app.slice", "--"]).args(&argv));
    if let Err(e) = scoped.or_else(|_| quiet(std::process::Command::new(&argv[0]).args(&argv[1..]))) {
        warn!("[SHELL] {} did not start: {e}", argv[0]);
    }
}

impl Model {
    fn new(hub: &Hub) -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let user_dirs = std::fs::read_to_string(format!("{home}/.config/user-dirs.dirs")).unwrap_or_default();
        let lang = std::env::var("LANG").unwrap_or_default().split(['_', '.']).next().unwrap_or_default().to_string();
        Self {
            apps: Vec::new(),
            apps_read: None,
            icons: Icons::new(),
            asked: HashSet::new(),
            icon_work: icon_worker(hub.clone()),
            tasks: Vec::new(),
            order: Vec::new(),
            groups: Vec::new(),
            tray: Vec::new(),
            tray_items: Vec::new(),
            state: String::new(),
            clock: Default::default(),
            menu: None,
            pinned: Vec::new(),
            desk_dir: os_shell::desktop_dir(&user_dirs, &home),
            desk: Vec::new(),
            desk_seen: None,
            selected: None,
            notes: Queue::default(),
            osd: None,
            list: None,
            core: Core::new(Pose::Front),
            settings_seen: None,
            lang,
        }
    }

    /// Asks for the picture `key` once: the icon `name` at `px` pixels.
    fn want_icon(&mut self, key: &str, name: &str, px: u32, own: &str) {
        if !name.is_empty() && self.asked.insert(key.to_string()) {
            let _ = self.icon_work.send((key.to_string(), name.to_string(), px, own.to_string()));
        }
    }

    /// The menu's programs, read again when they are older than half a minute.
    fn read_apps(&mut self) {
        if self.apps_read.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.apps = os_apps::load(&data_dirs("applications"), &self.lang);
        self.apps_read = Some(Instant::now());
        for app in self.apps.clone() {
            self.want_icon(&app.id, &app.icon, 48, "");
        }
    }

    /// The open windows from KWin; whether anything the taskbar shows changed.
    fn read_windows(&mut self) -> bool {
        let tasks: Vec<Task> = plasma::windows()
            .into_iter()
            .filter(|w| !w.is(plasma::state::SKIP_TASKBAR))
            .map(|w| {
                let app = os_apps::of_window(&self.apps, &w.app_id, &w.resource_name);
                let key = app.map(|a| a.id.clone()).unwrap_or_else(|| if w.app_id.is_empty() { w.resource_name.clone() } else { w.app_id.clone() });
                Task { active: w.is(plasma::state::ACTIVE), minimised: w.is(plasma::state::MINIMIZED), name: app.map(|a| a.name.clone()).unwrap_or_default(), app: key, uuid: w.uuid, title: w.title }
            })
            .collect();
        if tasks == self.tasks {
            return false;
        }
        self.tasks = tasks;
        self.groups = os_shell::groups(&self.tasks, &mut self.order);
        // (A program the menu does not know may still have an icon by its window's name.)
        for group in self.groups.clone() {
            self.want_icon(&group.app, &group.app.to_lowercase(), 48, "");
        }
        true
    }

    /// The Desktop folder's entries, read when the folder changed; whether they did.
    fn read_desk(&mut self) -> bool {
        let seen = std::fs::metadata(&self.desk_dir).and_then(|m| m.modified()).ok();
        if seen == self.desk_seen && (seen.is_some() || self.desk.is_empty()) {
            return false;
        }
        self.desk_seen = seen;
        let mut entries: Vec<(bool, String, PathBuf)> = std::fs::read_dir(&self.desk_dir).into_iter().flatten().flatten().map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().into_owned(), e.path())).filter(|(_, name, _)| !name.starts_with('.')).collect();
        entries.sort_by_key(|(is_dir, name, _)| (!is_dir, name.to_lowercase()));
        let mut desk = Vec::new();
        for (is_dir, name, path) in entries {
            // A program's entry shows the program's name and icon; a file its kind's.
            let entry = name.strip_suffix(".desktop").and_then(|id| std::fs::read_to_string(&path).ok().and_then(|text| os_apps::parse(id, &text, &self.lang)));
            let (name, icon) = match entry {
                Some(app) => (app.name, app.icon),
                None => (name.clone(), os_shell::file_icon(&name, is_dir).to_string()),
            };
            let key = format!("desk:{icon}");
            self.want_icon(&key, &icon, 96, "");
            desk.push(DeskItem { name, path, icon: key });
        }
        self.selected = None;
        std::mem::replace(&mut self.desk, desk) != self.desk
    }

    /// His settings file, read (never written) when it changed: the theme, the core's style
    /// and the menu's pins. Whether anything was read.
    fn read_settings(&mut self) -> bool {
        let seen = std::fs::metadata(crate::config::CONFIG_FILE).and_then(|m| m.modified()).ok();
        if seen == self.settings_seen {
            return false;
        }
        self.settings_seen = seen;
        let settings: serde_json::Value = std::fs::read_to_string(crate::config::CONFIG_FILE).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let look = &settings["interface"];
        if crate::theme_store::follow(look["hud_palette"].as_str().unwrap_or(crate::holo_theme::DEFAULT_PRESET)) {
            // (A surface is a still picture: the new colours stand at once.)
            crate::holo_theme::finish_fade();
        }
        self.core.style = look["core_style"].as_str().and_then(CoreStyle::of_word).unwrap_or_default();
        self.pinned = settings["os_favourites"].as_array().map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect()).unwrap_or_default();
        true
    }

    fn tray_changed(&mut self, tray: Vec<TrayEntry>) {
        self.tray_items = tray.iter().map(|e| TrayItem { key: format!("tray:{}{}", e.service, e.icon_name), title: e.title.clone(), attention: e.attention }).collect();
        for (entry, item) in tray.iter().zip(self.tray_items.clone()) {
            match &entry.pixmap {
                Some(picture) => drop(self.icons.insert(item.key.clone(), picture.clone())),
                None => {
                    let (key, name, own) = (item.key.clone(), entry.icon_name.clone(), entry.theme_path.clone());
                    self.want_icon(&key, &name, 48, &own);
                }
            }
        }
        self.tray = tray;
    }
}

impl Shell {
    fn surf(&mut self, id: u32) -> Option<&mut Surf> {
        self.surfs.iter_mut().find(|s| s.id == id)
    }

    fn mark(&mut self, kinds: &[Kind]) {
        for s in self.surfs.iter_mut().filter(|s| kinds.contains(&s.kind)) {
            s.dirty = true;
        }
    }

    fn has(&self, kind: Kind) -> bool {
        self.surfs.iter().any(|s| s.kind == kind)
    }

    fn close(&mut self, kind: Kind) {
        self.close_where(|s| s.kind == kind);
    }

    fn close_where(&mut self, gone: impl Fn(&Surf) -> bool) {
        for s in self.surfs.iter().filter(|s| gone(s)) {
            if let Some(viewport) = &s.viewport {
                viewport.destroy();
            }
            if let Some(fractional) = &s.fractional {
                fractional.destroy();
            }
            s.layer.destroy();
            s.surface.destroy();
            if self.pointer_on == Some(s.id) {
                self.pointer_on = None;
            }
            if self.keys_on == Some(s.id) {
                self.keys_on = None;
            }
        }
        self.surfs.retain(|s| !gone(s));
    }

    /// The monitor things open on: the one the pointer was last on, else the first.
    fn front_output(&self) -> Option<u32> {
        self.last_output.filter(|name| self.outputs.iter().any(|o| o.name == *name)).or(self.outputs.first().map(|o| o.name))
    }

    /// Opens a surface of `kind` on the monitor `output`, `size` points large (0: as far as
    /// its anchors reach), `right` points in from the right edge where that matters.
    fn open(&mut self, kind: Kind, output: u32, size: (u32, u32), right: i32) {
        let (Some(compositor), Some((layer_shell, version))) = (&self.compositor, &self.layer_shell) else { return };
        let Some(monitor) = self.outputs.iter().find(|o| o.name == output) else { return };
        let id = self.next_id;
        self.next_id += 1;
        let surface = compositor.create_surface(&self.qh, ());
        let (layer, namespace) = match kind {
            Kind::Desktop => (Layer::Background, "jai-desktop"),
            Kind::Core => (Layer::Bottom, "jai-core"),
            Kind::Bar => (Layer::Top, "jai-taskbar"),
            Kind::Menu => (Layer::Top, "jai-menu"),
            Kind::List => (Layer::Overlay, "jai-tray-menu"),
            Kind::Notes => (Layer::Overlay, "jai-notifications"),
            Kind::Osd => (Layer::Overlay, "jai-osd"),
        };
        let layer = layer_shell.get_layer_surface(&surface, Some(&monitor.output), layer, namespace.into(), &self.qh, id);
        layer.set_size(size.0, size.1);
        match kind {
            // (Zone 0: KWin keeps the desktop clear of the taskbar's zone, so no icon lies under it.)
            Kind::Desktop => layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right),
            Kind::Core => {}
            Kind::Bar => {
                layer.set_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right);
                layer.set_exclusive_zone(os_shell::BAR_H as i32);
            }
            Kind::Menu => {
                layer.set_anchor(Anchor::Bottom | Anchor::Left);
                layer.set_margin(0, 0, 6, 6);
                // The search line needs the keyboard; from version 4 a click elsewhere takes it
                // back (and the menu closes), before that the menu holds it until Escape.
                layer.set_keyboard_interactivity(if *version >= 4 { KeyboardInteractivity::OnDemand } else { KeyboardInteractivity::Exclusive });
            }
            Kind::List => {
                layer.set_anchor(Anchor::Bottom | Anchor::Right);
                layer.set_margin(0, right, 4, 0);
            }
            Kind::Notes => {
                layer.set_anchor(Anchor::Top | Anchor::Right);
                layer.set_margin(14, 14, 0, 0);
            }
            Kind::Osd => {
                layer.set_anchor(Anchor::Bottom);
                layer.set_margin(0, 0, 90, 0);
            }
        }
        if matches!(kind, Kind::Core | Kind::Osd) {
            // Nothing to click on these: the pointer goes through to what lies under them.
            let nothing = compositor.create_region(&self.qh, ());
            surface.set_input_region(Some(&nothing));
            nothing.destroy();
        }
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, &self.qh, ()));
        let fractional = self.fractional.as_ref().filter(|_| viewport.is_some()).map(|f| f.get_fractional_scale(&surface, &self.qh, id));
        surface.commit();
        let scale120 = monitor.scale.max(1) as u32 * 120;
        self.surfs.push(Surf { id, kind, output, surface, layer, viewport, fractional, scale120, size: (0, 0), snap: None, events: Vec::new(), pointer: pos2(-1.0, -1.0), dirty: true, waiting: None, next: None, drawn: false });
    }

    /// A monitor is described: its desktop, core and taskbar open.
    fn output_ready(&mut self, name: u32) {
        let Some(output) = self.outputs.iter_mut().find(|o| o.name == name && !o.used) else { return };
        output.used = true;
        info!("[SHELL] A monitor: its desktop and taskbar open");
        self.open(Kind::Desktop, name, (0, 0), 0);
        self.open(Kind::Bar, name, (0, os_shell::BAR_H as u32), 0);
    }

    fn output_gone(&mut self, name: u32) {
        if let Some(at) = self.outputs.iter().position(|o| o.name == name) {
            let gone = self.outputs.remove(at);
            if gone.output.version() >= 3 {
                gone.output.release();
            }
            self.close_where(|s| s.output == name);
            info!("[SHELL] A monitor left: its desktop and taskbar close");
        }
    }

    fn toggle_menu(&mut self, output: Option<u32>) {
        if self.model.menu.take().is_some() {
            self.close(Kind::Menu);
        } else if let Some(output) = output.or(self.front_output()) {
            self.model.read_apps();
            self.model.menu = Some(Menu::default());
            self.open(Kind::Menu, output, (os_shell::MENU_SIZE.x as u32, os_shell::MENU_SIZE.y as u32), 0);
        }
        self.mark(&[Kind::Bar]);
    }

    fn close_menu(&mut self) {
        if self.model.menu.take().is_some() {
            self.close(Kind::Menu);
            self.mark(&[Kind::Bar]);
        }
    }

    fn close_list(&mut self) {
        if self.model.list.take().is_some() {
            self.close(Kind::List);
        }
    }

    /// The cards' surface follows the queue: it opens with the first card, grows and shrinks
    /// with the stack and closes with the last.
    fn sync_notes(&mut self) {
        let shown = self.model.notes.shown();
        let size = os_notes::stack(&shown).1;
        let size = (size.x as u32, size.y.ceil() as u32);
        if shown.is_empty() {
            return self.close(Kind::Notes);
        }
        for note in self.model.notes.notes.clone() {
            self.model.want_icon(&note.icon, &note.icon, 32, "");
        }
        match self.surfs.iter_mut().find(|s| s.kind == Kind::Notes) {
            Some(s) if s.size == size || s.size == (0, 0) => s.dirty = true,
            Some(s) => {
                s.layer.set_size(size.0, size.1);
                s.surface.commit();
            }
            None => {
                if let Some(output) = self.front_output() {
                    self.open(Kind::Notes, output, size, 0);
                }
            }
        }
    }

    fn on_event(&mut self, ev: Ev) {
        match ev {
            Ev::ToggleMenu => self.toggle_menu(None),
            Ev::Note(mut note, timeout, urgency) => {
                note.until = os_notes::until(timeout, urgency, self.start.elapsed().as_secs_f64());
                self.model.notes.put(note);
                self.sync_notes();
            }
            Ev::CloseNote(id) => {
                if self.model.notes.close(id) {
                    os_bus::note_closed(id, os_notes::CLOSED);
                    self.sync_notes();
                }
            }
            Ev::Osd(osd) => {
                self.model.osd = Some((osd, Instant::now() + Duration::from_secs_f64(os_notes::OSD_SECS)));
                if self.has(Kind::Osd) {
                    self.mark(&[Kind::Osd]);
                } else if let Some(output) = self.front_output() {
                    self.open(Kind::Osd, output, (os_notes::OSD_SIZE.x as u32, os_notes::OSD_SIZE.y as u32), 0);
                }
            }
            Ev::Tray(tray) => {
                self.model.tray_changed(tray);
                self.mark(&[Kind::Bar]);
            }
            Ev::State(state) => {
                self.model.state = state;
                self.mark(&[Kind::Bar, Kind::Core]);
            }
            Ev::TrayMenu(service, menu, rows) => {
                self.close_list();
                let Some(output) = self.front_output() else { return };
                // Above its item: as far from the right edge as the item is.
                let bar = self.surfs.iter().find(|s| s.kind == Kind::Bar && s.output == output).map_or(0.0, |s| s.size.0 as f32);
                let layout = os_shell::bar_layout(bar, self.model.groups.len(), self.model.tray.len());
                let item = self.model.tray.iter().position(|e| e.service == service).and_then(|i| layout.tray.get(i).copied());
                let size = os_shell::list_size(&rows);
                let right = item.map_or(8.0, |r| (bar - r.right() - 4.0).max(4.0));
                self.model.list = Some((service, menu, rows, Instant::now() + Duration::from_secs(8)));
                self.open(Kind::List, output, (size.x as u32, size.y.ceil() as u32), right as i32);
            }
            Ev::Icon(key, image) => {
                self.model.icons.insert(key, image);
                self.mark(&[Kind::Bar, Kind::Menu, Kind::Desktop, Kind::Notes]);
            }
        }
    }

    /// What does not come as an event: the windows, the clock, the folder, the settings, the
    /// timers. Then every surface that needs it is drawn.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.ticked).as_secs_f32();
        self.ticked = now;
        if now.duration_since(self.polled) >= Duration::from_millis(200) {
            // shortcut: KWin's window list is asked five times a second; have `plasma` call
            // back on a change when a fifth of a second on the taskbar shows.
            self.polled = now;
            if self.model.read_windows() {
                self.mark(&[Kind::Bar]);
            }
            let time = chrono::Local::now();
            let clock = (time.format("%H:%M").to_string(), time.format("%a %d %b").to_string());
            if clock != self.model.clock {
                self.model.clock = clock;
                self.mark(&[Kind::Bar]);
                // (Once a minute is often enough for what Sir changes by hand.)
                if self.model.read_desk() {
                    self.mark(&[Kind::Desktop]);
                }
            }
            if self.model.read_settings() {
                self.mark(&[Kind::Desktop, Kind::Core, Kind::Bar, Kind::Menu, Kind::Notes, Kind::Osd, Kind::List]);
            }
        }
        // His core: its clock ring steps once a second; it moves while he is busy.
        let (mood, busy) = match self.model.state.as_str() {
            "listening" => (Mood::Listening, false),
            "thinking" => (Mood::Thinking, true),
            "speaking" => (Mood::Speaking, false),
            _ => (Mood::Rest, false),
        };
        self.model.core.set_mood(mood);
        self.model.core.set_pose(if busy { Pose::Gyro } else { Pose::Front }, busy);
        let day = chrono::Timelike::num_seconds_from_midnight(&chrono::Local::now()) as f32;
        self.model.core.step(dt, &Feed { voice: if mood == Mood::Speaking { 0.55 } else { 0.0 }, heard: if mood == Mood::Listening { 0.45 } else { 0.0 }, busy, asleep: self.model.state.is_empty(), day_secs: day, ..Default::default() });
        let core_every = Duration::from_millis(if mood != Mood::Rest || self.model.core.is_animating() { 66 } else { 1000 });
        for s in self.surfs.iter_mut().filter(|s| s.kind == Kind::Core) {
            s.next = Some(s.next.map_or(now + core_every, |at| at.min(now + core_every)));
        }

        let gone = self.model.notes.expire(self.start.elapsed().as_secs_f64());
        if !gone.is_empty() {
            gone.iter().for_each(|id| os_bus::note_closed(*id, os_notes::EXPIRED));
            self.sync_notes();
        }
        if self.model.osd.as_ref().is_some_and(|(_, until)| *until <= now) {
            self.model.osd = None;
            self.close(Kind::Osd);
        }
        if self.model.list.as_ref().is_some_and(|(.., until)| *until <= now) && self.pointer_on.and_then(|id| self.surfs.iter().find(|s| s.id == id)).is_none_or(|s| s.kind != Kind::List) {
            self.close_list();
        }
        if let (Some((key, at)), Some(keys_on)) = (self.repeat, self.keys_on) {
            if at <= now {
                self.repeat = Some((key, now + Duration::from_millis(40)));
                self.key(keys_on, key, true, true);
            }
        }
        for at in 0..self.surfs.len() {
            // (A click's action may have closed a surface while this loop runs.)
            let Some(s) = self.surfs.get(at) else { break };
            let due = s.dirty || s.next.is_some_and(|at| at <= now);
            // (KWin calls back when the last picture was shown; a surface nobody sees is not
            // called and so not drawn. A call that stays out for a second is not waited for.)
            let free = s.waiting.is_none_or(|since| now.duration_since(since) > Duration::from_secs(1));
            if due && free && s.snap.is_some() {
                self.draw(at);
            }
        }
    }

    /// How long the loop may sleep: until the next thing that happens by the clock.
    fn sleep_ms(&self) -> i32 {
        let now = Instant::now();
        let mut until = now + Duration::from_millis(200);
        for at in self.surfs.iter().filter_map(|s| if s.dirty { Some(now + Duration::from_millis(16)) } else { s.next }).chain(self.repeat.map(|(_, at)| at)).chain(self.model.osd.as_ref().map(|(_, at)| *at)).chain(self.model.notes.next_expiry().map(|secs| self.start + Duration::from_secs_f64(secs.max(0.0)))) {
            until = until.min(at);
        }
        until.saturating_duration_since(now).as_millis().max(1) as i32
    }

    /// One egui frame of the surface, rasterised into a buffer and handed to KWin.
    fn draw(&mut self, at: usize) {
        let Shell { surfs, model, modifiers, start, .. } = self;
        let s = &mut surfs[at];
        let Some(snap) = s.snap.as_mut() else { return };
        let ppp = s.scale120 as f32 / 120.0;
        snap.ctx.set_pixels_per_point(ppp);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(s.size.0 as f32, s.size.1 as f32));
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(start.elapsed().as_secs_f64()), predicted_dt: 1.0 / 30.0, events: std::mem::take(&mut s.events), modifiers: *modifiers, focused: true, ..Default::default() };
        let mut did = Did::None;
        let (kind, output) = (s.kind, s.output);
        let frame = snap.frame(input, |ctx| {
            crate::gui::apply_hud_visuals(ctx);
            match kind {
                Kind::Desktop => {
                    if let Some(opened) = os_shell::paint_desktop(ctx, &model.desk, &model.icons, &mut model.selected) {
                        did = Did::Open(opened);
                    }
                }
                Kind::Core => os_shell::paint_core(ctx, &model.core),
                Kind::Bar => {
                    let bar = Bar { groups: &model.groups, tray: &model.tray_items, icons: &model.icons, state: &model.state, clock: (&model.clock.0, &model.clock.1), menu_open: model.menu.is_some() };
                    if let Some(action) = os_shell::paint_bar(ctx, &bar) {
                        did = Did::Bar(action, output);
                    }
                }
                Kind::Menu => {
                    if let Some(menu) = model.menu.as_mut() {
                        if let Some(action) = os_shell::paint_menu(ctx, menu, &model.apps, &model.pinned, &model.icons) {
                            did = Did::Menu(action);
                        }
                    }
                }
                Kind::List => {
                    if let Some(picked) = model.list.as_ref().and_then(|(_, _, rows, _)| os_shell::paint_list(ctx, rows)) {
                        did = Did::Pick(picked);
                    }
                }
                Kind::Notes => {
                    if let Some(action) = os_notes::paint_notes(ctx, &model.notes.shown(), &model.icons) {
                        did = Did::Note(action);
                    }
                }
                Kind::Osd => {
                    if let Some((osd, _)) = &model.osd {
                        os_notes::paint_osd(ctx, osd);
                    }
                }
            }
            crate::gui::settle_new_areas(ctx);
        });
        let now = Instant::now();
        s.dirty = false;
        // (The core's pace is set by `tick`, whatever its painters ask for.)
        s.next = frame.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).filter(|d| *d < Duration::from_secs(5) && kind != Kind::Core).map(|d| now + d.max(Duration::from_millis(33)));
        let canvas = snap.rasterise(frame.shapes, frame.pixels_per_point);
        if let Some(buffer) = self.buffer(&canvas) {
            let s = &mut self.surfs[at];
            s.surface.attach(Some(&buffer), 0, 0);
            let (w, h) = os_shell::buffer_size(s.size, s.scale120);
            s.surface.damage_buffer(0, 0, w as i32, h as i32);
            match &s.viewport {
                Some(viewport) => viewport.set_destination(s.size.0 as i32, s.size.1 as i32),
                None => s.surface.set_buffer_scale((s.scale120 / 120).max(1) as i32),
            }
            s.surface.frame(&self.qh, s.id);
            s.surface.commit();
            if kind == Kind::Bar && !s.drawn && !std::mem::replace(&mut self.stands, true) {
                os_bus::desktop_stands();
            }
            (s.waiting, s.drawn) = (Some(now), true);
        }
        self.act(did);
    }

    /// The picture as a shared-memory buffer (a file in memory, mapped, filled and handed
    /// over; KWin keeps its own mapping, and the buffer is destroyed when KWin lets go of it).
    fn buffer(&self, canvas: &crate::ui_snapshot::Canvas) -> Option<wl_buffer::WlBuffer> {
        let shm = self.shm.as_ref()?;
        let (w, h) = (canvas.width as i32, canvas.height as i32);
        let len = canvas.width * canvas.height * 4;
        if len == 0 {
            return None;
        }
        // SAFETY: memfd_create returns a fresh descriptor this function owns (closed when
        // `fd` drops); the mapping is exactly `len` bytes of it, written through one slice
        // and unmapped before returning. Failures are checked before the next step.
        unsafe {
            let raw = libc::memfd_create(c"jai-shell".as_ptr(), libc::MFD_CLOEXEC);
            if raw < 0 {
                return None;
            }
            let fd = OwnedFd::from_raw_fd(raw);
            if libc::ftruncate(raw, len as libc::off_t) != 0 {
                return None;
            }
            let map = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, raw, 0);
            if map == libc::MAP_FAILED {
                return None;
            }
            canvas.write_argb(std::slice::from_raw_parts_mut(map.cast::<u8>(), len));
            libc::munmap(map, len);
            let pool = shm.create_pool(fd.as_fd(), len as i32, &self.qh, ());
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &self.qh, ());
            pool.destroy();
            Some(buffer)
        }
    }

    /// What a click on a surface asked for.
    fn act(&mut self, did: Did) {
        match did {
            Did::None => {}
            Did::Open(item) => {
                let Some(item) = self.model.desk.get(item) else { return };
                let entry = item.path.extension().is_some_and(|e| e == "desktop").then(|| std::fs::read_to_string(&item.path).ok().and_then(|text| os_apps::parse("", &text, ""))).flatten();
                match entry {
                    Some(app) => launch(os_apps::command(&app.exec), app.terminal),
                    None => launch(vec!["xdg-open".into(), item.path.to_string_lossy().into_owned()], false),
                }
            }
            Did::Bar(action, output) => match action {
                BarAction::Menu => self.toggle_menu(Some(output)),
                BarAction::Activate(uuid) => drop(plasma::set_state(&uuid, plasma::state::ACTIVE, true)),
                BarAction::Minimise(uuid) => drop(plasma::set_state(&uuid, plasma::state::MINIMIZED, true)),
                BarAction::Launch(app) => {
                    if let Some(app) = self.model.apps.iter().find(|a| a.id == app) {
                        launch(os_apps::command(&app.exec), app.terminal);
                    }
                }
                BarAction::Tray(item, click) => {
                    if let Some(entry) = self.model.tray.get(item) {
                        os_bus::tray_click(&self.hub, entry, click);
                    }
                }
                BarAction::Lamp => os_bus::show_hud(),
            },
            Did::Menu(action) => {
                match action {
                    MenuAction::Launch(id) => {
                        if let Some(app) = self.model.apps.iter().find(|a| a.id == id) {
                            launch(os_apps::command(&app.exec), app.terminal);
                        }
                    }
                    MenuAction::Order(text) => os_bus::order(text),
                    MenuAction::Power(power) => os_bus::power(power),
                    MenuAction::Pin(id, on) => {
                        // (Shown at once; the assistant writes it into his settings.)
                        self.model.pinned = os_shell::pinned_after(&self.model.apps, &self.model.pinned, &id, on);
                        os_bus::keep_favourites(self.model.pinned.clone());
                        return self.mark(&[Kind::Menu]);
                    }
                    MenuAction::Close => {}
                }
                self.close_menu();
            }
            Did::Pick(id) => {
                if let Some((service, menu, ..)) = self.model.list.take() {
                    os_bus::menu_pick(service, menu, id);
                }
                self.close(Kind::List);
            }
            Did::Note(action) => {
                let id = match action {
                    NoteAction::Dismiss(id) => id,
                    NoteAction::Invoke(id, key) => {
                        os_bus::action_invoked(id, &key);
                        id
                    }
                };
                if self.model.notes.close(id) {
                    os_bus::note_closed(id, os_notes::DISMISSED);
                }
                self.sync_notes();
            }
        }
    }

    /// A key for the surface that has the keyboard.
    fn key(&mut self, surf: u32, key: u32, pressed: bool, repeat: bool) {
        let Some((sym, text)) = self.xkb.as_ref().map(|xkb| xkb.key(key)) else { return };
        let modifiers = self.modifiers;
        let Some(s) = self.surf(surf) else { return };
        if let Some(key) = egui_key(sym) {
            s.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat, modifiers });
        }
        if pressed && !text.is_empty() && !text.chars().any(char::is_control) && !modifiers.ctrl && !modifiers.alt {
            s.events.push(egui::Event::Text(text));
        }
        s.dirty = true;
    }
}

/// What a frame's clicks asked for (acted on once the frame is handed over).
enum Did {
    None,
    Open(usize),
    Bar(BarAction, u32),
    Menu(MenuAction),
    Pick(i32),
    Note(NoteAction),
}

impl Dispatch<wl_registry::WlRegistry, ()> for Shell {
    fn event(shell: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } => match interface.as_str() {
                "wl_compositor" => shell.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_shm" => shell.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => shell.layer_shell = Some((registry.bind(name, version.min(4), qh, ()), version.min(4))),
                "wp_viewporter" => shell.viewporter = Some(registry.bind(name, 1, qh, ())),
                "wp_fractional_scale_manager_v1" => shell.fractional = Some(registry.bind(name, 1, qh, ())),
                "wp_cursor_shape_manager_v1" => shell.cursors = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" if shell.seat.is_none() => shell.seat = Some(registry.bind(name, version.min(5), qh, ())),
                "wl_output" => shell.outputs.push(Output { name, output: registry.bind(name, version.min(4), qh, name), scale: 1, used: false }),
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => shell.output_gone(name),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for Shell {
    fn event(shell: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event, name: &u32, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_output::Event::Scale { factor } => {
                if let Some(output) = shell.outputs.iter_mut().find(|o| o.name == *name) {
                    output.scale = factor;
                }
                // (Without the fractional-scale protocol the monitor's whole scale is the surface's.)
                for s in shell.surfs.iter_mut().filter(|s| s.output == *name && s.fractional.is_none()) {
                    (s.scale120, s.dirty) = (factor.max(1) as u32 * 120, true);
                }
            }
            wl_output::Event::Done => shell.output_ready(*name),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for Shell {
    fn event(shell: &mut Self, layer: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, id: &u32, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                let Some(s) = shell.surf(*id) else { return };
                if (width, height) != s.size && width > 0 && height > 0 {
                    s.size = (width, height);
                    let mut snap = Snapshot::new(vec2(width as f32, height as f32));
                    // What is not a full rectangle of his black lets the desktop through.
                    if matches!(s.kind, Kind::Menu | Kind::List | Kind::Notes | Kind::Osd) {
                        snap.clear = [0.0; 4];
                    }
                    s.snap = Some(snap);
                }
                (s.dirty, s.waiting) = (true, None);
                let (kind, output, size) = (s.kind, s.output, s.size);
                // The desktop knows its size now: his core stands over its middle.
                if kind == Kind::Desktop && !shell.surfs.iter().any(|s| s.kind == Kind::Core && s.output == output) {
                    let side = os_shell::core_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(size.0 as f32, size.1 as f32))).width() as u32;
                    shell.open(Kind::Core, output, (side, side), 0);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                let id = *id;
                match shell.surf(id).map(|s| s.kind) {
                    Some(Kind::Menu) => shell.close_menu(),
                    Some(Kind::List) => shell.close_list(),
                    _ => {}
                }
                shell.close_where(|s| s.id == id);
            }
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, u32> for Shell {
    fn event(shell: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, id: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let (wp_fractional_scale_v1::Event::PreferredScale { scale }, Some(s)) = (event, shell.surf(*id)) {
            (s.scale120, s.dirty) = (scale.max(60), true);
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Shell {
    fn event(shell: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, id: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let Some(s) = shell.surf(*id) {
            s.waiting = None;
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Shell {
    fn event(_: &mut Self, buffer: &wl_buffer::WlBuffer, event: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Shell {
    fn event(shell: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(has) } = event else { return };
        if has.contains(wl_seat::Capability::Pointer) && shell.pointer.is_none() {
            let pointer = seat.get_pointer(qh, ());
            let shape = shell.cursors.as_ref().map(|c| c.get_pointer(&pointer, qh, ()));
            shell.pointer = Some((pointer, shape));
        }
        if has.contains(wl_seat::Capability::Keyboard) && shell.keyboard.is_none() {
            shell.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Shell {
    fn event(shell: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let modifiers = shell.modifiers;
        // A move replaces the move before it: only where the pointer is now matters.
        let moved = |s: &mut Surf, x: f64, y: f64| {
            s.pointer = pos2(x as f32, y as f32);
            if matches!(s.events.last(), Some(egui::Event::PointerMoved(_))) {
                s.events.pop();
            }
            s.events.push(egui::Event::PointerMoved(s.pointer));
            // (The desktop has nothing that lights under the pointer: it is not drawn for a move.)
            s.dirty |= !matches!(s.kind, Kind::Desktop);
        };
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                if let Some((_, Some(shape))) = &shell.pointer {
                    shape.set_shape(serial, Shape::Default);
                }
                if let Some(s) = shell.surfs.iter_mut().find(|s| s.surface == surface) {
                    moved(s, surface_x, surface_y);
                    (shell.pointer_on, shell.last_output) = (Some(s.id), Some(s.output));
                }
            }
            wl_pointer::Event::Leave { .. } => {
                if let Some(s) = shell.pointer_on.take().and_then(|id| shell.surf(id)) {
                    s.events.push(egui::Event::PointerGone);
                    s.dirty = true;
                    if s.kind == Kind::List {
                        shell.close_list();
                    }
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some(s) = shell.pointer_on.and_then(|id| shell.surf(id)) {
                    moved(s, surface_x, surface_y);
                }
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let button = match button {
                    0x110 => egui::PointerButton::Primary,
                    0x111 => egui::PointerButton::Secondary,
                    0x112 => egui::PointerButton::Middle,
                    _ => return,
                };
                let pressed = state == WEnum::Value(wl_pointer::ButtonState::Pressed);
                let Some(s) = shell.pointer_on.and_then(|id| shell.surf(id)) else { return };
                s.events.push(egui::Event::PointerButton { pos: s.pointer, button, pressed, modifiers });
                s.dirty = true;
                // A press anywhere else on his surfaces puts the menus away ([J] itself toggles).
                let kind = s.kind;
                if pressed && !matches!(kind, Kind::Menu | Kind::Bar) {
                    shell.close_menu();
                }
                if pressed && kind != Kind::List {
                    shell.close_list();
                }
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                let Some(s) = shell.pointer_on.and_then(|id| shell.surf(id)) else { return };
                let delta = if axis == WEnum::Value(wl_pointer::Axis::VerticalScroll) { vec2(0.0, -value as f32) } else { vec2(-value as f32, 0.0) };
                s.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, modifiers });
                s.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Shell {
    fn event(shell: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                if let Some(xkb) = shell.xkb.as_mut() {
                    xkb.load(fd, size);
                }
            }
            wl_keyboard::Event::Enter { surface, .. } => shell.keys_on = shell.surfs.iter().find(|s| s.surface == surface).map(|s| s.id),
            wl_keyboard::Event::Leave { .. } => {
                // The keyboard went to another window: Sir clicked away from the menu.
                (shell.keys_on, shell.repeat) = (None, None);
                shell.close_menu();
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(xkb) = shell.xkb.as_mut() {
                    xkb.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
                // (The core modifiers' bits: Shift 1, Control 4, Alt 8.)
                shell.modifiers = egui::Modifiers { shift: mods_depressed & 1 != 0, ctrl: mods_depressed & 4 != 0, command: mods_depressed & 4 != 0, alt: mods_depressed & 8 != 0, mac_cmd: false };
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                shell.repeat = pressed.then(|| (key, Instant::now() + Duration::from_millis(420)));
                if let Some(on) = shell.keys_on {
                    shell.key(on, key, pressed, false);
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(Shell: wl_compositor::WlCompositor);
delegate_noop!(Shell: wl_shm_pool::WlShmPool);
delegate_noop!(Shell: wl_region::WlRegion);
delegate_noop!(Shell: ZwlrLayerShellV1);
delegate_noop!(Shell: WpViewporter);
delegate_noop!(Shell: WpViewport);
delegate_noop!(Shell: WpFractionalScaleManagerV1);
delegate_noop!(Shell: WpCursorShapeManagerV1);
delegate_noop!(Shell: WpCursorShapeDeviceV1);
delegate_noop!(Shell: ignore wl_shm::WlShm);
delegate_noop!(Shell: ignore wl_surface::WlSurface);

/// Connects to KWin and the session bus and runs the shell until `done` says so or the
/// compositor is gone (Err).
fn run_until(done: impl Fn(&Shell) -> bool) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland compositor to draw on: {e}"))?;
    let (hub, events, wake) = os_bus::start()?;
    let mut queue = conn.new_event_queue::<Shell>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let now = Instant::now();
    let model = Model::new(&hub);
    let mut shell = Shell {
        qh,
        hub,
        compositor: None,
        shm: None,
        layer_shell: None,
        viewporter: None,
        fractional: None,
        cursors: None,
        seat: None,
        pointer: None,
        keyboard: None,
        outputs: Vec::new(),
        surfs: Vec::new(),
        next_id: 1,
        pointer_on: None,
        keys_on: None,
        last_output: None,
        xkb: Xkb::new(),
        modifiers: egui::Modifiers::default(),
        repeat: None,
        start: now,
        ticked: now,
        polled: now - Duration::from_secs(1),
        stands: false,
        model,
    };
    queue.roundtrip(&mut shell).map_err(|e| e.to_string())?;
    if shell.layer_shell.is_none() || shell.compositor.is_none() || shell.shm.is_none() {
        return Err("this compositor has no layer shell (zwlr_layer_shell_v1): the shell needs KWin".into());
    }
    shell.model.read_settings();
    shell.model.read_apps();
    shell.model.read_desk();
    info!("[SHELL] J.A.I.OS's shell is up: {} programs in the menu, {} icons on the desktop", shell.model.apps.len(), shell.model.desk.len());
    while !done(&shell) {
        queue.dispatch_pending(&mut shell).map_err(|e| e.to_string())?;
        while let Ok(ev) = events.try_recv() {
            shell.on_event(ev);
        }
        shell.tick();
        conn.flush().map_err(|e| e.to_string())?;
        let Some(reading) = queue.prepare_read() else { continue };
        let mut fds = [libc::pollfd { fd: reading.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 }];
        // SAFETY: poll reads and writes the two entries of the array it is given; the wake
        // pipe's bytes are drained into a buffer of the length passed.
        unsafe {
            libc::poll(fds.as_mut_ptr(), 2, shell.sleep_ms());
            if fds[1].revents != 0 {
                let mut bytes = [0u8; 64];
                while libc::read(wake, bytes.as_mut_ptr().cast(), bytes.len()) > 0 {}
            }
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err("the compositor is gone".into());
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if let Err(wayland_client::backend::WaylandError::Protocol(e)) = reading.read() {
                return Err(e.to_string());
            }
        }
    }
    Ok(())
}

/// `jai --shell`. It ends with an error when it cannot draw; systemd starts it again, and
/// after three failures in a minute opens KDE's own desktop as safe mode.
pub fn run() -> ExitCode {
    let _ = tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).try_init();
    // SAFETY: sets one signal's disposition before any thread of the shell starts a program:
    // the programs it starts are not waited for, so the kernel reaps them.
    unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    match run_until(|_| false) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("J.A.I.OS's shell stops: {e}");
            ExitCode::FAILURE
        }
    }
}

/// For the check against a real KWin (`live_checks::kde`): the shell runs until every monitor
/// shows its desktop, core and taskbar, at most `secs` seconds; how many surfaces were drawn.
#[cfg(test)]
pub fn check(secs: u64) -> Result<usize, String> {
    let (end, drawn) = (Instant::now() + Duration::from_secs(secs), std::cell::Cell::new(0));
    run_until(|shell| {
        let shown = |kind: Kind, output: u32| shell.surfs.iter().any(|s| s.kind == kind && s.output == output && s.drawn);
        drawn.set(shell.surfs.iter().filter(|s| s.drawn).count());
        Instant::now() > end || (!shell.outputs.is_empty() && shell.outputs.iter().all(|o| [Kind::Desktop, Kind::Core, Kind::Bar].iter().all(|kind| shown(*kind, o.name))))
    })?;
    if Instant::now() > end {
        return Err(format!("after {secs} s only {} surfaces were drawn", drawn.get()));
    }
    Ok(drawn.get())
}
