//! Native HUD window: egui drawn with OpenGL (eframe/glow) straight into a desktop window — no
//! browser, no webview, no console. It lives on the main thread for the whole session; closing
//! it hides it to the tray, and JAI restyles it live when Sir asks (`InterfaceSettings`).
//!
//! Backend events are applied to a shared `HudModel` by a background task, so nothing is missed
//! while the window is hidden; the window drives `app::JAI` with `HudCommand`s.
//!
//! The window is the HUD board: an obsidian canvas with the gyroscopic ring emblem
//! (`rose_emblem_renderer`), the board of sharp technical blocks (`hud_board`), a micro header
//! strip and a micro telemetry strip. Every exchange appears in the `COMMS // LIVE RESPONSE` block
//! above the strip; the header's `[ CONSOLE ]` (Ctrl+L, "open text interface") slides
//! out the text console with the full transcript and an input line. JAI's live widgets,
//! confirmations and message cards render as blocks in the left system column; his extra native
//! windows are egui deferred viewports (`widget_view`). The board is always the desktop's
//! background (`wallpaper`); the floating orb and mini mode's strip are windows of their own.

#[path = "gui_widgets.rs"]
mod widget_view;

#[path = "gui_integrations.rs"]
mod integrations;

#[path = "gui_simlab.rs"]
mod simlab_view;

#[path = "gui_pairing.rs"]
mod pairing_view;

#[path = "gui_features.rs"]
mod features_view;

#[path = "gui_theme_studio.rs"]
mod theme_studio;
/// Every switch in one searchable panel.
#[path = "gui_settings.rs"]
mod settings_panel;

#[path = "gui_mixer.rs"]
mod mixer_view;
/// His keys in every program, each changed by pressing the new key.
#[path = "gui_hotkeys.rs"]
mod hotkeys_view;

#[path = "gui_accounts.rs"]
mod accounts_view;
/// Sir's admin window and the pop-up for his admin sign-in.
#[path = "gui_admin.rs"]
mod admin_view;

#[path = "gui_login.rs"]
mod login_view;

#[path = "gui_network_radar.rs"]
mod radar_view;

#[path = "gui_disk_map.rs"]
mod disk_map_view;

#[path = "gui_earth.rs"]
mod earth_view;
// live layers (C1): the departures board.
#[path = "gui_departures.rs"]
mod departures_view;

/// Pictures, live cameras and street view.
#[path = "gui_media.rs"]
mod media_view;

#[path = "gui_discord.rs"]
mod discord_view;

#[path = "gui_workout.rs"]
mod workout_view;

#[path = "gui_model3d.rs"]
mod model3d_view;
/// The HUD drawn without a window (the live self-test and the exhaustive verifier).
#[path = "gui_offscreen.rs"]
pub(crate) mod offscreen;

use crate::app::Jai;
use crate::holo::{self, Face};
use crate::holo_core::{Core, CoreInputs};
use crate::{holo_kit, holo_ui};
use crate::config::ConfigStore;
use crate::ears::AudioLevel;
use crate::hud::{ConfigState, HudCommand, HudEvent};
use crate::interface::{self, HudLayout, InterfaceSettings, WindowCommand};
use crate::memory::MemoryEntry;
use crate::modality::VoiceResponseMode;
use crate::skills::JaiSkill;
use crate::telemetry::SystemTelemetry;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontFamily, FontId, Frame, Key, Layout, Margin,
    Pos2, Rect, RichText, Rounding, Sense, Shape, Stroke, Ui, Vec2, ViewportCommand, ViewportId,
};
use std::collections::VecDeque;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tracing::{info, warn};
// The calls on his own windows: Win32 on Windows, X11 (XWayland under KDE Plasma) on Linux.
#[cfg(windows)]
use crate::{win32 as wm, win32_desktop as desk};
#[cfg(not(windows))]
use crate::{x11 as wm, x11 as desk};

const WINDOW_TITLE: &str = "J.A.I. HUD";
const MAX_MESSAGES: usize = 200;
const MAX_TOOL_LINES: usize = 60;
const MEMORY_CATEGORIES: [&str; 6] = ["preference", "personal", "project", "schedule", "system", "general"];
// ------------------------------------------------------------------- process-wide handle ----

enum Wake {
    Open,
    Quit,
}

/// Lets the tray, voice commands and a second `JAI.exe` reach the window from any thread.
struct Shared {
    wake: Mutex<Option<mpsc::Sender<Wake>>>,
    /// egui context while a window exists (repaints, viewport commands).
    ctx: Mutex<Option<egui::Context>>,
    /// Native window handle while a window exists (Windows).
    hwnd: AtomicIsize,
    /// Window actions requested before the window existed.
    pending: Mutex<Vec<WindowCommand>>,
    running: AtomicBool,
    quitting: AtomicBool,
}

static SHARED: Shared = Shared {
    wake: Mutex::new(None),
    ctx: Mutex::new(None),
    hwnd: AtomicIsize::new(0),
    pending: Mutex::new(Vec::new()),
    running: AtomicBool::new(false),
    quitting: AtomicBool::new(false),
};

/// Frames the main HUD has drawn (native windows are created and closed during its frames).
static ROOT_FRAMES: AtomicU64 = AtomicU64::new(0);
static WELCOME_STARTED: Mutex<Option<Instant>> = Mutex::new(None);

/// Cinematic energy is driven by the welcome protocol, independently of TTS volume.
pub fn welcome_energy() {
    *lock(&WELCOME_STARTED) = Some(Instant::now());
    request_repaint();
}

fn welcome_level() -> f32 {
    lock(&WELCOME_STARTED).map(|at| (1.0 - at.elapsed().as_secs_f32() / 8.0).clamp(0.0, 1.0)).unwrap_or(0.0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn context() -> Option<egui::Context> {
    lock(&SHARED.ctx).clone()
}

fn request_repaint() {
    if let Some(ctx) = context() {
        ctx.request_repaint_of(ViewportId::ROOT);
        board_due_in(Duration::ZERO);
    }
}

/// A repaint within one 60 Hz frame: a burst of backend events (streamed tokens, tool logs)
/// becomes one frame instead of one repaint per event.
fn request_repaint_soon() {
    if let Some(ctx) = context() {
        ctx.request_repaint_after_for(Duration::from_millis(16), ViewportId::ROOT);
        board_due_in(Duration::from_millis(16));
    }
}

/// When the board next wants to draw, in milliseconds since the HUD began (`hud_ms`): what
/// `keep_drawing` holds it to. Never, until it has asked.
static BOARD_DUE_MS: AtomicU64 = AtomicU64::new(u64::MAX);
/// How far past that the board is given before it is told to paint.
#[cfg_attr(not(windows), allow(dead_code))]
const BOARD_OVERDUE_MS: u64 = 20;

fn hud_ms() -> u64 {
    static BEGAN: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    BEGAN.elapsed().as_millis() as u64
}

/// The board wants its next frame in `wait` at the latest.
fn board_due_in(wait: Duration) {
    BOARD_DUE_MS.fetch_min(hud_ms() + wait.as_millis() as u64, Ordering::AcqRel);
}

/// Whether a board that wanted to draw at `due` (ms) has been kept waiting too long at `now`.
/// `kept`: it has just been kept from drawing; then it is told the moment it is due.
#[cfg_attr(not(windows), allow(dead_code))]
fn board_overdue(now: u64, due: u64, kept: bool) -> bool {
    now > due.saturating_add(if kept { 0 } else { BOARD_OVERDUE_MS })
}

/// Makes sure the board draws when it asked to, whatever his other windows do. Windows hands
/// a thread's windows their turn to paint from the top one down, and a window of his that
/// draws without a pause (a running simulation, the Earth as it turns) took every turn: the
/// board, at the very bottom as the desktop's background, drew nothing for as long as that
/// window stood. Seen live 2026-11-14: the emblem stood still, the orb did not come out, and a
/// closed window stayed, until the simulation was gone. A thread of its own looks a few times
/// a second, and a board that is overdue is told to paint by a message sent to it, which
/// waits for no turn. (Started once, with the board's first frame.)
#[cfg(windows)]
fn keep_drawing() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("jai-board-pulse".into()).spawn(|| {
            // (For a second after it was kept waiting it is told the moment it is due: it
            // then draws at more than half its own rate beside a window that takes every turn.)
            let mut kept_until = 0;
            while !SHARED.quitting.load(Ordering::Acquire) {
                // (Up to when the board is due, 20 ms at most: at rest it looks 50 times a second.)
                let rest = BOARD_DUE_MS.load(Ordering::Acquire).saturating_sub(hud_ms());
                std::thread::sleep(Duration::from_millis(if hud_ms() < kept_until { 5 } else { rest.clamp(5, 20) }));
                let (hwnd, now) = (SHARED.hwnd.load(Ordering::Acquire), hud_ms());
                let due = BOARD_DUE_MS.load(Ordering::Acquire);
                if hwnd != 0 && board_overdue(now, due, now < kept_until) && crate::win32::is_visible(hwnd) {
                    if board_overdue(now, due, false) {
                        kept_until = now + 1_000;
                    }
                    // (Its frame says when it wants the next; should none come of this, it
                    // is told again in a moment, not a hundred times a second.)
                    BOARD_DUE_MS.store(now + 100, Ordering::Release);
                    crate::win32_desktop::paint_now(hwnd);
                }
            }
        });
    });
}

/// Whether JAI can open native windows (false when headless).
pub fn windows_supported() -> bool {
    SHARED.running.load(Ordering::Acquire)
}

/// The monitor the board was sent to and has not gone to yet (`send_board_to_monitor`).
static HUD_MONITOR: Mutex<Option<usize>> = Mutex::new(None);

/// Sends the HUD's board to monitor `index` (1-based) as the fullscreen board there; a board
/// that spans every monitor comes back to that one. False when the HUD is a plain window, which
/// moves as any window does.
pub fn send_board_to_monitor(config: &ConfigStore, index: usize) -> bool {
    let iface = config.read(|c| c.interface.clone());
    if !(iface.fullscreen || iface.span_all_monitors) {
        return false;
    }
    *lock(&HUD_MONITOR) = Some(index);
    if iface.span_all_monitors {
        config.update(|c| {
            c.interface.span_all_monitors = false;
            c.interface.fullscreen = true;
        });
    }
    request_repaint();
    true
}

/// As the desktop's background he is on every monitor already: "move yourself to my second
/// monitor" sends his emblem to the middle of monitor `index`. (2026-11-14: the order moved the
/// background's own window, which is no window to move.)
pub fn send_emblem_to_monitor(config: &ConfigStore, monitors: &[crate::monitors::Monitor], index: usize) -> Result<String, String> {
    let (Some(first), Some(to)) = (home_monitor(monitors), monitors.iter().find(|m| m.index == index)) else {
        return Err(format!("There is no monitor {index}."));
    };
    // (A scripted session's background is its own monitor and no other.)
    if to.index != first.index && (test_session_monitor().is_some() || !config.read(|c| c.interface.background_all_monitors)) {
        return Err("My background covers one monitor only, Sir. Say \"background on all monitors\" and I can go there.".into());
    }
    let spot = emblem_spot_on(first.work, to.work);
    config.update(|c| {
        c.interface.orb_board_pos = spot;
        c.interface.orb_aside = false;
    });
    request_repaint();
    Ok(format!("My emblem stands on Monitor {index} now.\nSpoken: On monitor {index}, Sir."))
}

/// The monitor his home screen is on: Sir's first one, or a scripted session's own.
fn home_monitor(monitors: &[crate::monitors::Monitor]) -> Option<&crate::monitors::Monitor> {
    test_session_monitor().and_then(|own| monitors.iter().find(|m| m.index == own)).or_else(|| crate::monitors::primary(monitors))
}

/// The emblem's spot in the middle of the monitor whose work area is `to`, as a share of that
/// of Sir's first monitor, `first` (both left, top, right, bottom; beyond 0-1 on another
/// one). None: the first monitor's own middle, its home.
fn emblem_spot_on(first: [i32; 4], to: [i32; 4]) -> Option<[f32; 2]> {
    let share = |a: usize, b: usize| ((to[a] + to[b]) as f32 / 2.0 - first[a] as f32) / (first[b] - first[a]).max(1) as f32;
    (first != to).then(|| [share(0, 2), share(1, 3)])
}

/// The monitor the background's emblem stands on, by its spot in the settings (Sir's first one
/// while it is at home).
pub fn emblem_monitor(config: &ConfigStore, monitors: &[crate::monitors::Monitor]) -> Option<usize> {
    let first = home_monitor(monitors)?;
    let [x, y] = config.read(|c| c.interface.orb_board_pos).unwrap_or([0.5, 0.5]);
    let [l, t, r, b] = first.work;
    crate::monitors::monitor_at(monitors, l + (x * (r - l) as f32) as i32, t + (y * (b - t) as f32) as i32).or(Some(first.index))
}

/// The monitor the fullscreen board was sent to, once it can go there (a board that still
/// spans the monitors and a window leave the request where it is).
fn take_monitor_request(current: &InterfaceSettings) -> Option<usize> {
    if current.fullscreen && !current.span_all_monitors { lock(&HUD_MONITOR).take() } else { None }
}

/// The board id of simulation `id`'s block.
pub fn simulation_block_id(id: &str) -> String {
    simlab_view::dock_key(id)
}

/// The part of the HUD in-app blocks live in right now (nothing while no HUD is up).
pub fn block_area() -> Rect {
    context().map_or(Rect::NOTHING, |ctx| block_bounds(home_screen(&ctx)))
}

/// The HUD's native window handle (0 while it has none).
pub fn hud_hwnd() -> isize {
    SHARED.hwnd.load(Ordering::Acquire)
}

/// Widget content changed or a window closed: a repaint is enough (JAI's windows redraw and
/// hide themselves on their own schedule, even while the HUD sits in the tray).
pub fn refresh() {
    request_repaint();
}

/// Applies changed widgets/windows now: native windows are created, patched and closed during
/// the HUD's frames. If the HUD does not paint (never opened, or hidden in the tray by a system
/// that suspends hidden windows), it is brought up so the window can appear.
pub fn sync_windows() {
    if !SHARED.running.load(Ordering::Acquire) {
        return;
    }
    match context() {
        Some(ctx) => {
            ctx.request_repaint_of(ViewportId::ROOT);
            let before = ROOT_FRAMES.load(Ordering::Acquire);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                if ROOT_FRAMES.load(Ordering::Acquire) != before || SHARED.quitting.load(Ordering::Acquire) {
                    return;
                }
                // Hidden in the tray: run a few HUD frames minimised (no focus change), then hide
                // it again — the new window opens without the HUD popping up.
                #[cfg(windows)]
                {
                    let hwnd = SHARED.hwnd.load(Ordering::Acquire);
                    if hwnd != 0 && !crate::win32::is_visible(hwnd) {
                        crate::win32::show_minimized_no_activate(hwnd);
                        request_repaint();
                        let deadline = Instant::now() + Duration::from_millis(1500);
                        while ROOT_FRAMES.load(Ordering::Acquire) < before + 3 && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(30));
                            request_repaint();
                        }
                        if ROOT_FRAMES.load(Ordering::Acquire) >= before + 3 {
                            crate::win32::hide_hud(hwnd);
                            info!("[HUD] Pumped the hidden HUD for the window change (it stays in the tray).");
                            return;
                        }
                    }
                }
                info!("[HUD] The hidden HUD did not repaint; showing it so the new window can open.");
                show();
            });
        }
        None => wake(Wake::Open),
    }
}

fn wake(signal: Wake) {
    if let Some(tx) = lock(&SHARED.wake).as_ref() {
        let _ = tx.send(signal);
    }
}

/// Shows the HUD: brings the window to the front or opens it (tray click, "show yourself", a
/// second `JAI.exe`, restyling requests). Does nothing in headless mode.
pub fn show() {
    if !SHARED.running.load(Ordering::Acquire) {
        return;
    }
    // As the desktop's background there is no window to bring up (raising it would put the
    // desktop itself in front of what Sir is working in): his views open as windows of their
    // own, and a frame is all they need.
    if crate::wallpaper::in_use() {
        request_repaint();
        return;
    }
    #[cfg(windows)]
    {
        let hwnd = SHARED.hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            crate::win32::run_detached(move || crate::win32::show_hud(hwnd));
            return;
        }
    }
    match context() {
        Some(ctx) => {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
            // (A test session never takes the focus from Sir: he may be playing.)
            if !crate::platform::in_test_session() {
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
            }
            ctx.request_repaint();
        }
        None => wake(Wake::Open),
    }
}

/// Opens the mobile satellite pairing window (QR code + link).
pub fn show_pairing() {
    pairing_view::request();
    sync_windows();
}

/// "Open the interface settings": the settings panel opens and the HUD comes up.
pub fn open_interface_settings() {
    crate::hud_feel::open_settings_panel();
    show();
    request_repaint();
}

/// "Open my gym logs": the Gym & Workout PR Coach window opens on the HUD's next frame.
static WORKOUT_REQUEST: AtomicBool = AtomicBool::new(false);

/// Opens the Gym & Workout PR Coach window and brings the HUD up.
pub fn open_workout() {
    WORKOUT_REQUEST.store(true, Ordering::Release);
    show();
    request_repaint();
}

/// How many of his console, his history and his camera stand in windows of their own (as of
/// the HUD's last frame), and the request to close them.
static PIECES_UP: AtomicU8 = AtomicU8::new(0);
static PIECES_CLOSE: AtomicBool = AtomicBool::new(false);

/// "Close all windows": his text console, the history and the camera, while they are windows
/// of their own, close in the HUD's next frame. How many. (Sir, 2026-11-14: "close all
/// windows" answered "there are no secondary windows open" beside his text console.)
pub fn close_piece_windows() -> usize {
    let up = PIECES_UP.load(Ordering::Acquire) as usize;
    if up > 0 {
        PIECES_CLOSE.store(true, Ordering::Release);
        request_repaint();
    }
    up
}

/// "Open / close the text interface": 1 = open, 2 = close, applied on the HUD's next frame.
static CONSOLE_REQUEST: AtomicU8 = AtomicU8::new(0);

/// Slides the text console out (or back in) and brings the HUD up.
pub fn set_text_console(open: bool) {
    CONSOLE_REQUEST.store(if open { 1 } else { 2 }, Ordering::Release);
    if open {
        show();
    }
    request_repaint();
}

/// His windows on call (Sir's key for it, "show your windows"): every window of his comes in
/// front of the other programs' windows, in their own order, and none takes the keyboard. How
/// many there are. (The board itself and the corner orb are not among them.)
pub fn windows_on_call() -> usize {
    let own = crate::own_windows::list();
    let n = own.len();
    info!("[HUD] His windows on call: {n} brought to the front");
    // (And their list, to pick one from, move it or close it.)
    WINDOWS_LIST.store(true, Ordering::Release);
    request_repaint();
    wm::run_detached(move || {
        // (From the lowest up: they keep the order they had among themselves.)
        for w in own.iter().rev() {
            desk::raise(w.hwnd);
        }
    });
    n
}

/// The list of his windows is open ("show your windows", his key for it).
static WINDOWS_LIST: AtomicBool = AtomicBool::new(false);

/// `JAI_DEMO=desk-click`: where the header's `[ CHANGES ]` key stands (window points), for a
/// scripted session's stand-in for Sir's click on the desktop.
static DEMO_KEY: Mutex<Option<Rect>> = Mutex::new(None);


/// Text the console's input line takes on its next frame (the Feature Lab's `[ TRY ]`).
static CONSOLE_PREFILL: Mutex<Option<String>> = Mutex::new(None);

/// Opens the text console with `text` in its input line, ready for Sir to finish.
fn prefill_console(text: String) {
    *lock(&CONSOLE_PREFILL) = Some(text);
    set_text_console(true);
}

/// Orders a block hands to the HUD, carried out on its next frame as if typed (so they pass
/// the same checks: a question only reads, anything outside JAI asks first).
static ORDERS: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

/// `text` is carried out as an order on the HUD's next frame; `undo_first`: what a misheard
/// order changed in the last two minutes is taken back first.
pub fn run_order(text: String, undo_first: bool) {
    lock(&ORDERS).push((text, undo_first));
    refresh();
}

/// HUD controls the Feature Lab's `[ TRY ]` can open (`open_control`).
#[cfg(test)]
const CONTROL_IDS: &[&str] = &["screens", "theme_studio", "mixer"];
/// `[ TRY ]` on the monitor features: the SCREENS block opens on the HUD's next frame.
static SCREENS_REQUEST: AtomicBool = AtomicBool::new(false);

/// Opens a HUD control by id (the SCREENS mover, the Theme Studio, the Mixer) and brings the HUD
/// up.
fn open_control(id: &str) {
    match id {
        "screens" => SCREENS_REQUEST.store(true, Ordering::Release),
        "theme_studio" => theme_studio::open(),
        "mixer" => mixer_view::open(),
        _ => return,
    }
    show();
    request_repaint();
}

/// "Open the theme studio": the block where he designs a theme (and closes it again).
pub fn show_theme_studio(open: bool) {
    if open {
        open_control("theme_studio");
    } else {
        theme_studio::close();
        request_repaint();
    }
}

/// "Open the accounts list" (the memory database it reads), or closes it (`None`).
pub fn show_accounts(db: Option<String>) {
    match db {
        Some(db) => {
            accounts_view::open(db, false);
            show();
        }
        None => accounts_view::close(),
    }
    request_repaint();
}

/// His accounts list on another PC in admin mode: the accounts at his main PC.
pub fn show_remote_accounts() {
    accounts_view::open(String::new(), true);
    show();
    request_repaint();
}

/// "Open the admin window" (or closes it).
pub fn show_admin(open: bool) {
    admin_view::set_open(open);
    if open {
        show();
    }
    request_repaint();
}

/// The pop-up for his admin sign-in (on his main PC, once: the making of his admin account).
pub fn show_admin_sign_in() {
    admin_view::ask();
    show();
    request_repaint();
}

/// The admin window's tiles for the HUD's own windows that no order opens.
pub fn show_discord() {
    discord_view::open();
    show();
    request_repaint();
}

pub fn show_screens() {
    open_control("screens");
}

pub fn show_hub() {
    open_integrations(None);
}

/// `show_devices`: the devices drawer opens on the HUD's next frame.
static DEVICES_REQUEST: AtomicBool = AtomicBool::new(false);

pub fn show_devices() {
    DEVICES_REQUEST.store(true, Ordering::Release);
    show();
    request_repaint();
}

/// The installer's window (`installer::run`): the login of another PC in his look, then
/// everything fetched.
pub fn run_installer(rt: Handle, config: crate::config::ConfigStore, card: String, brain: Result<&'static str, String>, into: std::path::PathBuf) -> Result<(), String> {
    login_view::installing(config.read(|c| c.main_pc.clone()), crate::fleet::key(&config).is_some(), card, brain, &into);
    let viewport = egui::ViewportBuilder::default().with_title("Install J.A.I.").with_app_id("jai").with_inner_size([520.0, 560.0]).with_decorations(false).with_icon(Arc::new(window_icon().unwrap_or_default()));
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: true,
        persist_window: false,
        #[cfg(not(windows))]
        event_loop_builder: on_x11(),
        ..Default::default()
    };
    eframe::run_native(
        "J.A.I. setup",
        options,
        Box::new(move |cc| {
            holo::install_fonts(&cc.egui_ctx);
            configure_style(&cc.egui_ctx, 1.0);
            apply_hud_visuals(&cc.egui_ctx);
            *lock(&SHARED.ctx) = Some(cc.egui_ctx.clone());
            Ok(Box::new(Installer { rt, config }))
        }),
    )
    .map_err(|e| e.to_string())
}

struct Installer {
    rt: Handle,
    config: crate::config::ConfigStore,
}

impl eframe::App for Installer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        login_view::installer(ctx, &self.rt, &self.config);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// The login of a J.A.I. on another PC (`fleet`): opened while it has no key to Sir's PC.
pub fn show_login() {
    if !login_view::is_open() {
        login_view::open();
        show();
        request_repaint();
    }
}

/// "Open the mixer": a fader for every channel (and closes it again).
pub fn show_mixer(open: bool) {
    if open {
        open_control("mixer");
    } else {
        mixer_view::close();
        request_repaint();
    }
}

/// "Open the hotkeys window".
pub fn show_hotkeys() {
    hotkeys_view::open();
    show();
    request_repaint();
}

/// Whether the view with this `configure_interface` key is still up. (The core and the console
/// are asked to close either way: for them closing what is shut does nothing.)
pub fn view_is_up(key: &str) -> bool {
    match key {
        "theme_studio" => theme_studio::is_open(),
        "mixer_view" => mixer_view::is_open(),
        "feature_lab" => features_view::is_open(),
        _ => true,
    }
}

/// "Show me your core", "make your core bigger": applied on the HUD's next frame.
static CORE_REQUESTS: Mutex<Vec<crate::holo_mind::CoreCommand>> = Mutex::new(Vec::new());

/// Opens, closes, zooms or spins the core sphere and brings the HUD up.
pub fn core_command(cmd: crate::holo_mind::CoreCommand) {
    lock(&CORE_REQUESTS).push(cmd);
    if !matches!(cmd, crate::holo_mind::CoreCommand::Open(false)) {
        show();
    }
    request_repaint();
}

/// The time a chat line arrived ("14:02:11").
fn clock_now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Opens (or closes) the Feature Lab: every tool JAI has, each with its `[ TRY ]`.
pub fn show_features(open: bool) {
    features_view::set_open(open);
    if open {
        show();
    }
    request_repaint();
}

/// Opens the Integrations & Productivity Hub (on a service's tab) and brings the HUD up.
pub fn open_integrations(service: Option<&'static str>) {
    integrations::request(service);
    show();
    request_repaint();
}

/// Hides the HUD to the tray; JAI keeps running.
pub fn hide() {
    // (As the desktop's background there is nothing to hide: he is the desktop, always, and
    // `show` could not bring a hidden background back.)
    if crate::wallpaper::in_use() {
        return;
    }
    #[cfg(windows)]
    {
        let hwnd = SHARED.hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            crate::win32::hide_hud(hwnd);
        }
    }
    // (Hidden, not closed: closing the root window would end him.)
    #[cfg(not(windows))]
    if let Some(ctx) = context() {
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(false));
        ctx.request_repaint();
    }
}

/// Ends `run`; called once JAI's own shutdown work is done.
pub fn quit() {
    SHARED.quitting.store(true, Ordering::Release);
    #[cfg(windows)]
    {
        let hwnd = SHARED.hwnd.load(Ordering::Acquire);
        if hwnd != 0 && crate::win32::post_hud_quit(hwnd) {
            return;
        }
    }
    if let Some(ctx) = context() {
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
        ctx.request_repaint();
    }
    wake(Wake::Quit);
}

/// One-shot window actions from `configure_interface`.
pub fn window_command(command: WindowCommand) -> Result<(), String> {
    if !SHARED.running.load(Ordering::Acquire) {
        return Err("JAI is running without his HUD window (headless), so there is no window to change.".into());
    }
    // As the desktop's background he has no window to hide, shrink or move (Sir, 2026-11-20:
    // "he should always be the desktop"): the floating orb is what comes and goes.
    if crate::wallpaper::in_use() && command != WindowCommand::Show {
        return Err("I'm your desktop's background, Sir, always: I have no window to change. Say \"orb mode\" and my orb floats over your work.".into());
    }
    match command {
        WindowCommand::Show => show(),
        WindowCommand::Hide => hide(),
        #[cfg(windows)]
        other => {
            let hwnd = SHARED.hwnd.load(Ordering::Acquire);
            if hwnd == 0 {
                // Not opened yet: applied as soon as it exists.
                lock(&SHARED.pending).push(other);
                show();
            } else {
                crate::win32::run_detached(move || apply_window_command(hwnd, other));
            }
        }
        #[cfg(not(windows))]
        other => {
            context().ok_or("The HUD window is not open.")?;
            let hwnd = SHARED.hwnd.load(Ordering::Acquire);
            if hwnd == 0 && !matches!(other, WindowCommand::Minimize | WindowCommand::Maximize | WindowCommand::Restore) {
                return Err("The HUD runs on Wayland alone here (no X11), where no program may move its window.".into());
            }
            wm::run_detached(move || apply_window_command(hwnd, other));
        }
    }
    Ok(())
}

/// The window commands on X11: the state through the window toolkit, the place through X11.
#[cfg(not(windows))]
fn apply_window_command(hwnd: isize, command: WindowCommand) {
    use crate::interface::Corner;
    use crate::x11::Spot;
    let spot = match command {
        WindowCommand::Center => Spot::Center,
        WindowCommand::Bigger => Spot::Scale(1.2),
        WindowCommand::Smaller => Spot::Scale(1.0 / 1.2),
        WindowCommand::Place(Corner::Left) => Spot::Left,
        WindowCommand::Place(Corner::Right) => Spot::Right,
        WindowCommand::Place(Corner::TopLeft) => Spot::TopLeft,
        WindowCommand::Place(Corner::TopRight) => Spot::TopRight,
        WindowCommand::Place(Corner::BottomLeft) => Spot::BottomLeft,
        WindowCommand::Place(Corner::BottomRight) => Spot::BottomRight,
        WindowCommand::Show | WindowCommand::Hide => return,
        state => {
            if let Some(ctx) = context() {
                if state != WindowCommand::Minimize {
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
                }
                ctx.send_viewport_cmd_to(ViewportId::ROOT, match state {
                    WindowCommand::Minimize => ViewportCommand::Minimized(true),
                    _ => ViewportCommand::Maximized(state == WindowCommand::Maximize),
                });
                ctx.request_repaint();
            }
            return;
        }
    };
    show();
    if let Err(e) = crate::x11::arrange(hwnd, spot) {
        warn!("[HUD] Window action {command:?} failed: {e}");
    }
}

/// Resizes the HUD window (logical points) — `control_jai_interface` set_window_size.
pub fn set_window_size(width: f32, height: f32) -> Result<(), String> {
    if !SHARED.running.load(Ordering::Acquire) {
        return Err("JAI is running without his HUD window (headless), so there is no window to resize.".into());
    }
    let ctx = context().ok_or("The HUD window is not open.")?;
    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Maximized(false));
    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::InnerSize(vec2(width, height)));
    ctx.request_repaint_of(ViewportId::ROOT);
    Ok(())
}

/// Runs on the Win32 worker thread (synchronous window calls must not run on the window's own).
#[cfg(windows)]
fn apply_window_command(hwnd: isize, command: WindowCommand) {
    use crate::interface::Corner;
    use crate::win32::{place, show_window, snap, Placement, Snap, WindowCmd};
    if command != WindowCommand::Minimize {
        crate::win32::show_hud(hwnd);
    }
    let state = match command {
        WindowCommand::Minimize => Some(WindowCmd::Minimize),
        WindowCommand::Maximize => Some(WindowCmd::Maximize),
        WindowCommand::Restore => Some(WindowCmd::Restore),
        _ => None,
    };
    if let Some(state) = state {
        show_window(hwnd, state);
        return;
    }
    let result = match command {
        WindowCommand::Center => place(hwnd, Placement::Center),
        WindowCommand::Bigger => place(hwnd, Placement::Scale(1.2)),
        WindowCommand::Smaller => place(hwnd, Placement::Scale(1.0 / 1.2)),
        WindowCommand::Place(Corner::Left) => snap(hwnd, Snap::Left),
        WindowCommand::Place(Corner::Right) => snap(hwnd, Snap::Right),
        WindowCommand::Place(Corner::TopLeft) => place(hwnd, Placement::TopLeft),
        WindowCommand::Place(Corner::TopRight) => place(hwnd, Placement::TopRight),
        WindowCommand::Place(Corner::BottomLeft) => place(hwnd, Placement::BottomLeft),
        WindowCommand::Place(Corner::BottomRight) => place(hwnd, Placement::BottomRight),
        WindowCommand::Show | WindowCommand::Hide | WindowCommand::Minimize | WindowCommand::Maximize | WindowCommand::Restore => {
            Ok(())
        }
    };
    if let Err(e) = result {
        warn!("[HUD] Window action {command:?} failed: {e}");
    }
}

/// Remembers where the window is, so it reopens there (skipped in fullscreen).
pub fn remember_placement(config: &ConfigStore) {
    let hwnd = SHARED.hwnd.load(Ordering::Acquire);
    // (The background's rect is no window's place.)
    if hwnd == 0 || crate::wallpaper::in_use() || config.read(|c| c.interface.fullscreen) {
        return;
    }
    #[cfg(windows)]
    let placement = crate::win32::window_placement(hwnd);
    // (X11 tells the bounds the window has now: a maximised one keeps the place it had before.)
    #[cfg(not(windows))]
    let placement = crate::x11::window_state(hwnd).map(|(rect, maximized, _)| (if maximized { config.read(|c| c.hud_window).unwrap_or(rect) } else { rect }, maximized));
    if let Some((rect, maximized)) = placement {
        config.update(|c| {
            c.hud_window = Some(rect);
            c.hud_maximized = maximized;
        });
    }
}

/// The HUD's link to the backend; it follows JAI's events from `attach` on, whether or not a
/// window is open.
pub struct Hud {
    app: Jai,
    rt: Handle,
    model: Arc<Mutex<HudModel>>,
    wake: mpsc::Receiver<Wake>,
}

impl Hud {
    /// Starts following JAI (before he says anything, so the chat shows it all).
    pub fn attach(app: Jai, rt: Handle) -> Self {
        let (tx, wake) = mpsc::channel();
        *lock(&SHARED.wake) = Some(tx);
        let model = Arc::new(Mutex::new(HudModel::default()));
        feed_model(&app, &rt, &model);
        SHARED.running.store(true, Ordering::Release);
        Self { app, rt, model, wake }
    }

    /// Runs the window on the calling (main) thread until JAI quits. Returns early — JAI
    /// keeps running in the tray — if no window can be created (e.g. no OpenGL driver).
    pub fn run(self, open_now: bool) {
        let mut open = open_now;
        while !SHARED.quitting.load(Ordering::Acquire) {
            if !open {
                match self.wake.recv() {
                    Ok(Wake::Open) => {}
                    Ok(Wake::Quit) | Err(_) => break,
                }
                if SHARED.quitting.load(Ordering::Acquire) {
                    break;
                }
            }
            open = false;
            if let Err(e) = open_window(&self.app, &self.rt, &self.model) {
                crate::platform::alert(&format!(
                    "The J.A.I. window could not be opened ({e}). He keeps listening in the tray; \
                     updating the graphics driver usually fixes this."
                ));
                break;
            }
            // On Windows the window only ends when JAI quits. Elsewhere closing it lands here,
            // and the tray (or "show yourself") opens a new one.
            // As the desktop's background his window goes down with the desktop (when Explorer
            // starts again): once the desktop is back, the window is opened anew and handed to
            // it again. He himself never stopped.
            #[cfg(windows)]
            if !SHARED.quitting.load(Ordering::Acquire) && crate::wallpaper::in_use() {
                crate::wallpaper::lost();
                warn!("[DESKTOP] The desktop took my window down with it (Explorer starting again?); I open it anew once the desktop is back");
                let deadline = Instant::now() + Duration::from_secs(90);
                while !crate::win32_desktop::desktop_exists() && Instant::now() < deadline && !SHARED.quitting.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(500));
                }
                // (The new desktop gets a moment to stand before it is asked to take a window.)
                std::thread::sleep(Duration::from_millis(2500));
                open = true;
            }
        }
        SHARED.running.store(false, Ordering::Release);
        lock(&SHARED.wake).take();
    }
}

/// Keeps `model` current from backend events, whether or not a window is open.
fn feed_model(app: &Jai, rt: &Handle, model: &Arc<Mutex<HudModel>>) {
    let mut events = app.subscribe();
    {
        let mut m = lock(model);
        for ev in app.snapshot_events() {
            m.apply(ev);
        }
    }
    let feed = Arc::clone(model);
    rt.spawn(async move {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    // Audio levels arrive ~30×/s; the animated window repaints on its own.
                    let repaint = !matches!(ev, HudEvent::AudioLevel(_));
                    lock(&feed).apply(ev);
                    if repaint {
                        request_repaint_soon();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    send(app, rt, model, HudCommand::ListModels);
}

/// Runs a command on the backend; a direct reply (search results, model list, …) lands in the model.
fn send(app: &Jai, rt: &Handle, model: &Arc<Mutex<HudModel>>, cmd: HudCommand) {
    let (app, model) = (app.clone(), Arc::clone(model));
    rt.spawn(async move {
        if let Some(reply) = app.handle_command(cmd).await {
            lock(&model).apply(reply);
            request_repaint();
        }
    });
}

/// The monitor this session's HUD opens on (`--monitor`; 0 = where it opens anyway). A test
/// session's only: its settings file is read-only, nothing of this is saved.
static START_MONITOR: AtomicUsize = AtomicUsize::new(0);

/// `--monitor <n>`: this session's HUD opens on monitor `n` (1 = the primary), so a test window
/// stays off the monitor Sir is playing on.
pub fn start_on_monitor(n: usize) {
    START_MONITOR.store(n, Ordering::Release);
}

/// In a scripted session: the monitor it was given (`--monitor`, 1-based; 0: none). None in a
/// session of Sir's own. Whatever a scripted session puts on a screen goes on that monitor
/// and on no other.
pub fn test_session_monitor() -> Option<usize> {
    crate::platform::in_test_session().then(|| START_MONITOR.load(Ordering::Acquire))
}

/// The monitor `--monitor` asked for, in logical points; None without one (or when there is no
/// such monitor: then everything opens where it would anyway). Cheap enough for every frame.
fn start_monitor_points() -> Option<Rect> {
    let n = START_MONITOR.load(Ordering::Acquire);
    if n == 0 {
        return None;
    }
    let m = crate::monitors::recent().into_iter().find(|m| m.index == n)?;
    let scale = wm::primary_screen().2;
    Some(Rect::from_min_max(pos2(m.rect[0] as f32 / scale, m.rect[1] as f32 / scale), pos2(m.rect[2] as f32 / scale, m.rect[3] as f32 / scale)))
}

thread_local! {
    /// The next window made was placed by an order of Sir's ("move the radar to my second
    /// monitor", a window made at a spot he named): no remembered place over it.
    static PLACED_BY_ORDER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The window about to be made (`for_test_session`, next) was placed by an order of Sir's.
pub(crate) fn placed_by_order() {
    PLACED_BY_ORDER.with(|p| p.set(true));
}

/// A window of JAI's own in a test session: it never takes the focus (Sir may be playing),
/// and with `--monitor` one without a place of its own opens in the middle of that monitor
/// (else Windows would put it on the primary one).
pub(crate) fn for_test_session(builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
    // Every window of JAI's own passes here: it comes up hidden and shows itself once it has
    // drawn its first frame (`reveal_when_drawn`, `reveal_overdue`), so Windows never shows its
    // empty client area (a white or black flash) before the window's entrance.
    // It comes up without the keyboard, in every session: what Sir is typing or playing in
    // keeps it, and a click on the window gives it the focus. (Sir, 2026-11-13: "the HOW IT
    // WORKS buttons don't do anything", "when I detach a window it closes". Seen from outside
    // in his running session: the windows existed, had drawn their frames, and were hidden. A
    // scripted session's windows, which never took the focus as they were shown, had shown
    // every time; his took it. Why taking it left them hidden is not known, so `keep_shown`
    // looks after every one of them besides.)
    let mut builder = builder.with_visible(false).with_active(false);
    // (One icon for all of them, the same `Arc` in every frame: egui tells icons apart by
    // their `Arc`, and with a fresh one per frame each window had its icon set anew sixty
    // times a second.)
    builder.icon = shared_icon();
    // A window Sir has had before opens where it last stood, at the size it had, unless an
    // order of his placed it or another of his windows stands there; what the code asks of it
    // later is done. (His choice of 2026-11-14: "Windows remember their place".)
    let by_order = PLACED_BY_ORDER.with(|p| p.replace(false));
    let code = [builder.position.map(|p| [p.x, p.y]), builder.inner_size.map(|s| [s.x, s.y])];
    if let Some([x, y, w, h]) = builder.title.as_deref().and_then(|title| crate::own_windows::opening_place(title, code, by_order, |place| place_is_free(title, place))) {
        let scale = points_scale();
        return builder.with_position(pos2(x as f32, y as f32) / scale).with_inner_size(vec2(w as f32, h as f32) / scale);
    }
    // A window with no place of its own opens on the monitor Sir is using, not always on his
    // first one, and beside the core there. (His choice of 2026-11-13: "Windows open where
    // you work".)
    let placed = builder.position.is_none().then(|| place_where_sir_works(&builder)).flatten();
    match (placed, builder.position, start_monitor_points()) {
        (Some(at), ..) => builder.with_position(at),
        // (A scripted session whose monitor is not known yet: its middle, never Sir's screen.)
        (None, None, Some(m)) => {
            let size = builder.inner_size.unwrap_or(vec2(800.0, 600.0)).min(m.size());
            builder.with_position(m.center() - size / 2.0)
        }
        _ => builder,
    }
}

/// How many of the screen's pixels one of the HUD's points is (Windows' scale times his zoom).
fn points_scale() -> f32 {
    let scale = wm::primary_screen().2;
    (scale * context().map_or(1.0, |ctx| ctx.zoom_factor())).max(0.1)
}

/// Where the board's own top-left corner is on the screen (points); the HUD says so every
/// frame. With it the core has a place among his windows (`core_on_screen`).
static BOARD_ORIGIN: Mutex<Option<Pos2>> = Mutex::new(None);

/// The core on the screen (points), with the room its light takes.
fn core_on_screen() -> Option<Rect> {
    let origin = (*lock(&BOARD_ORIGIN))?;
    crate::hud_dock::orb().map(|core| core.translate(origin.to_vec2()))
}

/// Where a new window of `size` opens on the monitor whose work area is `work`: about the
/// core when he stands on that monitor, beside him on his circle, the emptier side first, off
/// the windows that stand there already (`holo_stage::slot`); else at `middle`, or where it
/// stands on none of them when one stands there.
fn opening_spot(work: Rect, size: Vec2, core: Option<Rect>, taken: &[Rect], middle: Pos2) -> Pos2 {
    use crate::holo_stage::{slot, GAP};
    let Some(core) = core.filter(|core| work.contains(core.center())) else {
        let on_one = taken.iter().any(|t| t.intersects(Rect::from_min_size(middle, size).shrink(1.0)));
        return on_one.then(|| crate::hud_dock::find_non_overlapping_slot(work, size, taken, GAP)).flatten().unwrap_or(middle);
    };
    // Out on his second circle where the screen has the room: the leader that ties the window
    // to him has a length to show there (hard against his first circle it has none).
    let far = Rect::from_center_size(core.center(), core.size() * 1.5);
    let with_core: Vec<Rect> = taken.iter().copied().chain([core]).collect();
    slot(work, size, far, taken, GAP)
        .or_else(|| slot(work, size, core, taken, GAP))
        // No room left about him: any free spot off him and off the others.
        .or_else(|| crate::hud_dock::find_non_overlapping_slot(work, size, &with_core, GAP))
        // No free spot at all: over the windows beside him, never on him.
        .or_else(|| stacked_beside(work, size, core, taken))
        .unwrap_or(middle)
}

/// A place beside the core for a window that finds no free one: on his emptier flank, over
/// the windows that stand there, a step outwards for each and below the header of the lowest,
/// so that every header stays in view. None for a window too wide to stand beside him at all.
fn stacked_beside(work: Rect, size: Vec2, core: Rect, taken: &[Rect]) -> Option<Pos2> {
    let c = core.center();
    let left = taken.iter().filter(|t| t.center().x < c.x).count();
    let sides = if left < taken.len() - left { [-1.0, 1.0] } else { [1.0, -1.0] };
    let top = (c.y - size.y / 2.0).clamp(work.top(), (work.bottom() - size.y).max(work.top()));
    sides.into_iter().find_map(|side: f32| {
        let x = if side > 0.0 { (core.right() + crate::holo_stage::GAP).min(work.right() - size.x) } else { (core.left() - crate::holo_stage::GAP - size.x).max(work.left()) };
        if Rect::from_min_size(pos2(x, top), size).intersects(core) {
            return None;
        }
        let there: Vec<&Rect> = taken.iter().filter(|t| (t.center().x - c.x) * side > 0.0).collect();
        let below = there.iter().map(|t| t.top() + WINDOW_TITLE_ROW + 4.0).fold(top, f32::max);
        Some(pos2((x + side * 34.0 * there.len() as f32).clamp(work.left(), work.right() - size.x), below.min((work.bottom() - size.y).max(work.top()))))
    })
}

/// A stage panel (the radar, the disk map, the Earth) whose window stands over the middle of
/// the board: the core makes room for it, as it did for such a panel on the board.
fn stage_window_covers_centre(ctx: &egui::Context, screen: Rect) -> bool {
    let Some(board) = ctx.input(|i| i.viewport().inner_rect) else { return false };
    let zone = crate::hud_dock::centre_safe_zone(screen);
    let motions = lock(&DETACHED_MOTIONS);
    crate::hud_dock::STAGE_BLOCKS.iter().map(|id| viewport_for_block(id)).any(|window| !motions.leaving.contains_key(&window) && motions.rects.get(&window).is_some_and(|rect| rect.translate(-board.min.to_vec2()).intersects(zone)))
}

/// The windows `place_where_sir_works` placed (points), by title, and when each was last asked
/// for.
static PLACES: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (Rect, Instant)>>> = std::sync::LazyLock::new(Default::default);

/// What stands in the way of a window titled `title` (points): his other windows where they
/// stand now, and those that have not shown themselves yet where they will come up (`placed` a
/// moment ago, or at the place they had before).
fn in_the_way(title: &str, placed: &std::collections::HashMap<String, (Rect, Instant)>) -> Vec<Rect> {
    let scale = points_scale();
    let points = |r: [i32; 4]| Rect::from_min_max(pos2(r[0] as f32, r[1] as f32) / scale, pos2(r[2] as f32, r[3] as f32) / scale);
    let standing: Vec<(String, Rect)> = crate::own_windows::list().into_iter().map(|w| (w.title, points(w.rect))).collect();
    let coming = placed.iter().map(|(t, (rect, _))| (t.clone(), *rect)).chain(crate::own_windows::opening_now().into_iter().map(|(t, [x, y, w, h])| (t, points([x, y, x + w, y + h]))));
    coming.filter(|(t, _)| !standing.iter().any(|(shown, _)| shown == t)).chain(standing.iter().cloned()).filter(|(t, _)| t != title).map(|(_, rect)| rect).collect()
}

/// Whether a window may come up at the place it had before (x, y, w, h): not where another of
/// his windows stands or comes up with it, unless he said "overlap the windows". (Sir,
/// 2026-11-19: "open 3 random windows" brought three up on one spot; each had last been open
/// alone, in the first place beside the core.)
fn place_is_free(title: &str, [x, y, w, h]: [i32; 4]) -> bool {
    let scale = points_scale();
    let place = Rect::from_min_size(pos2(x as f32, y as f32) / scale, vec2(w as f32, h as f32) / scale).shrink(1.0);
    let free = crate::hud_dock::overlap_allowed() || !in_the_way(title, &lock(&PLACES)).iter().any(|other| other.intersects(place));
    if !free {
        info!("[HUD] '{title}' does not come up where it stood before: another of his windows stands there");
    }
    free
}

/// Where a window of JAI's own that has no place yet opens (points): on the monitor Sir is
/// using, beside the core when he stands there (`opening_spot`), else in its middle, a step
/// aside for each window of his that opened just before; beside a game, on a monitor the game
/// is not on ("A game is left alone"). The same place for as long as the window is asked for
/// (by its title), so a window Sir has moved is never pulled back.
fn place_where_sir_works(builder: &egui::ViewportBuilder) -> Option<Pos2> {
    let title = builder.title.clone()?;
    let mut places = lock(&PLACES);
    let now = Instant::now();
    // (Not asked for during a second and no such window left: it was closed, and its next
    // opening is placed anew. A window that is still there keeps its place however long a
    // frame of the board took: placed anew it would jump.)
    places.retain(|title, (_, seen)| {
        let fresh = now.duration_since(*seen) < Duration::from_secs(1);
        fresh || wm::own_window(title).is_some()
    });
    if let Some((at, seen)) = places.get_mut(&title) {
        *seen = now;
        return Some(at.min);
    }
    let monitors = crate::monitors::recent();
    // (monitor_in_use already keeps off a game's monitor: a second swap here went back onto it.)
    let on = crate::wallpaper::monitor_in_use(&monitors)?;
    let scale = points_scale();
    let points = |r: [i32; 4]| Rect::from_min_max(pos2(r[0] as f32, r[1] as f32) / scale, pos2(r[2] as f32, r[3] as f32) / scale);
    let work = points(on.work);
    let size = builder.inner_size.unwrap_or(vec2(800.0, 600.0)).min(work.size());
    let step = (places.len() % 6) as f32 - 1.0;
    let middle = work.center() - size / 2.0 + vec2(34.0, 30.0) * step;
    let middle = pos2(middle.x.clamp(work.left(), (work.right() - size.x).max(work.left())), middle.y.clamp(work.top(), (work.bottom() - size.y).max(work.top())));
    let taken = in_the_way(&title, &places);
    // ("Overlap the windows": always the step aside in the middle.)
    let at = if crate::hud_dock::overlap_allowed() { middle } else { opening_spot(work, size, core_on_screen(), &taken, middle) };
    info!("[HUD] '{title}' opens at {:.0},{:.0}, {}", at.x, at.y, if at == middle { "in the middle of the monitor in use" } else { "about the core" });
    places.insert(title, (Rect::from_min_size(at, size), now));
    Some(at)
}

fn open_window(app: &Jai, rt: &Handle, model: &Arc<Mutex<HudModel>>) -> Result<(), String> {
    let (iface, placement, maximized) = app.config().read(|c| (c.interface.clone(), c.hud_window, c.hud_maximized));
    let level = if iface.always_on_top { egui::viewport::WindowLevel::AlwaysOnTop } else { egui::viewport::WindowLevel::Normal };
    let (size, min) = (layout_size(iface.layout), min_size(iface.layout));
    let start_on = start_monitor_points();
    let asked = START_MONITOR.load(Ordering::Acquire);
    if asked > 0 && start_on.is_none() {
        warn!("[HUD] --monitor {asked}: there is no monitor {asked} ({} connected); the HUD opens where it would anyway", crate::monitors::list().len());
    }
    // He starts as the desktop's background, always (Sir, 2026-11-20: "he shouldnt have a normal
    // window"), when there is a rect for it: a scripted session without a monitor of its own
    // has none and starts as the board. (The floating orb is a window of its own over it.)
    let behind = background_rect(&iface);
    let background = behind.is_some();
    let geometry = startup_geometry(&iface, behind, size, start_on);
    let fullscreen = geometry.fullscreen;
    // A test session's window never takes the focus (the window drill's does: it drives the
    // window's own keys). Its board covers the monitor as a plain borderless window: winit
    // forces a window it creates fullscreen to the front (an Alt tap and SetForegroundWindow,
    // which a game on another monitor would feel) and activates it on every change of it.
    let quiet = crate::platform::quiet_session();
    let viewport = egui::ViewportBuilder::default()
        .with_title(window_title(&iface))
        .with_app_id("jai")
        .with_inner_size(geometry.size)
        .with_resizable(true)
        .with_min_inner_size(min)
        // A see-through background ("opacity") needs a transparent surface, but on Windows a
        // transparent window does not take the whole monitor in fullscreen (it stayed a ~60 %
        // rectangle in the top-left corner): the fullscreen board is opaque.
        .with_transparent(geometry.transparent)
        .with_decorations(!iface.frameless && !fullscreen && !iface.span_all_monitors && !background)
        .with_fullscreen(fullscreen && !quiet)
        .with_maximized(maximized && !fullscreen && !quiet && !background)
        .with_window_level(if background { egui::viewport::WindowLevel::Normal } else { level })
        .with_icon(Arc::new(window_icon().unwrap_or_default()))
        // (As the background he never takes the keyboard.)
        .with_active(!quiet && !background);
    // Fullscreen: pinned to its monitor's full resolution (the primary's at (0, 0), or the one
    // `--monitor` named) from the first frame.
    let viewport = match geometry.position {
        Some(at) => viewport.with_position(at),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: placement.is_none() && !fullscreen && start_on.is_none(),
        persist_window: false,
        #[cfg(not(windows))]
        event_loop_builder: on_x11(),
        ..Default::default()
    };
    if fullscreen {
        let [x, y] = geometry.position.unwrap_or([0.0, 0.0]);
        info!("[HUD] Fullscreen board: {:.0}×{:.0} at ({x:.0}, {y:.0}), opaque", geometry.size.x, geometry.size.y);
    }
    if let Some(on) = start_on {
        info!("[HUD] This session opens on monitor {} (--monitor), at ({:.0}, {:.0})", START_MONITOR.load(Ordering::Acquire), on.left(), on.top());
    }
    let restore = placement.filter(|_| !maximized && !fullscreen && start_on.is_none() && !background);
    let (app, rt, model) = (app.clone(), rt.clone(), Arc::clone(model));
    eframe::run_native(
        "J.A.I.",
        options,
        Box::new(move |cc| Ok(Box::new(HudApp::new(cc, app, rt, model, restore)))),
    )
    .map_err(|e| e.to_string())
}

fn window_title(iface: &InterfaceSettings) -> String {
    if iface.title == interface::DEFAULT_TITLE {
        WINDOW_TITLE.to_string()
    } else {
        format!("{} HUD", iface.title)
    }
}

/// Window size that suits a layout (logical points at 100 % zoom).
fn layout_size(layout: HudLayout) -> Vec2 {
    match layout {
        HudLayout::Full => vec2(1500.0, 940.0),
        HudLayout::Compact => vec2(760.0, 900.0),
        HudLayout::Orb => vec2(360.0, 420.0),
    }
}

/// The smallest window a layout still works in: the full and compact HUD reflow down to a
/// compact side window; the floating orb can be tiny.
fn min_size(layout: HudLayout) -> Vec2 {
    match layout {
        HudLayout::Full | HudLayout::Compact => vec2(640.0, 420.0),
        HudLayout::Orb => vec2(220.0, 240.0),
    }
}

/// The gyroscope emblem icon (256 px) for the HUD and JAI's other windows; rendered once, as
/// the HUD opens (rendered at the first detach instead, it froze the HUD for a second).
pub(crate) fn window_icon() -> Option<egui::IconData> {
    static ICON: std::sync::OnceLock<egui::IconData> = std::sync::OnceLock::new();
    Some(ICON.get_or_init(crate::app_icon::window_icon).clone())
}

/// That icon as the one `Arc` every window of JAI's own carries (`for_test_session`).
fn shared_icon() -> Option<Arc<egui::IconData>> {
    static ICON: std::sync::OnceLock<Option<Arc<egui::IconData>>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| window_icon().map(Arc::new)).clone()
}

/// How the HUD window starts.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StartupGeometry {
    fullscreen: bool,
    transparent: bool,
    /// Logical points.
    size: Vec2,
    position: Option<[f32; 2]>,
}

/// As the desktop's background (`behind`, the rect it covers in pixels): the plain, opaque,
/// borderless window that the desktop is handed before it is first shown (`HudApp::new`).
/// Without one, the fullscreen board: borderless, opaque, the primary monitor's full resolution
/// at (0, 0); otherwise the layout's window size, transparent. `on`: the monitor a test session
/// opens on (`--monitor`, logical points): the board covers that one, a window starts in its
/// middle.
fn startup_geometry(iface: &InterfaceSettings, behind: Option<[i32; 4]>, windowed: Vec2, on: Option<Rect>) -> StartupGeometry {
    if let Some([l, t, r, b]) = behind {
        let scale = wm::primary_screen().2.max(0.1);
        return StartupGeometry { fullscreen: false, transparent: false, size: vec2((r - l) as f32 / scale, (b - t) as f32 / scale), position: Some([l as f32 / scale, t as f32 / scale]) };
    }
    let spanning = iface.span_all_monitors;
    let fullscreen = iface.fullscreen && !spanning;
    if !fullscreen && !spanning {
        let position = on.map(|m| {
            let size = windowed.min(m.size());
            [m.center().x - size.x / 2.0, m.center().y - size.y / 2.0]
        });
        return StartupGeometry { fullscreen, transparent: true, size: windowed, position };
    }
    if let (Some(m), false) = (on, spanning) {
        return StartupGeometry { fullscreen, transparent: false, size: m.size(), position: Some([m.left(), m.top()]) };
    }
    // One board across every monitor starts as the opaque borderless board on the first one
    // (never winit's fullscreen); `enforce_fullscreen` stretches it over the others at once.
    let (w, h) = primary_screen_points().unwrap_or((windowed.x, windowed.y));
    StartupGeometry { fullscreen, transparent: false, size: vec2(w, h), position: Some([0.0, 0.0]) }
}

/// The screen rect (left, top, right, bottom, physical pixels) he covers as the desktop's
/// background: every monitor or Sir's first, as he has it set. A scripted session's covers
/// only the monitor that session was given (`--monitor`, or `JAI_BACKDROP_MONITOR`): never one
/// Sir works on; without one it has no background.
fn background_rect(iface: &InterfaceSettings) -> Option<[i32; 4]> {
    let monitors = crate::monitors::recent();
    if crate::platform::in_test_session() {
        let named = std::env::var("JAI_BACKDROP_MONITOR").ok().and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or_else(|| START_MONITOR.load(Ordering::Acquire));
        return monitors.iter().find(|m| m.index == named && named > 0).map(|m| m.rect);
    }
    crate::wallpaper::rect_for(iface.background_all_monitors, &monitors)
}

/// The primary monitor in logical points (`GetSystemMetrics` pixels over the system DPI scale).
fn primary_screen_points() -> Option<(f32, f32)> {
    let (w, h, scale) = wm::primary_screen();
    (w > 0 && h > 0).then(|| (w as f32 / scale, h as f32 / scale))
}

fn native_window(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|handle| handle.as_raw()) {
        #[cfg(windows)]
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        #[cfg(not(windows))]
        Ok(RawWindowHandle::Xlib(handle)) => handle.window as isize,
        #[cfg(not(windows))]
        Ok(RawWindowHandle::Xcb(handle)) => handle.window.get() as isize,
        _ => 0,
    }
}

/// On Linux his windows run on X11 (XWayland under KDE Plasma) when there is one: there a program
/// may place, raise and pin its windows, which Wayland lets none do.
#[cfg(not(windows))]
fn on_x11() -> Option<eframe::EventLoopBuilderHook> {
    use winit::platform::x11::EventLoopBuilderExtX11;
    std::env::var_os("DISPLAY").map(|_| {
        Box::new(|builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>| {
            builder.with_x11();
        }) as eframe::EventLoopBuilderHook
    })
}

// ----------------------------------------------------------------------------- model ----

#[derive(Clone, Copy, PartialEq)]
enum Speaker {
    Sir,
    Jai,
    System,
}

struct ChatLine {
    id: String,
    who: Speaker,
    /// The bubble's header: "YOU" or "J.A.I." (empty for system lines).
    meta: String,
    /// A tiny pill beside the header: the latency ("140 ms"), "not for JAI", a time.
    pill: Option<String>,
    /// Everything technical (source, language, wake word, STT timing, model) — on hover only.
    detail: String,
    text: String,
    streaming: bool,
    /// Heard without the wake word.
    faded: bool,
    interrupted: bool,
    /// The start of a sentence Sir is still finishing (replaced by the stitched transcript).
    partial: bool,
    /// A WhatsApp message waiting for Sir's go-ahead.
    /// When the line arrived ("14:02:11"), for the text console log.
    at: String,
    card: Option<MessageCardView>,
    /// A saved instant-replay video with its open buttons.
    clip: Option<ClipCardView>,
    /// A PC shutdown / restart waiting for Sir's double-check.
    power: Option<PowerCardView>,
}

#[derive(Clone)]
struct PowerCardView {
    id: String,
    /// "shutdown" or "restart".
    action: String,
    /// The confirm button ("Confirm PC Shutdown").
    label: String,
    /// pending, confirmed, cancelled or expired.
    status: String,
}

#[derive(Clone)]
struct ClipCardView {
    video: String,
    folder: String,
    seconds: u32,
}

#[derive(Clone)]
struct MessageCardView {
    id: String,
    recipient: String,
    phone: String,
    text: String,
    status: String,
}

struct ToolLine {
    id: usize,
    tool: String,
    summary: String,
    details: String,
    meta: String,
    success: bool,
}

struct Toast {
    text: String,
    error: bool,
    since: Instant,
    until: Instant,
}

/// Everything the HUD shows, kept current by `feed_model`.
#[derive(Default)]
struct HudModel {
    state: String,
    detail: String,
    window_until: Option<Instant>,
    window_total_ms: u64,
    level: Option<AudioLevel>,
    vad_note: String,
    messages: VecDeque<ChatLine>,
    /// Last turn: speech-to-text, first token, first audio (ms after the end of speech).
    latency: [Option<u64>; 3],
    tools: VecDeque<ToolLine>,
    tool_count: usize,
    telemetry: Option<SystemTelemetry>,
    memories: Vec<MemoryEntry>,
    memory_query: Option<String>,
    skills: Vec<JaiSkill>,
    cfg: Option<ConfigState>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    models: Vec<String>,
    toasts: VecDeque<Toast>,
    offline: bool,
    /// The acoustic radar heard a knock: the HUD flashes amber until then.
    room_alert: Option<(String, Instant)>,
    /// A background module's signal ("BRAIN → WEBUNTIS", alert) for the telemetry strip.
    signal: Option<(String, bool, Instant)>,
    /// The badge of the last silent quick command ("Paused") and until when it shows.
    quick: Option<(String, bool, Instant)>,
}

impl HudModel {
    fn apply(&mut self, ev: HudEvent) {
        let now = Instant::now();
        match ev {
            HudEvent::State { state, detail, window_ms } => {
                self.detail = if detail == "listening_active" { "Listening — speak now".into() } else { detail };
                if window_ms > 0 {
                    self.window_until = Some(Instant::now() + Duration::from_millis(window_ms));
                    self.window_total_ms = self.window_total_ms.max(window_ms);
                } else if state != "listening" {
                    self.window_until = None;
                }
                self.state = state;
            }
            HudEvent::AudioLevel(level) => self.level = Some(level),
            HudEvent::VadEvent { event, detail } => {
                let label = match event.as_str() {
                    "listening_active" => "speech started",
                    "speech_ended" => "end of speech",
                    "barge_in" => "barge-in",
                    "discarded" => "· noise discarded",
                    "calibrated" => "OK: calibrated",
                    other => other,
                };
                self.vad_note = if detail.is_empty() { label.to_string() } else { format!("{label} — {detail}") };
            }
            HudEvent::Transcript { text, language, source, wake_word, stt_ms, audio_ms } => {
                let ignored = source == "ignored";
                // The technical story of the line stays in the tooltip; the bubble says "YOU".
                let mut detail = vec![format!("Heard via {source}"), language];
                match wake_word {
                    // An ignored line carries why: no wake word, or chatter over a game / Discord.
                    Some(reason) if ignored => detail.push(format!("not addressed · {reason}")),
                    Some(word) => detail.push(format!("wake word: {word}")),
                    None if ignored => detail.push("not addressed".into()),
                    None => {}
                }
                if stt_ms > 0 {
                    detail.push(format!("speech-to-text {stt_ms} ms for {:.1} s of audio", audio_ms as f32 / 1000.0));
                    self.latency = [Some(stt_ms), None, None];
                }
                let detail = detail.join(" · ");
                let pill = if ignored {
                    Some("not for JAI".to_string())
                } else {
                    (stt_ms > 0).then(|| format!("{stt_ms} ms"))
                };
                let meta = "YOU".to_string();
                // The rest of an unfinished sentence replaces its first half.
                let partial = self.messages.iter().rposition(|l| l.who == Speaker::Sir && l.partial);
                match partial.filter(|_| source == "stitched") {
                    Some(i) => {
                        let line = &mut self.messages[i];
                        line.text = text;
                        line.meta = meta;
                        line.pill = pill;
                        line.detail = detail;
                        line.partial = false;
                    }
                    None => self.push(ChatLine {
                        id: String::new(),
                        who: Speaker::Sir,
                        pill,
                        detail,
                        meta,
                        text,
                        streaming: false,
                        faded: ignored,
                        interrupted: false,
                        partial: false,
                        at: clock_now(),
                        card: None,
                        clip: None,
                        power: None,
                    }),
                }
            }
            HudEvent::ChatHistory { lines } => {
                if self.messages.is_empty() && !lines.is_empty() {
                    for (time, role, text) in lines {
                        let sir = role == "user";
                        self.push(ChatLine {
                            id: String::new(),
                            who: if sir { Speaker::Sir } else { Speaker::Jai },
                            pill: Some(time.get(11..16).unwrap_or(&time).to_string()),
                            detail: format!("An earlier session, {time}"),
                            meta: if sir { "YOU".to_string() } else { "J.A.I.".to_string() },
                            text,
                            streaming: false,
                            faded: true,
                            interrupted: false,
                            partial: false,
                            at: clock_now(),
                            card: None,
                        clip: None,
                        power: None,
                        });
                    }
                    self.push(ChatLine {
                        id: String::new(),
                        who: Speaker::System,
                        pill: None,
                        detail: String::new(),
                        meta: String::new(),
                        text: "— new session —".into(),
                        streaming: false,
                        faded: false,
                        interrupted: false,
                        partial: false,
                        at: clock_now(),
                        card: None,
                        clip: None,
                        power: None,
                    });
                }
            }
            HudEvent::Retracted { message_id } => {
                if !message_id.is_empty() {
                    self.messages.retain(|l| !(l.who == Speaker::Jai && l.id == message_id));
                }
                if let Some(line) = self.messages.iter_mut().rev().find(|l| l.who == Speaker::Sir) {
                    line.partial = true;
                }
            }
            HudEvent::FirstToken { message_id, latency_ms, since_speech_ms, model } => {
                self.latency[1] = Some(since_speech_ms);
                self.reply(&message_id).detail = format!("{model} · first token after {latency_ms} ms");
            }
            HudEvent::Token { message_id, token } => self.reply(&message_id).text.push_str(&token),
            HudEvent::AssistantDone { message_id, text, latency_ms, model, interrupted } => {
                let line = self.reply(&message_id);
                if line.text.trim().is_empty() {
                    line.text = text;
                }
                line.streaming = false;
                line.interrupted = interrupted;
                line.meta = "J.A.I.".to_string();
                line.pill = if interrupted { Some("interrupted".to_string()) } else { (latency_ms > 0).then(|| format!("{latency_ms} ms")) };
                line.detail = format!(
                    "{model}{}{}",
                    if latency_ms > 0 { format!(" · answered in {latency_ms} ms") } else { String::new() },
                    if interrupted { " · interrupted" } else { "" }
                );
                if line.text.trim().is_empty() {
                    self.messages.retain(|l| l.id != message_id);
                }
            }
            HudEvent::TurnMetrics { stt_ms, first_token_ms, first_audio_ms } => {
                self.latency = [Some(stt_ms), first_token_ms, Some(first_audio_ms)];
            }
            HudEvent::ToolLog { tool, args, result, success, duration_ms, timestamp } => {
                self.tool_count += 1;
                let summary: String = result.chars().take(180).collect();
                self.tools.push_front(ToolLine {
                    id: self.tool_count,
                    summary: if summary.len() < result.len() { format!("{summary}…") } else { summary },
                    details: format!("args: {}\n\n{result}", serde_json::to_string_pretty(&args).unwrap_or_default()),
                    meta: format!("{timestamp} · {duration_ms} ms"),
                    tool: tool.clone(),
                    success,
                });
                self.tools.truncate(MAX_TOOL_LINES);
                // "Clip that": the saved video gets its own card with open buttons.
                if tool == "save_instant_replay" && success {
                    let field = |key: &str| result.lines().find_map(|l| l.trim().strip_prefix(key)).map(|v| v.trim().to_string());
                    if let Some(video) = field("Video:") {
                        let folder = field("Folder:").unwrap_or_else(|| std::path::Path::new(&video).parent().map(|p| p.display().to_string()).unwrap_or_default());
                        let seconds = args.get("seconds").and_then(serde_json::Value::as_u64).unwrap_or(30) as u32;
                        self.push(ChatLine {
                            id: String::new(),
                            who: Speaker::System,
                            pill: None,
                            detail: String::new(),
                            meta: "INSTANT REPLAY".into(),
                            text: String::new(),
                            streaming: false,
                            faded: false,
                            interrupted: false,
                            partial: false,
                            at: clock_now(),
                            card: None,
                            clip: Some(ClipCardView { video, folder, seconds }),
                            power: None,
                        });
                    }
                }
            }
            HudEvent::SystemMetrics { telemetry } => self.telemetry = Some(telemetry),
            HudEvent::Memories { items, query } => {
                self.memories = items;
                self.memory_query = query.filter(|q| !q.trim().is_empty());
            }
            HudEvent::Skills { items } => self.skills = items,
            HudEvent::ConfigState(cfg) => {
                self.window_total_ms = cfg.window_secs * 1000;
                self.cfg = Some(cfg);
            }
            HudEvent::Devices { inputs, outputs } => {
                self.inputs = inputs;
                self.outputs = outputs;
            }
            HudEvent::Models { items, current } => self.models = if items.is_empty() { vec![current] } else { items },
            HudEvent::Notice { message } => self.toast(message, false),
            HudEvent::Guardian { message } => self.toast(message, true),
            HudEvent::Error { message } => {
                self.toast(message.clone(), true);
                self.push(ChatLine {
                    id: String::new(),
                    who: Speaker::System,
                    pill: None,
                    detail: String::new(),
                    meta: String::new(),
                    text: format!("WARN: {message}"),
                    streaming: false,
                    faded: false,
                    interrupted: false,
                    partial: false,
                    at: clock_now(),
                    card: None,
                        clip: None,
                        power: None,
                });
            }
            HudEvent::CorePulse { path, alert } => self.signal = Some((path.join(" → ").to_uppercase(), alert, now + Duration::from_secs(4))),
            HudEvent::MessageCard { id, recipient, phone, text, status } => {
                match self.messages.iter_mut().rev().find(|l| l.card.as_ref().is_some_and(|c| c.id == id)) {
                    Some(line) => {
                        if let Some(c) = line.card.as_mut() {
                            c.status = status;
                        }
                    }
                    None => self.push(ChatLine {
                        id: id.clone(),
                        who: Speaker::System,
                        pill: None,
                        detail: String::new(),
                        meta: "WHATSAPP · MESSAGE CARD".into(),
                        text: String::new(),
                        streaming: false,
                        faded: false,
                        interrupted: false,
                        partial: false,
                        at: clock_now(),
                        card: Some(MessageCardView { id, recipient, phone, text, status }),
                        clip: None,
                        power: None,
                    }),
                }
            }
            HudEvent::RoomAlert { message } => {
                self.room_alert = Some((message.clone(), now + Duration::from_secs(8)));
                self.toast(message, true);
            }
            HudEvent::QuickAction { label, ok } => self.quick = Some((label, ok, now + Duration::from_secs(3))),
            HudEvent::PowerCard { id, action, label, status } => {
                match self.messages.iter_mut().rev().find(|l| l.power.as_ref().is_some_and(|c| c.id == id)) {
                    Some(line) => {
                        if let Some(c) = line.power.as_mut() {
                            c.status = status;
                        }
                    }
                    None if status == "pending" => self.push(ChatLine {
                        id: id.clone(),
                        who: Speaker::System,
                        pill: None,
                        detail: String::new(),
                        meta: "SYSTEM POWER · CONFIRMATION".into(),
                        text: String::new(),
                        streaming: false,
                        faded: false,
                        interrupted: false,
                        partial: false,
                        at: clock_now(),
                        card: None,
                        clip: None,
                        power: Some(PowerCardView { id, action, label, status }),
                    }),
                    None => {}
                }
            }
            HudEvent::Shutdown => self.offline = true,
        }
    }

    fn push(&mut self, line: ChatLine) {
        self.messages.push_back(line);
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
    }

    /// JAI's (possibly still streaming) reply with this id.
    fn reply(&mut self, id: &str) -> &mut ChatLine {
        match self.messages.iter().rposition(|l| l.id == id && l.who == Speaker::Jai) {
            Some(i) => &mut self.messages[i],
            None => {
                self.push(ChatLine {
                    id: id.to_string(),
                    who: Speaker::Jai,
                    pill: None,
                    detail: String::new(),
                    meta: "J.A.I.".into(),
                    text: String::new(),
                    streaming: true,
                    faded: false,
                    interrupted: false,
                    partial: false,
                    at: clock_now(),
                    card: None,
                        clip: None,
                        power: None,
                });
                self.messages.back_mut().expect("just pushed")
            }
        }
    }

    /// The notifications waited behind the start-up sequence: their time starts now.
    fn restart_toasts(&mut self) {
        let now = Instant::now();
        for t in &mut self.toasts {
            let life = t.until.saturating_duration_since(t.since);
            t.since = now;
            t.until = now + life;
        }
    }

    fn toast(&mut self, text: String, error: bool) {
        // Into the history ("what did I miss?"), and the screen's edges light up.
        crate::hud_notify::record(&text, error);
        let since = Instant::now();
        let stays = crate::hud_notify::stays(error, crate::hud_feel::night_active());
        // A notice from the source of the card still on screen joins that card (its newest
        // lines, and how many there were) instead of stacking another one on it.
        if !error {
            if let Some(last) = self.toasts.back_mut().filter(|t| !t.error && t.until > since) {
                if let Some(joined) = crate::hud_notify::grouped(&last.text, &text) {
                    last.text = joined;
                    last.until = since + stays;
                    return;
                }
            }
        }
        self.toasts.push_back(Toast { text, error, since, until: since + stays });
        while self.toasts.len() > 4 {
            self.toasts.pop_front();
        }
    }
}

// --------------------------------------------------------------------------- palette ----

fn rgb([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// `Stroke::new` takes `impl Into<f32>`, which leaves float literals untyped.
use crate::holo::stroke;

fn rgba([r, g, b]: [u8; 3], alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Colours derived from the interface settings.
#[derive(Clone, Copy)]
struct Palette {
    accent: Color32,
    /// Window background (with the configured opacity).
    bg: Color32,
    /// Card fill (stays more opaque than the background, so text keeps its contrast).
    card: Color32,
    /// Opaque card colour for popups, menus and dialogs.
    card_solid: Color32,
    /// Inputs and list items.
    well: Color32,
    well_solid: Color32,
    line: Color32,
    line_strong: Color32,
    text: Color32,
    muted: Color32,
    /// All is well: plain ink (colour is for what is live or wrong).
    nominal: Color32,
    /// A caution, never the theme's own colour.
    warn: Color32,
    /// What failed.
    alert: Color32,
    /// The same light further back: a second series, a counterpart.
    deep: Color32,
}

impl Palette {
    /// The stock controls' colours from the live theme: ink for structure and text, the
    /// accent for what is chosen, glass for surfaces. `opacity` thins the canvas only.
    fn new(s: &InterfaceSettings) -> Self {
        let t = crate::holo::theme();
        let o = s.opacity as f32 / 100.0;
        let glass = t.glass();
        let solid = Color32::from_rgb(glass.r(), glass.g(), glass.b());
        let well = blend(t.void(), t.ink(), 0.07);
        Self {
            accent: t.accent(),
            bg: rgba(t.void, o),
            card: glass.gamma_multiply(0.62 + 0.38 * o),
            card_solid: solid,
            well: well.gamma_multiply(0.7 + 0.3 * o),
            well_solid: well,
            line: crate::holo::a(t.ink(), 0.20),
            line_strong: crate::holo::a(t.ink(), 0.46),
            text: t.ink(),
            muted: t.muted(),
            nominal: t.ink(),
            warn: t.warn(),
            alert: t.alert(),
            deep: t.deep(),
        }
    }

    /// The pop-up cards' look.
    fn card_style(&self) -> crate::dynamic_popups::CardStyle {
        crate::dynamic_popups::CardStyle { surface: self.card_solid, accent: self.accent, text: self.text, muted: self.muted }
    }
}

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    rgb(interface::mix([a.r(), a.g(), a.b()], [b.r(), b.g(), b.b()], t))
}

/// egui's own controls in the hologram look: sharp corners, hairlines of ink, the accent for
/// what is chosen or under the pointer, glass for menus and windows, no drop shadows.
fn visuals(p: &Palette) -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(p.text);
    // Panels are transparent: the canvas (with Sir's opacity) is painted underneath.
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.card_solid;
    v.extreme_bg_color = p.well_solid;
    v.faint_bg_color = p.well_solid;
    v.code_bg_color = p.well_solid;
    v.window_stroke = stroke(1.0, p.line_strong);
    v.window_rounding = Rounding::ZERO;
    v.menu_rounding = Rounding::ZERO;
    v.window_shadow = egui::epaint::Shadow { offset: vec2(0.0, 4.0), blur: 22.0, spread: 0.0, color: Color32::from_black_alpha(150) };
    v.popup_shadow = egui::epaint::Shadow { offset: vec2(0.0, 3.0), blur: 14.0, spread: 0.0, color: Color32::from_black_alpha(140) };
    v.selection.bg_fill = p.accent.gamma_multiply(0.30);
    v.selection.stroke = stroke(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.35 };
    v.text_cursor.stroke = stroke(2.0, p.accent);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card_solid;
    w.noninteractive.weak_bg_fill = p.card_solid;
    w.noninteractive.bg_stroke = stroke(1.0, p.line);
    w.noninteractive.fg_stroke = stroke(1.0, p.text);
    w.inactive.bg_fill = p.well_solid;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = stroke(1.0, p.line_strong.gamma_multiply(0.7));
    w.inactive.fg_stroke = stroke(1.0, p.text);
    w.hovered.bg_fill = blend(p.well_solid, p.accent, 0.16);
    w.hovered.weak_bg_fill = blend(p.well_solid, p.accent, 0.10);
    w.hovered.bg_stroke = stroke(1.0, p.accent);
    w.hovered.fg_stroke = stroke(1.0, p.text);
    w.active.bg_fill = p.accent;
    w.active.weak_bg_fill = p.accent;
    w.active.bg_stroke = stroke(1.0, p.accent);
    w.active.fg_stroke = stroke(1.0, Color32::BLACK);
    w.open = w.hovered;
    for widget in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        widget.rounding = Rounding::ZERO;
    }
    v
}


fn label_family() -> FontFamily {
    FontFamily::Name("label".into())
}

fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

thread_local! {
    /// Text size factor of the window being drawn: 1.0 for the HUD (its zoom is egui's), a
    /// JAI window's own `font_scale` while that window draws (egui's zoom is shared by all
    /// windows). Every viewport is drawn on the GUI thread, one after the other.
    static TEXT_SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// A font size in the current window's text scale.
fn ts(size: f32) -> f32 {
    size * TEXT_SCALE.with(|s| s.get())
}

fn bold(size: f32) -> FontId {
    FontId::new(ts(size), bold_family())
}

/// Text styles at the interface's font scale (on top of the zoom).
fn configure_style(ctx: &egui::Context, font_scale: f32) {
    use egui::TextStyle;
    let f = font_scale.clamp(0.5, 2.0);
    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(10.5 * f)),
            (TextStyle::Body, FontId::proportional(13.5 * f)),
            (TextStyle::Button, FontId::new(13.0 * f, label_family())),
            (TextStyle::Heading, FontId::new(17.0 * f, FontFamily::Name("display".into()))),
            (TextStyle::Monospace, FontId::monospace(12.0 * f)),
        ]
        .into();
        s.spacing.item_spacing = vec2(8.0, 6.0);
        // A scroll bar is a hairline of ink over the content's edge, brighter under the pointer.
        s.spacing.scroll = egui::style::ScrollStyle { bar_width: 3.0, floating_width: 2.0, handle_min_length: 14.0, foreground_color: true, dormant_handle_opacity: 0.25, active_handle_opacity: 0.6, interact_handle_opacity: 1.0, dormant_background_opacity: 0.0, active_background_opacity: 0.0, interact_background_opacity: 0.0, ..egui::style::ScrollStyle::floating() };
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size.y = 24.0 * f.max(1.0);
    });
    // Ctrl + Plus / Minus / 0 change the interface zoom (persisted), handled in `handle_keys`.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

// ------------------------------------------------------------------------- the reactor ----

/// Animation state of the core (the node graph and its centre).
#[derive(Clone, Copy, Default)]
struct Reactor {
    phase: f32,
    energy: f32,
    /// Rotation of the dashed ring and the slow drift of the node circle (radians).
    spin: [f32; 3],
    /// Smoothed loudness of Sir's microphone and of JAI's voice (0-1): the strings vibrate
    /// with them.
    mic: f32,
    voice: f32,
}

impl Reactor {
    /// What the emblem's voice equaliser follows: JAI's own voice only (his TTS playback
    /// level) — Sir talking into the microphone leaves it calm.
    fn emblem_voice(&self) -> f32 {
        self.voice
    }

    fn step(&mut self, state: &str, level: Option<&AudioLevel>, dt: f32) {
        self.step_with(state, level, dt, crate::tts::output_level());
    }

    /// `step` with JAI's playback level given (`tts::output_level()`, 0-1 of full scale).
    fn step_with(&mut self, state: &str, level: Option<&AudioLevel>, dt: f32, playback: f32) {
        let target = match state {
            "speaking" => 0.45 + (self.phase * 3.2).sin() * 0.2,
            "thinking" => 0.3 + (self.phase * 6.0).sin() * 0.08,
            "interrupted" => 0.8,
            _ => level
                .filter(|l| l.phase != "muted")
                .map(|l| ((l.rms_db + 70.0) / 50.0).clamp(0.06, 1.0))
                .unwrap_or(0.06),
        };
        let follow = |current: &mut f32, target: f32, rate: f32| *current += (target - *current) * (1.0 - rate.powf(dt * 60.0));
        follow(&mut self.energy, target, 0.82);
        let mic = level.filter(|l| l.phase != "muted").map(|l| ((l.rms_db + 62.0) / 42.0).clamp(0.0, 1.0)).unwrap_or(0.0);
        let voice = (playback * 6.0).clamp(0.0, 1.0);
        // Quick attack, slower release, like a VU meter.
        let (mic_rate, voice_rate) = (if mic > self.mic { 0.55 } else { 0.9 }, if voice > self.voice { 0.5 } else { 0.88 });
        follow(&mut self.mic, mic, mic_rate);
        follow(&mut self.voice, voice, voice_rate);
        self.phase += dt * 2.7;
        // Periods in seconds (negative = anticlockwise), faster in the matching state.
        let periods = [
            if state == "speaking" { 5.0 } else { 18.0 },
            if state == "thinking" { -2.4 } else { -11.0 },
            if state == "listening" { 60.0 } else { 240.0 },
        ];
        for (angle, period) in self.spin.iter_mut().zip(periods) {
            *angle = (*angle + dt * TAU / period).rem_euclid(TAU);
        }
    }
}

/// What the emblem's voice ticks follow after a second of `state` with JAI's own playback at
/// `jai` (0-1) and Sir's microphone at `mic` (0-1): the reactor's own mapping, as the visual
/// verifier asks it. The microphone never reaches the emblem.
pub(crate) fn emblem_voice_of(state: &str, jai: f32, mic: f32) -> f32 {
    let level = AudioLevel { rms: mic, rms_db: -62.0 + 42.0 * mic.clamp(0.0, 1.0), probability: mic, noise_floor: 0.0, start_gate: 0.0, start_threshold: 0.0, release_threshold: 0.0, phase: "listening", speaking: mic > 0.5 };
    let mut reactor = Reactor::default();
    for _ in 0..60 {
        reactor.step_with(state, Some(&level), 1.0 / 60.0, jai / 6.0);
    }
    reactor.emblem_voice()
}

pub(crate) use features_view::TryPlan;

/// What the Feature Lab's button does for the tool `def` (the exhaustive verifier presses
/// every one of them).
pub(crate) fn lab_plan(def: &serde_json::Value) -> TryPlan {
    features_view::try_plan(def)
}

/// The HUD's look for a context that is not the HUD's window (the visual verifier's stage):
/// the same visuals and text styles, in the active theme.
pub(crate) fn apply_hud_visuals(ctx: &egui::Context) {
    ctx.set_visuals(visuals(&Palette::new(&InterfaceSettings::default())));
}

/// egui measures a window, a menu or a tooltip invisibly in the pass that first shows it, and
/// for that one pass its hit area is a guess (500 x 400 points, pushed back inside the screen)
/// that can lie over the pointer and take the press meant for what is under it. A pass that has
/// measured something new is therefore run again at once: the next press is tested against
/// what is on screen, and nothing appears a frame late. Called at the end of every pass.
pub(crate) fn settle_new_areas(ctx: &egui::Context) {
    let layers = ctx.memory(|m| m.layer_ids().len());
    let seen = ctx.data_mut(|d| std::mem::replace(d.get_temp_mut_or_insert_with(egui::Id::new("jai_layers_seen"), || 0usize), layers));
    if layers > seen {
        ctx.request_discard("a new area was measured");
    }
}


fn state_name(state: &str) -> &str {
    match state {
        "" | "idle" => "Idle",
        "listening" => "Listening",
        "thinking" => "Thinking",
        "speaking" => "Speaking",
        "interrupted" => "Interrupted",
        other => other,
    }
}

// --------------------------------------------------------------------------- widgets ----
// What the drawer's cards and the panels are made of, all on the kit (`holo_ui`, `holo_kit`).

#[derive(Clone, Copy)]
enum Tone {
    Normal,
    On,
    Danger,
}

/// A key: plain, solid (its thing is on), or in the alert colour (it ends or deletes something).
fn button(ui: &mut Ui, text: &str, tone: Tone) -> egui::Response {
    match tone {
        Tone::Normal => holo_ui::key(ui, text),
        Tone::On => holo_ui::key_on(ui, text, true),
        Tone::Danger => holo_ui::key_alert(ui, text),
    }
}

/// A reading in a small outlined tag (the data face).
fn pill(ui: &mut Ui, text: impl Into<String>, colour: Color32) -> egui::Response {
    let text = text.into();
    let g = ui.painter().layout_no_wrap(text, FontId::monospace(ts(11.0)), colour);
    let (rect, response) = ui.allocate_exact_size(g.size() + vec2(14.0, 6.0), Sense::hover());
    holo::outline(ui.painter(), &holo::plate(rect, [0.0, 0.0, 4.0, 0.0]), holo::a(holo::theme().ink(), 0.22));
    ui.painter().galley(rect.min + vec2(7.0, 3.0), g, colour);
    response
}

/// A state as a tag in wide capitals, outlined in its colour.
fn chip(ui: &mut Ui, text: &str, colour: Color32) {
    let label = text.replace('_', " ");
    let width = holo::galley(ui.painter(), Face::Label, &label, 8.0, colour).size().x + 12.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
    holo_kit::chip(ui.painter(), rect.min, &label, colour, false, 1.0);
}

/// A part of a panel: a tick of the accent, its name in wide capitals, what belongs at its
/// right (`aux`), a rule between them, and its body under it. Nothing is boxed.
fn card<R>(ui: &mut Ui, title: &str, aux: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui) -> R) -> R {
    let t = holo::theme();
    ui.horizontal(|ui| {
        ui.set_width(ui.available_width());
        let (tick, _) = ui.allocate_exact_size(vec2(3.0, 11.0), Sense::hover());
        ui.painter().rect_filled(tick, Rounding::ZERO, t.accent());
        let name = ui.label(RichText::new(title.to_uppercase()).font(holo::font(Face::Label, ts(8.5))).color(t.ink()).extra_letter_spacing(1.2)).rect;
        let aux_left = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            aux(ui);
            ui.min_rect().left().min(ui.max_rect().right())
        });
        holo::rule(ui.painter(), pos2(name.right() + 10.0, name.center().y), pos2(aux_left.inner - 8.0, name.center().y), holo::a(t.ink(), 0.13));
    });
    ui.add_space(4.0);
    let inner = body(ui);
    ui.add_space(14.0);
    inner
}

/// A list entry inside a card: its content on a hairline, a bar on its left (`mark` colours
/// it: a state; else faint ink).
fn item(ui: &mut Ui, mark: Option<Color32>, body: impl FnOnce(&mut Ui)) {
    let t = holo::theme();
    let shown = Frame::none().inner_margin(Margin { left: 11.0, right: 4.0, top: 6.0, bottom: 6.0 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        body(ui);
    });
    let r = shown.response.rect;
    holo::rule(ui.painter(), r.left_bottom(), r.right_bottom(), holo::a(t.ink(), 0.08));
    ui.painter().rect_filled(Rect::from_min_max(r.left_top() + vec2(0.0, 4.0), pos2(r.left() + 2.0, r.bottom() - 4.0)), Rounding::ZERO, mark.unwrap_or(holo::a(t.ink(), 0.22)));
    ui.add_space(2.0);
}

fn muted(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).size(ts(11.5)).color(holo::theme().muted()));
}

/// A label and its value on one line, dots between them (`holo_kit::pair`); returns the row's
/// response (for tooltips).
fn row(ui: &mut Ui, key: &str, value: impl Into<String>) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        holo_kit::pair(ui.painter(), rect, key, &value.into(), 1.0);
    }
    response
}

/// A level (0..1) as a row of cells, with ticks at the `markers`' shares.
fn meter(ui: &mut Ui, fraction: f32, height: f32, fill: Color32, markers: &[(f32, Color32)]) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height.max(7.0)), Sense::hover());
    let shown = ui.ctx().animate_value_with_time(response.id.with("glide"), fraction.clamp(0.0, 1.0), crate::holo_motion::TOUCH);
    holo_kit::cells(ui.painter(), rect, shown, ((rect.width() / 5.0) as usize).max(8), fill, 1.0);
    for (at, colour) in markers {
        let x = holo::snap(rect.left() + rect.width() * at.clamp(0.0, 1.0), holo::ppp(ui.painter()));
        holo::rule(ui.painter(), pos2(x, rect.top() - 2.0), pos2(x, rect.bottom() + 2.0), *colour);
    }
}

/// A slider under its name, the value it has at the name's right.
fn slider(ui: &mut Ui, label: &str, shown: String, widget: holo_ui::Slider<'_>) -> egui::Response {
    row(ui, label, shown);
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(widget.show_value(false))
}

/// A picker whose first entry ("Auto …") stands for `None`.
fn device_combo(ui: &mut Ui, id: &str, auto: &str, devices: &[String], current: Option<&str>) -> Option<Option<String>> {
    let mut picked = None;
    holo_ui::Picker::from_id_salt(id).width(ui.available_width()).selected_text(current.unwrap_or(auto)).show_ui(ui, |ui| {
        if holo_ui::item(ui, auto, current.is_none()).clicked() {
            picked = Some(None);
        }
        for d in devices {
            if holo_ui::item(ui, d.as_str(), current == Some(d.as_str())).clicked() {
                picked = Some(Some(d.clone()));
            }
        }
    });
    picked
}

/// KB/s → "820 KB/s" / "4.2 MB/s".
fn rate(kbps: f32) -> String {
    if kbps >= 1024.0 {
        format!("{:.1} MB/s", kbps / 1024.0)
    } else {
        format!("{kbps:.0} KB/s")
    }
}

fn db_fraction(db: f32) -> f32 {
    ((db + 80.0) / 80.0).clamp(0.0, 1.0)
}

fn to_db(linear: f32) -> f32 {
    20.0 * linear.max(1e-6).log10()
}

// ---------------------------------------------------------------------------- the app ----

/// The tabs of the Settings drawer: everything technical lives here, out of the way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DrawerTab {
    Voice,
    System,
    Memory,
    Activity,
    Look,
}

impl HudApp {
    fn command(&self, cmd: HudCommand) {
        send(&self.app, &self.rt, &self.model, cmd);
    }

    fn set_interface(&self, mut settings: InterfaceSettings) {
        self.app.config().update(|c| {
            // (Mini mode borrows the orb's window: what it set for that is not Sir's setting.)
            if settings.orb_anchor == MINI_STRIP {
                settings.orb_mode = c.interface.orb_mode && !settings.mini_strip;
                settings.orb_anchor = c.interface.orb_anchor.clone();
                settings.orb_on_top = c.interface.orb_on_top;
            }
            settings.reconcile(&c.interface);
            c.interface = settings;
        });
    }

    /// The fullscreen board on monitor `index` (1-based; None or a monitor that is gone: the
    /// primary one), from wherever the window is: spanning every monitor, fullscreen on
    /// another one, or a window. winit makes a window fullscreen on the monitor it is on, so
    /// the window is put there as a small plain window first.
    fn fullscreen_on(&mut self, ctx: &egui::Context, index: Option<usize>) {
        // (A scripted session's board goes on that session's own monitor and on no other: on
        // 2026-11-13 a test's board, back from being the background, stood on Sir's main
        // monitor for a quarter of a minute.)
        let index = index.or_else(|| test_session_monitor().filter(|n| *n > 0));
        let monitors = crate::monitors::list();
        let Some(m) = index.and_then(|i| monitors.iter().find(|m| m.index == i)).or_else(|| crate::monitors::primary(&monitors)) else {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
            return;
        };
        let ppp = ctx.pixels_per_point();
        let points = |px: i32| px as f32 / ppp;
        let inset = (m.width().min(m.height()) / 8).max(40);
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(points(m.rect[0] + inset), points(m.rect[1] + inset))));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(points(m.width() - 2 * inset), points(m.height() - 2 * inset))));
        if crate::platform::quiet_session() {
            // A test session's board: `enforce_fullscreen` stretches the borderless window over
            // that monitor once it stands there (winit's fullscreen would take the focus).
            self.fullscreen_check = Instant::now().checked_sub(Duration::from_millis(1300)).unwrap_or_else(Instant::now);
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
            self.fullscreen_check = Instant::now();
        }
        self.fullscreen_fixes = 0;
        info!("[HUD] Fullscreen board on {}", m.describe());
    }

    /// Applies interface changes (from JAI or the Interface window) to the live window.
    fn sync_interface(&mut self, ctx: &egui::Context) -> InterfaceSettings {
        let mut current = self.app.config().read(|c| c.interface.clone());
        // Mini mode and pure orb mode are two shapes of the one window: asked for both, the one
        // asked for last stands.
        if current.mini_strip && current.orb_mode {
            let mini_is_new = self.applied.as_ref().is_some_and(|p| p.orb_mode && p.orb_anchor != MINI_STRIP);
            self.app.config().update(|c| if mini_is_new { c.interface.orb_mode = false } else { c.interface.mini_strip = false });
            current = self.app.config().read(|c| c.interface.clone());
        }
        // Mini mode: the window becomes the strip the way it becomes the orb (the orb's own
        // settings stay as they are in the configuration: `set_interface` never writes these).
        if current.mini_strip {
            current.orb_mode = true;
            current.orb_anchor = MINI_STRIP.to_string();
            current.orb_on_top = true;
        }
        // The HUD's colours (board, console, core, emblem) cross-fade to the chosen theme.
        if crate::theme_store::follow(&current.hud_palette) {
            info!("[THEME] {}", current.hud_palette);
        }
        let fading = crate::holo_theme::fading();
        if fading {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        // Every window's colours follow the theme's fade, not only the board.
        let generation = crate::holo_theme::generation();
        if self.applied.as_ref() == Some(&current) {
            // "Move yourself to monitor 2" while the fullscreen board is up.
            if let Some(index) = take_monitor_request(&current) {
                self.fullscreen_on(ctx, Some(index));
            }
            if fading || generation != self.theme_generation {
                self.theme_generation = generation;
                self.palette = Palette::new(&current);
                ctx.set_visuals(visuals(&self.palette));
            }
            return current;
        }
        self.theme_generation = generation;
        self.emblem.set_custom_home(current.orb_board_pos.map(|[x, y]| vec2(x, y)));
        self.emblem.style = crate::holo_core::CoreStyle::of_word(&current.core_style).unwrap_or_default();
        self.emblem.size = current.orb_scale;
        if let Some([x, y]) = current.orb_board_pos.filter(|_| self.applied.is_none()) {
            info!("[HUD] Emblem home: Sir's spot at {:.0} %, {:.0} % of the screen (double-click it, or \"center your orb\", for the middle)", x * 100.0, y * 100.0);
        }
        crate::hud_dock::set_orb_aside(current.orb_aside);
        crate::hud_dock::set_overlap_allowed(current.allow_window_overlap);
        crate::tts::set_cues(current.sound_cues);
        let previous = self.applied.replace(current.clone());
        self.palette = Palette::new(&current);
        ctx.set_visuals(visuals(&self.palette));
        ctx.set_zoom_factor(current.scale);
        if previous.as_ref().is_none_or(|p| p.font_scale != current.font_scale) {
            configure_style(ctx, current.font_scale);
        }
        // The window was created with the initial state; only changes need commands.
        if let Some(prev) = previous {
            if prev.layout != current.layout {
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(min_size(current.layout)));
            }
            // A turn of the mouse wheel over the orb is no news for the log.
            let quiet = InterfaceSettings { orb_scale: prev.orb_scale, ..current.clone() } == prev;
            // (As the desktop's background he has no level, frame or size of a window: what is
            // set meanwhile applies when he is a window again.)
            let in_desktop = self.as_background();
            if prev.always_on_top != current.always_on_top && !in_desktop {
                let level = if current.always_on_top {
                    egui::viewport::WindowLevel::AlwaysOnTop
                } else {
                    egui::viewport::WindowLevel::Normal
                };
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
            }
            // The window's three shapes: one board across every monitor, the fullscreen board on
            // one monitor, a (maximised) window. Each change of shape has one way through.
            // (And a fourth: the desktop's background, which is no window at all. Coming back from
            // it is like coming back from the span: `wallpaper::tick` has taken the window out
            // of the desktop by the time these commands are carried out.)
            let background = |i: &InterfaceSettings| background_rect(i).is_some();
            let shape = |i: &InterfaceSettings| (background(i), i.span_all_monitors, i.fullscreen);
            let (to_background, from_background) = (background(&current), background(&prev));
            if shape(&prev) != shape(&current) {
                let target = lock(&HUD_MONITOR).take();
                if to_background {
                    if !from_background {
                        // Behind every other window at once, before it becomes a plain window
                        // where it is to stand: it never lies over Sir's windows on the way.
                        // (Not hidden for that moment: a hidden window draws no frames, and
                        // the frames are what hand it to the desktop. Seen 2026-11-14: hidden,
                        // it waited eight seconds for the next order, and was hidden again by
                        // the helper that wakes a board hidden in the tray.)
                        desk::sink(self.hwnd);
                        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::viewport::WindowLevel::Normal));
                        if let Some([l, t, r, b]) = background_rect(&current) {
                            let ppp = ctx.pixels_per_point();
                            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(l as f32 / ppp, t as f32 / ppp)));
                            ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2((r - l) as f32 / ppp, (b - t) as f32 / ppp)));
                        }
                    }
                } else if from_background && !current.span_all_monitors && !current.fullscreen {
                    // Back as a window: where a plain window of the layout's size belongs.
                    ctx.send_viewport_cmd(ViewportCommand::Decorations(!current.frameless));
                    let monitors = crate::monitors::list();
                    let own = test_session_monitor().and_then(|n| monitors.iter().find(|m| m.index == n));
                    if let Some(m) = own.or_else(|| crate::monitors::primary(&monitors)) {
                        let ppp = ctx.pixels_per_point();
                        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2((m.rect[0] + 80) as f32 / ppp, (m.rect[1] + 60) as f32 / ppp)));
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(layout_size(current.layout) / current.scale));
                    }
                    if current.always_on_top {
                        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::viewport::WindowLevel::AlwaysOnTop));
                    }
                } else if current.span_all_monitors {
                    // "Use all monitors": borderless and not maximised; `enforce_fullscreen`
                    // stretches the window over the virtual desktop (now, and whenever it drifts).
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                    // Straight onto the box around the monitors, in the same breath: leaving
                    // fullscreen puts the window back where it was before it (a small window,
                    // which stood there for two seconds until the next check stretched it).
                    if let Some([x, y, w, h]) = crate::monitors::virtual_bounds(&crate::monitors::list()) {
                        let ppp = ctx.pixels_per_point();
                        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(x as f32 / ppp, y as f32 / ppp)));
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(w as f32 / ppp, h as f32 / ppp)));
                    }
                    // The check that it really covers them follows once those have been carried out.
                    self.fullscreen_check = Instant::now().checked_sub(Duration::from_millis(1300)).unwrap_or_else(Instant::now);
                    info!("[HUD] One board across every monitor");
                } else if current.fullscreen {
                    // Back from spanning (or from the desktop): Sir's first monitor (or the one
                    // he named). From a window: the monitor the window is on.
                    if prev.span_all_monitors || from_background || target.is_some() {
                        self.fullscreen_on(ctx, target);
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                        // (A test session's: the check below stretches the borderless window.)
                        if !crate::platform::quiet_session() {
                            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                        }
                        // A maximised window stays maximised underneath when it goes fullscreen;
                        // the check that lets go of that follows once the change is carried out.
                        self.fullscreen_check = Instant::now().checked_sub(Duration::from_millis(1300)).unwrap_or_else(Instant::now);
                    }
                } else {
                    // Fullscreen (or the span) → a maximised window ("windowed mode", F11).
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    if prev.span_all_monitors {
                        // The spanned window is as large as the desktop: a plain window on the
                        // first monitor is what gets maximised.
                        let monitors = crate::monitors::list();
                        if let Some(m) = crate::monitors::primary(&monitors) {
                            let ppp = ctx.pixels_per_point();
                            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2((m.rect[0] + 80) as f32 / ppp, (m.rect[1] + 60) as f32 / ppp)));
                            ctx.send_viewport_cmd(ViewportCommand::InnerSize(layout_size(current.layout) / current.scale));
                        }
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Decorations(!current.frameless));
                    if crate::platform::quiet_session() {
                        // Maximising activates a window: a test session's becomes a plain window
                        // of the layout's size near the corner of its monitor instead.
                        let monitors = crate::monitors::list();
                        let start = START_MONITOR.load(Ordering::Acquire);
                        if let Some(m) = monitors.iter().find(|m| m.index == start).or_else(|| crate::monitors::primary(&monitors)) {
                            let ppp = ctx.pixels_per_point();
                            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2((m.rect[0] + 80) as f32 / ppp, (m.rect[1] + 60) as f32 / ppp)));
                            ctx.send_viewport_cmd(ViewportCommand::InnerSize(layout_size(current.layout) / current.scale));
                        }
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                    }
                }
            } else if let Some(index) = take_monitor_request(&current) {
                self.fullscreen_on(ctx, Some(index));
            }
            if prev.frameless != current.frameless && !in_desktop {
                ctx.send_viewport_cmd(ViewportCommand::Decorations(!current.frameless));
            }
            if prev.title != current.title {
                ctx.send_viewport_cmd(ViewportCommand::Title(window_title(&current)));
            }
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            if prev.layout != current.layout && !current.fullscreen && !maximized && !in_desktop {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(layout_size(current.layout) / current.scale));
            }
            if !quiet {
                info!("[HUD] Interface now: {}", current.describe());
            }
        }
        current
    }

    /// The floating orb (orb mode) and mini mode's strip: a window of their own in front of
    /// everything, its black see-through, while the board stays the desktop's background (Sir,
    /// 2026-11-20: "he should always be the desktop but the floating orb … should still exist
    /// and be functinal"). It stands at its corner of the chosen monitor, `orb_size_px` square;
    /// dragged, where Sir leaves it.
    fn floating_orb(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings) {
        if !iface.orb_mode {
            if self.orb_geometry.take().is_some() {
                info!("[HUD] The floating orb is gone");
            }
            return;
        }
        let viewport = ViewportId::from_hash_of("jai_floating_orb");
        let (now, ppp) = (Instant::now(), ctx.pixels_per_point());
        let want = (iface.orb_size_px, iface.orb_anchor.clone(), iface.orb_monitor);
        if self.orb_geometry.as_ref().is_none_or(|(had, _)| *had != want) {
            let native = ctx.native_pixels_per_point().unwrap_or(1.0);
            let px = |points: f32| (points * native).round() as i32;
            // (Where it stands now, in pixels: a new size keeps its centre.)
            let centre = ctx.input(|i| i.raw.viewports.get(&viewport).and_then(|v| v.outer_rect)).map(|r| [(r.center().x * ppp) as i32, (r.center().y * ppp) as i32]);
            let monitors = crate::monitors::recent();
            // Its monitor (a scripted session's: that session's own, never Sir's); 0: the one it is on.
            let on = match test_session_monitor().filter(|n| *n > 0).unwrap_or(iface.orb_monitor as usize) {
                0 => centre.and_then(|[x, y]| monitors.iter().find(|m| m.contains(x, y))),
                n => monitors.iter().find(|m| m.index == n),
            }
            .or_else(|| crate::monitors::primary(&monitors));
            let rect = orb_place(&iface.orb_anchor, on.map_or([0, 0, 1920, 1040], |m| m.work), px(iface.orb_size_px), px(24.0), px(MINI_STRIP_HEIGHT), centre);
            if self.orb_geometry.is_none() {
                info!("[HUD] The floating orb comes out ({:.0} px, {}, at {},{})", iface.orb_size_px, iface.orb_anchor, rect[0], rect[1]);
                self.orb_tended = (now, now.checked_sub(Duration::from_secs(1)).unwrap_or(now));
            }
            self.orb_geometry = Some((want, rect));
        }
        let Some((_, [x, y, w, h])) = self.orb_geometry else { return };
        let builder = egui::ViewportBuilder::default()
            .with_title(crate::wallpaper::ORB_TITLE)
            .with_app_id("jai")
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_resizable(false)
            .with_window_level(if iface.orb_on_top { egui::viewport::WindowLevel::AlwaysOnTop } else { egui::viewport::WindowLevel::Normal })
            .with_position(pos2(x as f32, y as f32) / ppp)
            .with_inner_size(vec2(w as f32, h as f32) / ppp);
        let (born, last) = self.orb_tended;
        if now.duration_since(born) < Duration::from_millis(1500) || now.duration_since(last) > Duration::from_millis(300) {
            self.orb_tended.1 = now;
            tend_orb_window(ctx, viewport, crate::hud_feel::game_in_front());
        }
        ctx.show_viewport_immediate(viewport, for_test_session(builder), |ctx, _class| {
            self.pure_orb(ctx, m, iface);
            crate::live_shots::tick(ctx);
        });
    }

    /// Resize handles on all eight edges and corners of the frameless window (winit gives an
    /// undecorated window no resize border of its own). Each claims the cursor only while it is
    /// hovered or dragged.
    fn resize_grips(ctx: &egui::Context) {
        use egui::viewport::ResizeDirection as Dir;
        use egui::CursorIcon as Cursor;
        let r = ctx.screen_rect();
        // Corners stay clear of the title bar's buttons (12 pt from the edge).
        let (t, c) = (5.0, 10.0);
        let grips = [
            (Rect::from_min_max(pos2(r.left() + c, r.top()), pos2(r.right() - c, r.top() + t)), Dir::North, Cursor::ResizeNorth),
            (Rect::from_min_max(pos2(r.left() + c, r.bottom() - t), pos2(r.right() - c, r.bottom())), Dir::South, Cursor::ResizeSouth),
            (Rect::from_min_max(pos2(r.left(), r.top() + c), pos2(r.left() + t, r.bottom() - c)), Dir::West, Cursor::ResizeWest),
            (Rect::from_min_max(pos2(r.right() - t, r.top() + c), pos2(r.right(), r.bottom() - c)), Dir::East, Cursor::ResizeEast),
            (Rect::from_min_size(r.left_top(), vec2(c, c)), Dir::NorthWest, Cursor::ResizeNorthWest),
            (Rect::from_min_size(pos2(r.right() - c, r.top()), vec2(c, c)), Dir::NorthEast, Cursor::ResizeNorthEast),
            (Rect::from_min_size(pos2(r.left(), r.bottom() - c), vec2(c, c)), Dir::SouthWest, Cursor::ResizeSouthWest),
            (Rect::from_min_size(pos2(r.right() - c, r.bottom() - c), vec2(c, c)), Dir::SouthEast, Cursor::ResizeSouthEast),
        ];
        for (i, (rect, dir, cursor)) in grips.into_iter().enumerate() {
            egui::Area::new(egui::Id::new(("hud_resize_grip", i)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .movable(false)
                .constrain(false)
                .show(ctx, |ui| {
                    let (_, response) = ui.allocate_exact_size(rect.size(), Sense::drag());
                    // Only over the grip itself, or while resizing with it: never while another
                    // widget is dragged across the edge (egui reports a cursor every frame, so a
                    // stray claim here would make the arrow flicker between shapes).
                    let other_drag = ctx.dragged_id().is_some_and(|id| id != response.id);
                    if response.dragged() || (response.hovered() && !other_drag) {
                        ctx.set_cursor_icon(cursor);
                    }
                    if response.drag_started() {
                        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
                    }
                });
        }
    }

    /// The close button hides the window to the tray; JAI keeps running.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || SHARED.quitting.load(Ordering::Acquire) {
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        remember_placement(self.app.config());
        hide();
        info!("[HUD] Hidden to the tray");
    }

    fn flush_memory_search(&mut self) {
        if self.memory_search_at.is_some_and(|t| t.elapsed() > Duration::from_millis(200)) {
            self.memory_search_at = None;
            self.command(HudCommand::SearchMemories { query: self.memory_search.clone() });
        }
    }

    // -------------------------------------------------------------------- header ----

    // ------------------------------------------------------------------- columns ----

    /// `[ Mirror Input | Always Speak | Silent Text-Only ]`, kept in sync with voice and chat.
    fn response_mode_selector(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        let Some(cfg) = &m.cfg else { return };
        let current = cfg.response_mode.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("REPLIES").font(bold(10.5)).color(p.muted).extra_letter_spacing(1.0));
            for (mode, label, hover) in [
                (VoiceResponseMode::MirrorInput, "Mirror Input", "Spoken answers when you speak, written answers when you type"),
                (VoiceResponseMode::AlwaysSpeak, "Always Speak", "Every answer out loud, even typed questions"),
                (VoiceResponseMode::TextOnly, "Silent Text-Only", "Never speak; answers only appear here"),
            ] {
                let selected = current == mode;
                if holo_ui::key_on(ui, label, selected).on_hover_text(hover).clicked() && !selected {
                    self.command(HudCommand::SetResponseMode { mode });
                }
            }
            if let VoiceResponseMode::CustomCondition(c) = &current {
                pill(ui, format!("CUSTOM: {}", c.replace('_', " ")), p.warn)
                    .on_hover_text("A custom rule JAI applies per answer (set by voice or chat)");
            }
            if cfg.voice_muted {
                pill(ui, "voice muted", p.alert).on_hover_text("Say \"unmute your voice\" to hear JAI again");
            }
        });
    }

    // --------------------------------------------------------------------- cards ----

    fn audio_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        let phase = m.level.as_ref().map(|l| l.phase).unwrap_or("—");
        let phase_colour = match phase {
            "speaking" | "pre_speech" | "hangover" => p.nominal,
            "muted" => p.alert,
            "calibrating" | "mic_test" => p.warn,
            _ => p.accent,
        };
        card(ui, "MICROPHONE & VAD", |ui| chip(ui, phase, phase_colour), |ui| {
            if let Some(level) = &m.level {
                let value = if level.phase == "muted" {
                    "mic muted".to_string()
                } else if level.rms_db <= -119.0 {
                    "silence (no signal)".to_string()
                } else {
                    format!("{:.1} dBFS", level.rms_db)
                };
                row(ui, "Input level", value);
                let voice = level.rms >= level.start_gate;
                meter(
                    ui,
                    db_fraction(level.rms_db),
                    12.0,
                    if voice { p.nominal } else { p.accent },
                    &[(db_fraction(to_db(level.noise_floor)), p.deep), (db_fraction(to_db(level.start_gate)), p.warn)],
                );
                row(ui, "Speech probability (Silero)", format!("{:.2}", level.probability));
                let hot = level.probability >= level.start_threshold;
                meter(
                    ui,
                    level.probability,
                    7.0,
                    if hot { p.warn } else { p.accent },
                    &[(level.start_threshold, p.warn), (level.release_threshold, blend(p.deep, p.text, 0.3))],
                );
                ui.horizontal_wrapped(|ui| {
                    for (colour, text) in [
                        (p.deep, format!("noise floor {:.0} dB", to_db(level.noise_floor))),
                        (p.warn, "start gate".to_string()),
                        (blend(p.deep, p.text, 0.3), "release".to_string()),
                    ] {
                        ui.label(RichText::new("[#]").size(ts(10.0)).color(colour));
                        ui.label(RichText::new(text).size(ts(10.0)).color(p.muted));
                    }
                });
            } else {
                muted(ui, "Waiting for the microphone…");
            }
            muted(ui, if m.vad_note.is_empty() { " " } else { m.vad_note.as_str() });
            if let Some(cfg) = &m.cfg {
                let mut start = cfg.vad_start_threshold;
                let r = slider(ui, "VAD sensitivity (start threshold)", format!("{start:.2}"), crate::holo_ui::Slider::new(&mut start, 0.2..=0.9).step_by(0.01))
                    .on_hover_text("Silero probability needed to start an utterance (lower = more sensitive)");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: Some(start), release_threshold: None, silence_ms: None, noise_multiplier: None });
                }
                let mut release = cfg.vad_release_threshold;
                let r = slider(ui, "Release threshold", format!("{release:.2}"), crate::holo_ui::Slider::new(&mut release, 0.1..=0.8).step_by(0.01))
                    .on_hover_text("Below this probability a frame counts as silence");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: None, release_threshold: Some(release), silence_ms: None, noise_multiplier: None });
                }
                let mut silence = cfg.silence_ms as f32;
                let r = slider(ui, "Silence before end-of-speech", format!("{silence:.0} ms"), crate::holo_ui::Slider::new(&mut silence, 150.0..=1500.0).step_by(10.0))
                    .on_hover_text("How long you must pause before JAI treats your sentence as finished");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: None, release_threshold: None, silence_ms: Some(silence as u32), noise_multiplier: None });
                }
                let mut multiplier = cfg.noise_multiplier;
                let r = slider(ui, "Noise gate multiplier", format!("×{multiplier:.1}"), crate::holo_ui::Slider::new(&mut multiplier, 1.2..=5.0).step_by(0.1))
                    .on_hover_text("Speech must be this many times louder than the room noise to start");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: None, release_threshold: None, silence_ms: None, noise_multiplier: Some(multiplier) });
                }
            }
            ui.add_space(2.0);
            ui.columns(2, |cols| {
                let recalibrate = holo_ui::key(&mut cols[0], "Recalibrate");
                if recalibrate.on_hover_text("Re-measure the room noise (stay quiet for ~1 s)").clicked() {
                    self.command(HudCommand::Recalibrate);
                }
                let test = holo_ui::key(&mut cols[1], "Test mic");
                if test.on_hover_text("Record 2.5 s and run Whisper on it").clicked() {
                    self.command(HudCommand::TestMic);
                }
            });
        });
    }

    fn devices_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let mut refresh = false;
        card(
            ui,
            "AUDIO DEVICES",
            |ui| {
                if crate::holo_ui::key(ui, "RELOAD").on_hover_text("Rescan devices").clicked() {
                    refresh = true;
                }
            },
            |ui| {
                let Some(cfg) = &m.cfg else {
                    muted(ui, "Loading…");
                    return;
                };
                muted(ui, "Microphone");
                if let Some(device) = device_combo(ui, "hud_mic", "Auto (default + failover)", &m.inputs, cfg.requested_mic.as_deref()) {
                    self.command(HudCommand::SetMicDevice { device });
                }
                row(ui, "Active", &cfg.active_mic);
                muted(ui, "Speaker");
                if let Some(device) = device_combo(ui, "hud_speaker", "System default", &m.outputs, cfg.requested_speaker.as_deref()) {
                    self.command(HudCommand::SetSpeakerDevice { device });
                }
                row(ui, "Active", &cfg.active_speaker);
                let mut loudness = cfg.loudness_pct as f32;
                if slider(ui, "JAI voice loudness", format!("{loudness:.0} %"), crate::holo_ui::Slider::new(&mut loudness, 0.0..=200.0).step_by(5.0)).changed() {
                    self.command(HudCommand::SetLoudness { volume_pct: loudness as u8 });
                }
                ui.horizontal(|ui| {
                    let voices = if cfg.voices.is_empty() { std::slice::from_ref(&cfg.voice) } else { cfg.voices.as_slice() };
                    crate::holo_ui::Picker::from_id_salt("hud_voice").width(ui.available_width() - 110.0).selected_text(&cfg.voice).show_ui(ui, |ui| {
                        for voice in voices {
                            if crate::holo_ui::item(ui, voice, *voice == cfg.voice).clicked() {
                                self.command(HudCommand::SetVoice { voice: voice.clone() });
                            }
                        }
                    });
                    if crate::holo_ui::key(ui, "Test voice").clicked() {
                        self.command(HudCommand::TestVoice);
                    }
                });
                row(ui, "Speech engines", format!("{} · {}", if cfg.stt_ready { "whisper OK: " } else { "whisper loading…" }, cfg.tts_status))
                    .on_hover_text(format!("{} | {}", cfg.stt_model, cfg.tts_status));
            },
        );
        if refresh {
            self.command(HudCommand::RefreshDevices);
        }
    }

    fn system_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        let cpu_name = m.telemetry.as_ref().map(|t| t.cpu_name.trim().to_string()).unwrap_or_default();
        card(
            ui,
            "SYSTEM",
            |ui| {
                ui.add(egui::Label::new(RichText::new(cpu_name).size(ts(10.5)).color(p.muted)).truncate());
            },
            |ui| {
                let Some(t) = &m.telemetry else {
                    muted(ui, "Collecting telemetry…");
                    return;
                };
                let bar = |ui: &mut Ui, pct: f32, hot: bool| meter(ui, pct / 100.0, 6.0, if hot { p.warn } else { p.accent }, &[]);
                row(ui, "CPU", format!("{:.1}% · {} threads", t.cpu_usage_pct, t.cpu_cores));
                bar(ui, t.cpu_usage_pct, t.cpu_usage_pct > 85.0);
                row(ui, "RAM", format!("{:.1} / {:.1} GB", t.ram_used_gb, t.ram_total_gb));
                bar(ui, t.ram_usage_pct, t.ram_usage_pct > 90.0);
                match &t.gpu {
                    Some(g) => {
                        let mut parts = Vec::new();
                        if let Some(c) = g.temperature_c {
                            parts.push(format!("{c:.0}°C"));
                        }
                        if let Some(u) = g.utilization_pct {
                            parts.push(format!("{u:.0}%"));
                        }
                        if let Some(w) = g.power_w {
                            parts.push(format!("{w:.0} W"));
                        }
                        row(ui, &g.name.replace("NVIDIA GeForce ", ""), if parts.is_empty() { "—".into() } else { parts.join(" · ") });
                        bar(ui, g.utilization_pct.unwrap_or(0.0), g.temperature_c.unwrap_or(0.0) >= 80.0);
                        if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb.filter(|t| *t > 0)) {
                            row(ui, "VRAM", format!("{:.1} / {:.1} GB", used as f32 / 1024.0, total as f32 / 1024.0));
                            let pct = used as f32 / total as f32 * 100.0;
                            bar(ui, pct, pct > 92.0);
                        }
                    }
                    None => {
                        row(ui, "GPU", "no telemetry");
                    }
                }
                row(ui, "Network", format!("↓ {} · ↑ {}", rate(t.net_down_kbps), rate(t.net_up_kbps)));
                for d in &t.disks {
                    row(ui, &format!("Drive {}", d.mount), format!("{:.0} GB free / {:.0} GB", d.free_gb, d.total_gb));
                    bar(ui, d.used_pct, d.used_pct > 92.0);
                }
            },
        );
    }

    /// A PC shutdown / restart waiting for Sir's double-check: a glowing crimson card with
    /// `[ Confirm PC Shutdown ]` / `[ Cancel ]` (or just "yes" / "no" within 30 s).
    fn power_card(&mut self, ui: &mut Ui, p: &Palette, card: &PowerCardView) {
        let pending = card.status == "pending";
        let glow = if pending { ((ui.input(|i| i.time) * 3.0).sin().abs() * 0.5 + 0.5) as f32 } else { 0.35 };
        let colour = match card.status.as_str() {
            "confirmed" => p.alert,
            "pending" => p.alert,
            _ => p.muted,
        };
        Frame::none()
            .fill(p.alert.gamma_multiply(if pending { 0.10 } else { 0.04 }))
            .stroke(stroke(if pending { 1.6 } else { 1.0 }, colour.gamma_multiply(glow)))
            .rounding(Rounding::ZERO)
            .inner_margin(Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.9).max(180.0));
                ui.label(RichText::new("SYSTEM POWER · DOUBLE-CHECK").size(ts(9.5)).color(p.alert).extra_letter_spacing(0.8));
                let what = if card.action == "restart" { "Restart this PC now?" } else { "Shut down this PC now?" };
                ui.label(RichText::new(what).font(bold(13.5)).color(p.alert));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| match card.status.as_str() {
                    "pending" => {
                        if button(ui, &format!("{}", card.label), Tone::Danger).clicked() {
                            self.command(HudCommand::PowerCard { id: card.id.clone(), confirm: true });
                        }
                        if crate::holo_ui::key(ui, "Cancel").clicked() {
                            self.command(HudCommand::PowerCard { id: card.id.clone(), confirm: false });
                        }
                        muted(ui, "or say “yes” / “no” within 30 s");
                    }
                    "confirmed" => {
                        ui.label(RichText::new(if card.action == "restart" { "Restarting…" } else { "Shutting down…" }).color(p.alert));
                    }
                    "expired" => {
                        ui.label(RichText::new("Timed out — aborted").color(p.muted));
                    }
                    _ => {
                        ui.label(RichText::new("Aborted").color(p.muted));
                    }
                });
            });
        if pending {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        ui.add_space(4.0);
    }

    /// A WhatsApp message waiting for Sir's go-ahead: `[ Send via WhatsApp ]` / `[ Cancel ]`.
    fn message_card(&mut self, ui: &mut Ui, p: &Palette, card: &MessageCardView) {
        let colour = match card.status.as_str() {
            "sent" => p.nominal,
            "cancelled" => p.muted,
            "pending" => p.nominal.gamma_multiply(0.8),
            _ => p.alert,
        };
        Frame::none()
            .fill(p.well)
            .stroke(stroke(1.0, colour.gamma_multiply(0.6)))
            .rounding(Rounding::ZERO)
            .inner_margin(Margin::symmetric(12.0, 8.0))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.9).max(160.0));
                ui.label(RichText::new("WHATSAPP · MESSAGE CARD").size(ts(9.5)).color(p.muted).extra_letter_spacing(0.6));
                ui.label(RichText::new(format!("To {}{}", card.recipient, if card.phone.is_empty() { String::new() } else { format!("  ({})", card.phone) })).font(bold(12.5)).color(p.accent));
                ui.add(egui::Label::new(RichText::new(&card.text).size(ts(13.5))).wrap());
                ui.add_space(4.0);
                ui.horizontal(|ui| match card.status.as_str() {
                    "pending" => {
                        if button(ui, "Send via WhatsApp", Tone::On).clicked() {
                            self.command(HudCommand::MessageCard { id: card.id.clone(), send: true });
                        }
                        if crate::holo_ui::key(ui, "Cancel").clicked() {
                            self.command(HudCommand::MessageCard { id: card.id.clone(), send: false });
                        }
                        muted(ui, "or just say “yes”");
                    }
                    "sent" => {
                        ui.label(RichText::new("OK: Sent").color(p.nominal));
                    }
                    "cancelled" => {
                        ui.label(RichText::new("Cancelled").color(p.muted));
                    }
                    other => {
                        ui.label(RichText::new(format!("WARN: {other}")).color(p.alert));
                    }
                });
            });
        ui.add_space(4.0);
    }

    /// The knock alert: an amber frame pulsing round the whole HUD and a banner.
    fn room_alert_overlay(&self, ctx: &egui::Context, m: &HudModel) {
        let Some((message, until)) = &m.room_alert else { return };
        let now = Instant::now();
        if *until <= now {
            return;
        }
        let t = holo::theme();
        let pulse = ((ctx.input(|i| i.time) * 6.0).sin().abs() * 0.7 + 0.3) as f32;
        egui::Area::new(egui::Id::new("hud_room_alert"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(0.0, 0.0))
            .interactable(false)
            .show(ctx, |ui| {
                // The screen's edge beats in the caution colour (light, not a painted frame),
                // and what it is stands on a plate under the top rail.
                let rect = home_screen(ctx);
                let edge = rect.shrink(1.0);
                let mut light = holo::Light::new(ui.painter());
                light.halo(&[edge.left_top(), edge.right_top(), edge.right_bottom(), edge.left_bottom()], true, 26.0, holo::lit(t.warn(), 0.5 * pulse));
                light.strip(&[edge.left_top(), edge.right_top(), edge.right_bottom(), edge.left_bottom()], true, 2.0, holo::a(t.warn(), pulse));
                light.paint(ui.painter());
                let banner = Rect::from_center_size(pos2(rect.center().x, rect.top() + 70.0), vec2(rect.width().min(560.0), 44.0));
                crate::holo_frame::card(ui.painter(), banner, 0.0, 1.0);
                holo::outline(ui.painter(), &holo::plate(banner, [0.0, 0.0, crate::holo_frame::CUT, 0.0]), t.warn());
                let chip = holo_kit::chip(ui.painter(), pos2(banner.left() + 12.0, banner.center().y - 7.0), "ALERT", t.warn(), true, 1.0);
                let words = holo::galley_within(ui.painter(), Face::Body, message, 14.0, t.ink(), banner.right() - chip.right() - 24.0);
                ui.painter().galley(pos2(chip.right() + 10.0, banner.center().y - words.size().y / 2.0), words, t.ink());
            });
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn memory_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        let n = m.memories.len();
        let count = match &m.memory_query {
            Some(_) => format!("{n} match{}", if n == 1 { "" } else { "es" }),
            None => format!("{n} facts"),
        };
        card(ui, "LONG-TERM MEMORY", |ui| muted(ui, count), |ui| {
            ui.horizontal(|ui| {
                let w = ui.available_width();
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.memory_key).hint_text("key (e.g. coffee)").desired_width(w * 0.5));
                crate::holo_ui::Picker::from_id_salt("hud_memory_category")
                    .width(ui.available_width())
                    .selected_text(&self.memory_category)
                    .show_ui(ui, |ui| {
                        for c in MEMORY_CATEGORIES {
                            crate::holo_ui::option(ui, &mut self.memory_category, c.to_string(), c);
                        }
                    });
            });
            ui.horizontal(|ui| {
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.memory_value).hint_text("Fact to remember…").desired_width(ui.available_width() - 96.0));
                ui.add(crate::holo_ui::Number::new(&mut self.memory_importance).range(1..=10).prefix("FAV ")).on_hover_text("Importance 1-10");
                if button(ui, "+", Tone::On).on_hover_text("Remember").clicked() {
                    let (key, value) = (self.memory_key.trim().to_string(), self.memory_value.trim().to_string());
                    if !key.is_empty() && !value.is_empty() {
                        self.command(HudCommand::AddMemory {
                            key,
                            value,
                            category: Some(self.memory_category.clone()),
                            importance: Some(self.memory_importance),
                        });
                        self.memory_key.clear();
                        self.memory_value.clear();
                    }
                }
            });
            let search = crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.memory_search).hint_text("Search memories…").desired_width(f32::INFINITY));
            if search.changed() {
                self.memory_search_at = Some(Instant::now());
            }
            ui.add_space(2.0);
            if m.memories.is_empty() {
                muted(ui, if m.memory_query.is_some() { "No matches." } else { "Nothing remembered yet." });
            }
            for mem in m.memories.iter().take(120) {
                item(ui, None, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&mem.key).font(bold(12.0)).color(p.accent));
                        chip(ui, &mem.category, p.nominal);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if crate::holo_ui::key(ui, "X").on_hover_text("Forget this").clicked() {
                                self.command(HudCommand::DeleteMemory { id_or_key: mem.id.to_string() });
                            }
                            muted(ui, format!("FAV {}", mem.importance));
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(&mem.value).size(ts(12.0))).wrap());
                });
            }
        });
    }

    fn skills_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        card(ui, "SKILLS", |ui| muted(ui, m.skills.len().to_string()), |ui| {
            if m.skills.is_empty() {
                muted(ui, "No skills yet — say “create a skill that …”");
            }
            for skill in &m.skills {
                item(ui, None, |ui| {
                    ui.horizontal(|ui| {
                        let title = if skill.title.is_empty() { &skill.name } else { &skill.title };
                        ui.label(RichText::new(title).font(bold(12.0)).color(p.accent));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if crate::holo_ui::key(ui, "X").on_hover_text("Delete skill").clicked() {
                                self.confirm_delete_skill = Some(skill.name.clone());
                            }
                            if crate::holo_ui::key(ui, "Run").clicked() {
                                self.command(HudCommand::RunSkill { name: skill.name.clone() });
                            }
                            muted(ui, format!("{}×", skill.usage_count));
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(&skill.description).size(ts(12.0))).wrap());
                    if !skill.trigger_phrases.is_empty() {
                        muted(ui, format!("“{}”", skill.trigger_phrases.join("” · “")));
                    }
                });
            }
        });
    }

    fn tools_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        card(ui, "TOOL LOG", |ui| muted(ui, m.tool_count.to_string()), |ui| {
            if m.tools.is_empty() {
                muted(ui, "No tools used yet.");
            }
            for t in &m.tools {
                item(ui, Some(if t.success { p.nominal } else { p.alert }), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&t.tool).font(bold(11.5)).color(p.accent));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| muted(ui, &t.meta));
                    });
                    ui.add(egui::Label::new(RichText::new(&t.summary).size(ts(12.0))).wrap());
                    holo_ui::fold(ui, ("hud_tool", t.id), "details", false, |ui| {
                            ui.add(egui::Label::new(RichText::new(&t.details).monospace().size(ts(10.5)).color(p.muted)).wrap());
                        });
                });
            }
        });
    }

    fn brain_card(&mut self, ui: &mut Ui, m: &HudModel) {
        card(ui, "BRAIN", |_| {}, |ui| {
            let Some(cfg) = &m.cfg else {
                muted(ui, "Loading…");
                return;
            };
            muted(ui, "Primary model");
            let mut models = m.models.clone();
            if !models.contains(&cfg.model) {
                models.push(cfg.model.clone());
            }
            crate::holo_ui::Picker::from_id_salt("hud_model").width(ui.available_width()).selected_text(&cfg.model).show_ui(ui, |ui| {
                for model in &models {
                    if crate::holo_ui::item(ui, model, *model == cfg.model).clicked() && *model != cfg.model {
                        self.command(HudCommand::SetModel { model: model.clone() });
                    }
                }
            });
            row(ui, "Fallback", &cfg.fallback_model);
            row(ui, "Engine", &cfg.llm).on_hover_text(&cfg.llm);
            if crate::holo_ui::key(ui, "Clear conversation").on_hover_text("Forget the current conversation (long-term memory is kept)").clicked() {
                self.command(HudCommand::ClearHistory);
            }
        });
    }

    // ------------------------------------------------------------------- overlays ----

    /// JAI's contextual cards beside the orb, and what Sir clicks on them.
    fn popups(&self, ctx: &egui::Context, dt: f32) {
        use crate::dynamic_popups::PopupAction;
        for action in crate::dynamic_popups::show(ctx, self.palette.card_style(), dt) {
            let media = |a: crate::media::MediaAction| {
                self.rt.spawn(async move {
                    let _ = crate::media::execute(a).await;
                });
            };
            match action {
                PopupAction::MediaPrevious => media(crate::media::MediaAction::Previous),
                PopupAction::MediaPlayPause => media(crate::media::MediaAction::PlayPause),
                PopupAction::MediaNext => media(crate::media::MediaAction::Next),
                // (A drag of the slider is a hundred changes a second: the mixer carries out the
                // latest, one call at a time.)
                PopupAction::SetVolume { target, percent } => {
                    self.rt.spawn(async move { crate::audio_mixer::slide(&target, percent).await });
                }
                PopupAction::Confirm(_) => self.command(HudCommand::Chat { text: "yes".into() }),
                PopupAction::Cancel(_) => self.command(HudCommand::Chat { text: "no".into() }),
                // A butler card's button is its fix, carried out as the tool call it stands for;
                // any other card's button goes to JAI as a request.
                PopupAction::Button(label) => match crate::proactive_butler::click(&label) {
                    Some(fix) => self.run_calls(vec![(fix.tool, fix.args)]),
                    None => self.command(HudCommand::Chat { text: label }),
                },
            }
        }
    }

    /// The notifications: one stack at the top right of the home screen, under the header
    /// strip and clear of the text console (`hud_notify`).
    fn toasts(&self, ctx: &egui::Context, m: &HudModel) {
        if m.toasts.is_empty() {
            return;
        }
        let home = home_screen(ctx);
        let right = crate::dynamic_popups::card_column_right(home, 360.0, console_rect());
        let area = Rect::from_min_max(pos2(home.left(), home.top() + HEADER_H + 34.0), pos2(right, home.bottom()));
        let now = Instant::now();
        let cards: Vec<crate::hud_notify::Card<'_>> = m.toasts.iter().map(|t| crate::hud_notify::Card { text: &t.text, alert: t.error, age: now.saturating_duration_since(t.since).as_secs_f32(), left: t.until.saturating_duration_since(now).as_secs_f32() }).collect();
        if crate::hud_notify::paint_stack(ctx, area, &cards) {
            ctx.request_repaint();
        }
    }

    /// `[ UNDO ]` by the notices for a few seconds after a change of his own: a click takes
    /// it back, as "undo that" does. (Not beside a game: nothing pops up over it.)
    fn undo_chip(&self, ctx: &egui::Context) {
        let Some(age) = crate::undo::kept_for().filter(|age| *age < UNDO_CHIP_FOR) else { return };
        if crate::vram_guard::in_game() {
            return;
        }
        let home = home_screen(ctx);
        let right = crate::dynamic_popups::card_column_right(home, 360.0, console_rect());
        // In its last 0.4 s it fades away.
        let alpha = ((UNDO_CHIP_FOR - age).as_secs_f32() / 0.4).min(1.0);
        let label = "UNDO";
        let width = crate::holo_kit::key_width(&ctx.layer_painter(egui::LayerId::background()), label);
        let at = pos2(right - width, home.top() + HEADER_H + 10.0);
        egui::Area::new(egui::Id::new("hud_undo_chip")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(width, crate::holo_kit::KEY_H), Sense::click());
            holo_kit::key(ui.painter(), rect, label, response.hovered() as u8 as f32, response.is_pointer_button_down_on(), true, alpha);
            if response.on_hover_text("Take the last change back (\"undo that\")").clicked() {
                self.run_calls(vec![("undo_last_action", serde_json::json!({}))]);
            }
        });
        ctx.request_repaint_after(Duration::from_millis(if alpha < 1.0 { 16 } else { 250 }));
    }

    /// Where part `part` of the tour (`TOUR`) stands on the home screen; None while it shows
    /// nothing (no block open, an empty tray, the console closed).
    fn tour_rect(&self, ctx: &egui::Context, part: &str) -> Option<Rect> {
        let home = home_screen(ctx);
        match part {
            "emblem" => Some(self.emblem_spot).filter(|(_, r, _)| *r > 0.0).map(|(c, r, _)| Rect::from_center_size(c, Vec2::splat(r * 2.2))),
            "header" => Some(Rect::from_min_max(pos2(home.left() + 6.0, home.top() + 5.0), pos2(home.right() - 6.0, home.top() + 16.0 + HEADER_H))),
            "comms" => ctx.data(|d| d.get_temp::<(u64, Rect)>(egui::Id::new("jai_tour_comms"))).filter(|(frame, _)| frame + 2 >= ctx.cumulative_pass_nr()).map(|(_, r)| r.expand(4.0)),
            "board" => crate::hud_dock::block_rects().into_iter().reduce(|a, b| a.union(b)).map(|r| r.expand(6.0)),
            "tray" => ctx.memory(|m| m.area_rect(egui::Id::new("jai_block_tray"))).filter(|_| !crate::hud_dock::minimized_blocks().is_empty()).map(|r| r.expand(4.0)),
            "telemetry" => Some(Rect::from_min_max(pos2(home.left() + 6.0, home.bottom() - 16.0 - TELEMETRY_H), pos2(home.right() - 6.0, home.bottom() - 4.0))),
            "console" => console_rect(),
            _ => None,
        }
    }

    /// The tour's light: a soft outline round the part his sentence is about, gliding from part
    /// to part, gone a moment after the last.
    fn tour_outline(&self, ctx: &egui::Context) {
        let Some((began, cues)) = lock(&TOUR_CUES).clone() else { return };
        let t = began.elapsed().as_secs_f32();
        if cues.last().is_none_or(|(_, at)| t > at + TOUR_LAST_SECS) {
            *lock(&TOUR_CUES) = None;
            return;
        }
        ctx.request_repaint();
        let now = cues.iter().rev().find(|(_, at)| *at <= t).and_then(|(k, _)| self.tour_rect(ctx, TOUR[*k].0));
        let id = egui::Id::new("jai_tour_light");
        let alpha = ctx.animate_bool_with_time(id, now.is_some(), crate::holo_motion::LEAVE);
        // (Where nothing is lit it glides on from its last place.)
        let last = ctx.data(|d| d.get_temp::<Rect>(id)).or(now);
        let Some(target) = now.or(last) else { return };
        ctx.data_mut(|d| d.insert_temp(id, target));
        let edge = |k: usize, v: f32| ctx.animate_value_with_time(id.with(k), v, 0.35);
        let r = Rect::from_min_max(pos2(edge(0, target.left()), edge(1, target.top())), pos2(edge(2, target.right()), edge(3, target.bottom())));
        tour_light(&ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id)), r, crate::hud_board::active_colour().gamma_multiply(alpha));
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.confirm_shutdown {
            let mut open = true;
            board_window(ctx, "confirm_shutdown", "Shut down // are you sure", [380.0, 190.0], &mut open, |ui| {
                    ui.label("Shut down J.A.I. completely?");
                    muted(ui, "Closing the window only hides him in the tray.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if button(ui, "Shut down", Tone::Danger).clicked() {
                            self.confirm_shutdown = false;
                            self.command(HudCommand::Shutdown);
                        }
                        if crate::holo_ui::key(ui, "Cancel").clicked() {
                            self.confirm_shutdown = false;
                        }
                    });
                });
            if !open {
                self.confirm_shutdown = false;
            }
            if !self.confirm_shutdown {
                crate::hud_dock::forget("confirm_shutdown");
            }
        }
        if let Some(name) = self.confirm_delete_skill.clone() {
            let mut open = true;
            board_window(ctx, "confirm_delete_skill", "Delete skill // are you sure", [380.0, 170.0], &mut open, |ui| {
                    ui.label(format!("Delete the skill \"{name}\"?"));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if button(ui, "Delete", Tone::Danger).clicked() {
                            self.confirm_delete_skill = None;
                            self.command(HudCommand::DeleteSkill { name: name.clone() });
                        }
                        if crate::holo_ui::key(ui, "Cancel").clicked() {
                            self.confirm_delete_skill = None;
                        }
                    });
                });
            if !open {
                self.confirm_delete_skill = None;
            }
            if self.confirm_delete_skill.is_none() {
                crate::hud_dock::forget("confirm_delete_skill");
            }
        }
    }

    fn offline_overlay(&self, ctx: &egui::Context) {
        let t = holo::theme();
        egui::Area::new(egui::Id::new("hud_offline"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let rect = ctx.screen_rect();
                ui.painter().rect_filled(rect, Rounding::ZERO, holo::a(t.void(), 0.94));
                let said = holo::text(ui.painter(), rect.center(), Align2::CENTER_CENTER, Face::Label, "J.A.I. is shutting down", 12.0, t.accent());
                holo::rule(ui.painter(), pos2(said.left(), said.bottom() + 8.0), pos2(said.right(), said.bottom() + 8.0), holo::a(t.ink(), 0.3));
                ui.allocate_rect(rect, Sense::click());
            });
    }
}

/// A title-bar button: a glyph on a rounded, accent-lit square. On the close button, `danger` turns it red
/// when hovered; on the toggles (settings, standby) it lights them while their panel is open.
fn title_button(ui: &mut Ui, p: &Palette, glyph: &str, danger: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 26.0), Sense::click());
    let hot = response.hovered();
    let close = glyph == "X";
    let colour = match (hot, danger, close) {
        (true, true, true) => p.alert,
        (_, true, false) => p.accent,
        (true, _, _) => p.accent,
        _ => p.muted,
    };
    let fill = match (hot, danger, close) {
        (true, true, true) => p.alert.gamma_multiply(0.18),
        (_, true, false) => p.accent.gamma_multiply(0.14),
        (true, _, _) => p.accent.gamma_multiply(0.12),
        _ => Color32::TRANSPARENT,
    };
    ui.painter().rect(rect, Rounding::ZERO, fill, stroke(1.0, if hot { colour.gamma_multiply(0.6) } else { p.line.gamma_multiply(0.5) }));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(ts(13.0)), colour);
    response
}

/// Where the floating orb's window stands (x, y, w, h in pixels) on a monitor's `work` area
/// (left, top, right, bottom): at its corner (`anchor`), `side` pixels square, `margin` from
/// the edges; mini mode's strip across the top, `strip` pixels tall; "free" round `centre`,
/// where Sir left it (without one: the bottom-right corner).
fn orb_place(anchor: &str, work: [i32; 4], side: i32, margin: i32, strip: i32, centre: Option<[i32; 2]>) -> [i32; 4] {
    let [l, t, r, b] = work;
    if anchor == MINI_STRIP {
        return [l, t, r - l, strip];
    }
    let (x, y) = match (anchor, centre) {
        ("top-left", _) => (l + margin, t + margin),
        ("top-right", _) => (r - side - margin, t + margin),
        ("bottom-left", _) => (l + margin, b - side - margin),
        ("center", _) => ((l + r - side) / 2, (t + b - side) / 2),
        ("free", Some([x, y])) => (x - side / 2, y - side / 2),
        _ => (r - side - margin, b - side - margin),
    };
    [x, y, side, side]
}

/// A saved instant replay: `[ Open Video Clip ]` `[ Open Folder ]`.
fn clip_card(ui: &mut Ui, p: &Palette, clip: &ClipCardView) {
    let name = std::path::Path::new(&clip.video).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| clip.video.clone());
    Frame::none()
        .fill(p.well)
        .stroke(stroke(1.0, crate::holo::theme().accent().gamma_multiply(0.55)))
        .rounding(Rounding::ZERO)
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .show(ui, |ui| {
            ui.set_max_width((ui.available_width() * 0.9).max(180.0));
            ui.label(RichText::new(format!("INSTANT REPLAY · LAST {} S", clip.seconds)).size(ts(9.5)).color(p.muted).extra_letter_spacing(0.6));
            ui.add(egui::Label::new(RichText::new(&name).font(bold(12.5)).color(crate::holo::theme().accent())).truncate()).on_hover_text(&clip.video);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if button(ui, "Open Video Clip", Tone::On).clicked() {
                    open_path(&clip.video, false);
                }
                if holo_ui::key(ui, "Open folder").clicked() {
                    open_path(if clip.folder.is_empty() { &clip.video } else { &clip.folder }, clip.folder.is_empty());
                }
            });
        });
    ui.add_space(4.0);
}

/// Opens a file with its default app, or shows it selected in Explorer (`reveal`).
fn open_path(path: &str, reveal: bool) {
    let path = path.to_string();
    #[cfg(windows)]
    crate::win32::run_detached(move || {
        let result = if reveal {
            crate::win32::shell_execute("explorer.exe", Some(&format!("/select,\"{path}\"")), None)
        } else {
            crate::win32::shell_execute(&path, None, None)
        };
        if let Err(e) = result {
            warn!("[HUD] Could not open {path}: {e}");
        }
    });
    #[cfg(not(windows))]
    {
        let target = if reveal { std::path::Path::new(&path).parent().map(|d| d.display().to_string()).unwrap_or(path.clone()) } else { path.clone() };
        if let Err(e) = std::process::Command::new("xdg-open").arg(&target).spawn() {
            warn!("[HUD] Could not open {target}: {e}");
        }
    }
}

/// The ground of a window of his own: the canvas's black (with Sir's opacity) and its dot grid.
fn paint_background(ctx: &egui::Context, p: &Palette) {
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    painter.rect_filled(rect, Rounding::ZERO, p.bg);
    holo::dot_field(&painter, rect, DOT_SPACING, rect.center(), holo::a(holo::theme().ink(), DOT_ALPHA));
}

impl HudApp {
    /// The Settings drawer: every technical panel, one tab at a time, out of the conversation's
    /// way.
    fn settings_drawer(&mut self, ui: &mut Ui, m: &HudModel, iface: &InterfaceSettings) {
        let p = self.palette;
        ui.horizontal(|ui| {
            ui.label(RichText::new("SETTINGS").font(bold(13.0)).color(p.accent).extra_letter_spacing(1.6));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if title_button(ui, &p, "X", false).on_hover_text("Close the settings").clicked() {
                    self.drawer_open = false;
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (DrawerTab::Voice, "Voice"),
                (DrawerTab::System, "System"),
                (DrawerTab::Memory, "Memory"),
                (DrawerTab::Activity, "Activity"),
                (DrawerTab::Look, "Look"),
            ] {
                let selected = self.drawer_tab == tab;
                if holo_ui::key_on(ui, label, selected).clicked() {
                    self.drawer_tab = tab;
                }
            }
        });
        ui.add_space(8.0);
        crate::holo_block::faded(ui, egui::ScrollArea::vertical().id_salt("hud_drawer").auto_shrink([false; 2]), |ui| match self.drawer_tab {
            DrawerTab::Voice => {
                card(ui, "REPLIES", |_| {}, |ui| self.response_mode_selector(ui, m));
                self.audio_card(ui, m);
                self.devices_card(ui, m);
            }
            DrawerTab::System => {
                self.status_card(ui, m);
                self.system_card(ui, m);
                self.brain_card(ui, m);
            }
            DrawerTab::Memory => {
                self.memory_card(ui, m);
                self.skills_card(ui, m);
            }
            DrawerTab::Activity => self.tools_card(ui, m),
            DrawerTab::Look => self.look_card(ui, iface),
        });
    }

    /// Settings → System: the last turn's latency, the model, the reply mode and the languages.
    fn status_card(&mut self, ui: &mut Ui, m: &HudModel) {
        card(ui, "LATENCY & LANGUAGE", |_| {}, |ui| {
            let ms = |v: Option<u64>| v.map(|v| format!("{v} ms")).unwrap_or_else(|| "—".into());
            let [stt, token, audio] = m.latency;
            row(ui, "Speech-to-text", ms(stt)).on_hover_text("Last turn, from the end of your speech");
            row(ui, "First token", ms(token));
            row(ui, "First audio", ms(audio));
            if let Some(cfg) = &m.cfg {
                row(ui, "Language model", &cfg.model);
                let mode = if cfg.voice_muted { "muted".to_string() } else { cfg.response_mode.display() };
                row(ui, "Replies", mode);
                row(ui, "Follow-up window", format!("{} s after each answer", cfg.window_secs));
            }
            row(ui, "Languages", "German, English, Denglisch → British English");
        });
    }

}

// ------------------------------------------------------------------- the HUD board canvas ----
// No chat log and no input bar: an obsidian canvas with the ring emblem, the board of blocks,
// a micro header strip (top right) and a micro audio/telemetry strip (bottom left). Ctrl+L opens
// a small floating command prompt; everything else is voice.

/// The strips' ink and their quieter ink: the theme's structure colour.
fn strip_ink() -> Color32 {
    crate::holo::theme().ink()
}
fn strip_muted() -> Color32 {
    crate::holo::theme().muted()
}
/// Header strip and telemetry strip heights.
const HEADER_H: f32 = 16.0;
const TELEMETRY_H: f32 = 14.0;
/// The drafting grid: 1 px dots every 24 px.
const DOT_SPACING: f32 = 24.0;
const DOT_ALPHA: f32 = 0.085;
/// The Stark workshop's registration marks: every fourth dot is a small "+" (arms 3 px), a
/// little brighter, fixed to the canvas while it drifts.
const CROSS_EVERY: i64 = 4;
const CROSS_ARM: f32 = 3.0;
const CROSS_ALPHA: f32 = 0.20;
/// The COMMS block (what Sir said, what JAI answers) fades this long after the last word.
const CAPTION_SECS: f32 = 20.0;
/// The band above the telemetry strip the board leaves to the COMMS block (its frame, a heard
/// line and two rows of his answer).
const CAPTION_BAND: f32 = 124.0;
/// The slide-out text console.
const CONSOLE_WIDTH: f32 = 440.0;
/// The left column of system blocks (confirmations, message cards, clips, widgets).
const SYSTEM_COLUMN: f32 = 320.0;

fn strip_font() -> FontId {
    FontId::monospace(11.0)
}

/// The size of the shell's wide capitals (the top rail, the state plate, the key hints).
const RAIL_TEXT: f32 = 8.0;

/// How wide `text` stands in the rail's capitals.
fn rail_width(painter: &egui::Painter, text: &str) -> f32 {
    holo::galley(painter, Face::Label, text, RAIL_TEXT, Color32::WHITE).size().x
}

/// One word of the top rail. It stands in plain ink over the rule; the pointer lights it
/// (`hot` 0-1: a filament of `colour` grows under it), and while its thing is open it is a
/// solid leaning block of `colour` with black lettering.
fn rail_key(painter: &egui::Painter, rect: Rect, text: &str, hot: f32, on: bool, colour: Color32) {
    let t = holo::theme();
    if on {
        let shape = holo_kit::leaning(rect);
        let mut light = holo::Light::new(painter);
        light.halo(&[shape[0], shape[1], shape[2], shape[3], shape[0]], false, 7.0, holo::lit(colour, 0.24));
        light.paint(painter);
        holo::fill(painter, &shape, holo::mix(colour, t.hot(), 0.3 * hot));
        holo::text(painter, rect.center(), Align2::CENTER_CENTER, Face::Label, text, RAIL_TEXT, Color32::BLACK);
        return;
    }
    holo::text(painter, rect.center(), Align2::CENTER_CENTER, Face::Label, text, RAIL_TEXT, holo::mix(t.muted(), t.ink(), 0.45 + 0.55 * hot));
    if hot > 0.02 {
        let (mid, half, y) = (rect.center().x, (rect.width() / 2.0 - 3.0) * hot, rect.bottom() + 1.5);
        let mut light = holo::Light::new(painter);
        light.neon(&[pos2(mid - half, y), pos2(mid + half, y)], false, 1.3, holo::a(colour, hot), 0.8);
        light.paint(painter);
    }
}

/// A picker on the top rail: what is chosen (a camera's name) in a slot, a marker at its end.
fn rail_picker(painter: &egui::Painter, rect: Rect, text: &str, hot: f32) {
    let t = holo::theme();
    let shape = holo::plate(rect, [0.0, 0.0, 5.0, 0.0]);
    holo::fill(painter, &shape, holo::a(t.ink(), 0.05 * hot));
    holo::outline(painter, &shape, holo::mix(holo::a(t.ink(), 0.34), t.accent(), hot));
    let name = holo::galley_within(painter, Face::Label, text, RAIL_TEXT, t.ink(), rect.width() - 30.0);
    painter.galley(pos2(rect.left() + 8.0, rect.center().y - name.size().y / 2.0), name, t.ink());
    let c = pos2(rect.right() - 12.0, rect.center().y);
    painter.add(Shape::convex_polygon(vec![c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0), c + vec2(0.0, 2.5)], holo::mix(t.muted(), t.accent(), hot), Stroke::NONE));
}

/// The state plate's cross-fade.
const PILL_FADE: f32 = 0.12;

/// Where the screen card stands: under the emblem at home, above the bottom strip while the
/// emblem is docked; always whole on the screen.
fn screen_card_rect(screen: Rect, frame: &crate::holo_core::CoreFrame) -> Rect {
    let size = vec2(340.0, 72.0);
    let top = if frame.dock < 0.5 { frame.centre.y + frame.radius * 1.32 } else { screen.bottom() - 10.0 - TELEMETRY_H - 14.0 - size.y };
    let x = if frame.dock < 0.5 { frame.centre.x } else { screen.center().x };
    let rect = Rect::from_min_size(pos2(x - size.x / 2.0, top), size);
    let inside = screen.shrink(12.0);
    rect.translate(vec2((inside.left() - rect.left()).max(0.0) + (inside.right() - rect.right()).min(0.0), (inside.top() - rect.top()).max(0.0) + (inside.bottom() - TELEMETRY_H - 14.0 - rect.bottom()).min(0.0)))
}

/// The frost over the whole HUD while JAI looks at the screen (`screen_context::frost`).
fn paint_frost(ctx: &egui::Context, screen: Rect) {
    let frost = crate::screen_context::frost();
    if frost <= 0.0 {
        return;
    }
    ctx.request_repaint();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("jai_screen_frost")));
    // (The ink's own tint: the glass mists over while he looks.)
    painter.rect_filled(screen, Rounding::ZERO, holo::a(holo::theme().ink(), 0.27 * frost));
}

/// Where the key hints end on the right: clear of the docked emblem's corner, which moves left
/// of the text console while that is out (`console` = how far it has slid in).
fn key_hints_right(screen: Rect, console: f32) -> f32 {
    screen.right() - console.max(0.0) - crate::hud_dock::DOCK_INSET - crate::holo_core::DOCKED_DIAMETER - 16.0
}

/// The key hints of the bottom strip, right-aligned to `right` on the line `mid`: each label
/// and its rect, left to right.
fn key_hints(painter: &egui::Painter, right: f32, mid: f32) -> Vec<(&'static str, Rect)> {
    let mut placed = Vec::new();
    let mut x = right;
    for label in ["ESC INTERRUPT", "F1 MUTE"] {
        let size = vec2(rail_width(painter, label) + 10.0, TELEMETRY_H);
        let rect = Rect::from_min_size(pos2(x - size.x, mid - size.y / 2.0), size);
        x = rect.left() - 18.0;
        placed.push((label, rect));
    }
    placed.reverse();
    placed
}

/// One key hint in `rect`: the key as a cap (an outlined block), what it does beside it.
fn paint_key_hint(painter: &egui::Painter, rect: Rect, label: &str, hot: bool) {
    let t = holo::theme();
    let (key, does) = label.split_once(' ').unwrap_or((label, ""));
    let cap = Rect::from_min_size(rect.min, vec2(rail_width(painter, key) + 8.0, rect.height()));
    holo::outline(painter, &holo::plate(cap, [0.0; 4]), if hot { t.accent() } else { holo::a(t.ink(), 0.40) });
    holo::text(painter, cap.center(), Align2::CENTER_CENTER, Face::Label, key, RAIL_TEXT, if hot { t.accent() } else { t.ink() });
    holo::text(painter, pos2(cap.right() + 6.0, rect.center().y), Align2::LEFT_CENTER, Face::Label, does, RAIL_TEXT, if hot { t.ink() } else { t.muted() });
}

/// The status pill's word for the HUD's state.
pub(crate) fn pill_word(state: &str, muted: bool, asleep: bool) -> &'static str {
    // (In game mode the model is out of VRAM, yet he hears, thinks and speaks.)
    let game = crate::vram_guard::in_game();
    match state {
        _ if asleep && !game => "STANDBY",
        // A wait says why.
        "thinking" if crate::websearch::searching() => "SEARCHING THE WEB",
        "thinking" if crate::brain::loading() => "LOADING BRAIN",
        "thinking" if game => "ON CPU · GAME",
        "thinking" => "THINKING",
        "speaking" => "SPEAKING",
        _ if muted => "MUTED",
        _ => "LISTENING",
    }
}

/// Every word the status pill shows (`pill_word`).
pub(crate) const PILL_WORDS: [&str; 8] = ["LISTENING", "THINKING", "SEARCHING THE WEB", "LOADING BRAIN", "ON CPU · GAME", "SPEAKING", "MUTED", "STANDBY"];

/// Whether `word` is one of the waits (he is working on it).
fn pill_works(word: &str) -> bool {
    matches!(word, "THINKING" | "SEARCHING THE WEB" | "LOADING BRAIN" | "ON CPU · GAME")
}

/// The state plate's colours for its word: lettering, lamp, fill, edge. Colour is information:
/// the accent for what he is doing (solid while he speaks), the alert for a microphone that
/// is off, plain ink while he only listens.
pub(crate) fn pill_colours(word: &str) -> [Color32; 4] {
    let t = holo::theme();
    match word {
        "LISTENING" => [t.ink(), t.accent(), Color32::TRANSPARENT, holo::a(t.ink(), 0.34)],
        _ if pill_works(word) => [t.hot(), t.accent(), holo::a(t.accent(), 0.10), t.accent()],
        "SPEAKING" => [Color32::BLACK, Color32::BLACK, t.accent(), t.accent()],
        "MUTED" => [t.alert(), t.alert(), Color32::TRANSPARENT, holo::a(t.alert(), 0.70)],
        _ => [t.muted(), t.faint(), Color32::TRANSPARENT, holo::a(t.ink(), 0.20)],
    }
}

/// The colour a state reads in where it stands alone on the canvas (a lamp, a ring).
pub(crate) fn state_colour(word: &str) -> Color32 {
    let t = holo::theme();
    match word {
        "LISTENING" => t.ink(),
        "SPEAKING" => t.hot(),
        "MUTED" => t.alert(),
        "STANDBY" => t.muted(),
        _ => t.accent(),
    }
}

/// The status pill's tooltip for its word.
fn pill_tip(word: &str) -> &'static str {
    match word {
        "LISTENING" => "Listening: say \"JAI\" and your command (F1 mutes the microphone).",
        "THINKING" => "Thinking: working on your request.",
        "SPEAKING" => "Speaking (Esc interrupts).",
        "MUTED" => "The microphone is off (F1 or Ctrl+M turns it on).",
        "SEARCHING THE WEB" => "Searching the web for your request.",
        "LOADING BRAIN" => "Loading the brain: the answer follows once the model is ready.",
        "ON CPU · GAME" => "Game mode: thinking on the CPU, so the game keeps the graphics card.",
        _ => "Standby: say \"JAI\" or click the emblem to wake him.",
    }
}

/// Where the status pill stands (room for its longest word): centred in the top row, or just
/// left of the header row's boxes where they reach the centre; None where the row leaves no
/// room at all.
fn pill_rect(painter: &egui::Painter, screen: Rect, header: &[Rect]) -> Option<Rect> {
    let width = PILL_WORDS.iter().map(|w| rail_width(painter, w)).fold(0.0, f32::max) + 36.0;
    let size = vec2(width, HEADER_H + 2.0);
    let centred = Rect::from_center_size(pos2(screen.center().x, screen.top() + 10.0 + HEADER_H / 2.0), size);
    if !header.iter().any(|r| r.expand(8.0).intersects(centred)) {
        return Some(centred);
    }
    let row_left = header.iter().map(|r| r.left()).fold(f32::INFINITY, f32::min);
    let beside = Rect::from_min_size(pos2(row_left - 14.0 - width, centred.top()), size);
    (beside.left() >= screen.left() + 12.0).then_some(beside)
}

/// The rule under the top rail: a hairline of ink across the screen with a tick at each end,
/// and on its middle a filament in the accent that widens and brightens with JAI's `voice`
/// (0-1). It stays above the blocks' area (`block_bounds`).
fn header_rule(painter: &egui::Painter, screen: Rect, voice: f32) {
    let band = header_rule_band(screen);
    let (left, right, y) = (band.left(), band.right(), band.top());
    if right - left < 80.0 {
        return;
    }
    let t = holo::theme();
    let voice = voice.clamp(0.0, 1.0);
    holo::rule(painter, pos2(left, y), pos2(right, y), holo::a(t.ink(), 0.16));
    for x in [left, right] {
        holo::rule(painter, pos2(x, y - 3.0), pos2(x, y + 3.0), holo::a(t.ink(), 0.45));
    }
    let mid = (left + right) / 2.0;
    let half = (right - left) * (0.05 + 0.09 * voice);
    let mut light = holo::Light::new(painter);
    light.neon(&[pos2(mid - half, y + 0.5), pos2(mid + half, y + 0.5)], false, 1.2, holo::a(holo::mix(t.accent(), t.hot(), voice), 0.55 + 0.45 * voice), 0.5 + 0.7 * voice);
    light.paint(painter);
}

/// Where the line under the header runs, its glow included.
fn header_rule_band(screen: Rect) -> Rect {
    let y = screen.top() + 12.0 + HEADER_H + 3.0;
    Rect::from_min_max(pos2(screen.left() + 12.0, y), pos2(screen.right() - 12.0, y + 3.0))
}

/// The state plate for `word` centred on `centre` at `alpha`: a leaning block with a lamp and
/// the word. While he works a bar of light runs along its foot; while he speaks it is solid.
fn paint_pill(painter: &egui::Painter, centre: Pos2, word: &str, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let [ink, lamp, fill, edge] = pill_colours(word);
    let rect = Rect::from_center_size(centre, vec2(rail_width(painter, word) + 36.0, HEADER_H + 2.0));
    let shape = holo_kit::leaning(rect);
    holo::fill(painter, &shape, holo::a(fill, alpha));
    holo::outline(painter, &shape, holo::a(edge, alpha));
    painter.rect_filled(Rect::from_center_size(pos2(rect.left() + 14.0, centre.y), vec2(5.0, 5.0)), Rounding::ZERO, holo::a(lamp, alpha));
    holo::text(painter, pos2(rect.left() + 24.0, centre.y), Align2::LEFT_CENTER, Face::Label, word, RAIL_TEXT, holo::a(ink, alpha));
    if pill_works(word) {
        let clock = painter.ctx().input(|i| i.time);
        let (from, to) = (rect.left() + 2.0, rect.right() - holo_kit::LEAN - 2.0);
        let run = 22.0;
        let x = from + (to - from - run) * crate::holo_motion::ease_in_out((clock * 0.8).fract() as f32);
        let mut light = holo::Light::new(painter);
        light.neon(&[pos2(x, rect.bottom()), pos2(x + run, rect.bottom())], false, 1.4, holo::a(holo::theme().hot(), alpha), 0.9);
        light.paint(painter);
    }
}

/// How strongly the voice rim shows for the microphone's VAD `phase` and level `mic` (0-1):
/// only while Sir speaks (not while JAI does), and only with the emblem at home.
fn rim_target(state: &str, phase: &str, mic: f32, dock: f32) -> f32 {
    let speech = matches!(phase, "pre_speech" | "speaking" | "hangover");
    if !speech || state == "speaking" || dock >= 0.5 {
        0.0
    } else {
        mic.clamp(0.0, 1.0).max(0.25) * (1.0 - 2.0 * dock)
    }
}

struct HudApp {
    app: Jai,
    rt: Handle,
    model: Arc<Mutex<HudModel>>,
    /// Interface settings currently applied to the window.
    applied: Option<InterfaceSettings>,
    palette: Palette,
    reactor: Reactor,
    last_frame: Instant,
    #[cfg_attr(not(windows), allow(dead_code))]
    hwnd: isize,
    memory_key: String,
    memory_value: String,
    memory_category: String,
    memory_importance: i32,
    memory_search: String,
    memory_search_at: Option<Instant>,
    ptt_button: bool,
    ptt_key: bool,
    ptt_sent: bool,
    confirm_shutdown: bool,
    confirm_delete_skill: Option<String>,
    /// Widgets and JAI's extra windows send their clicks through this.
    widget_host: widget_view::WidgetHost,
    /// The Integrations & Productivity Hub window.
    hub: integrations::Hub,
    workout: workout_view::WorkoutWindow,
    /// The Settings drawer (microphone, devices, system, memory, look) and its open tab.
    drawer_open: bool,
    drawer_tab: DrawerTab,
    /// The floating orb's window, as it was last placed: (size px, anchor, monitor) and where
    /// that put it (x, y, w, h in pixels).
    orb_geometry: Option<((f32, String, u8), [i32; 4])>,
    /// The gyroscopic ring emblem.
    emblem: Core,
    /// The board of blocks (textures).
    board: crate::hud_board::BoardView,
    /// `[ CONSOLE ]` (Ctrl+L, "open text interface"): the slide-out text console, its
    /// input line and whether the input should take the keyboard focus.
    console_open: bool,
    /// The drawer's slide (it keeps sliding out after a close until it is off screen).
    console_anim: crate::hud_board::UiAnimState,
    console_input: String,
    console_focus: bool,
    /// `[ CORE ]` / "show me your core": the 3D module sphere.
    core: crate::holo_mind::MindView,
    core_open: bool,
    /// Frames drawn, and the fullscreen watchdog's last check / re-applications.
    frames: u64,
    fullscreen_check: Instant,
    fullscreen_fixes: u32,
    /// `[ HISTORY ]`: the conversation and the saved reports.
    history_open: bool,
    history_reports: Vec<(i64, String, String, usize, usize)>,
    /// `[ CAMERA ]` and its source.
    camera_open: bool,
    camera_device: usize,
    camera_texture: Option<(u64, egui::TextureHandle)>,
    /// `[ SCREENS ]`.
    screens_open: bool,
    /// `--window-drill`: the real window driven and checked (`window_drill`).
    drill: Option<crate::window_drill::Drill>,
    /// The telemetry strip's peak hold (dBFS).
    peak_db: f32,
    /// The COMMS block's lines: (Sir's line, JAI's line) and when each last changed.
    captions: [(String, Instant); 2],
    /// Where Sir dragged the COMMS block (from its place above the telemetry strip).
    comms_offset: Vec2,
    /// Something covers the centre safe zone (last frame): only then the emblem docks.
    centre_covered: bool,
    /// How far the text console has slid in (0-1).
    console_slide: f32,
    /// `holo_theme::generation()` the palette was built at.
    theme_generation: u64,
    /// `[ PIN ]`: the console stays in front and Esc leaves it open.
    console_pinned: bool,
    /// The last click, key or turn (the emblem's idle morph waits 12 s after it).
    last_activity: Instant,
    /// The console sits against the right edge (the board and the emblem keep left of it).
    console_at_edge: bool,
    /// Ctrl+F: the search across every open block, its text and whether it takes the focus.
    find_open: bool,
    find_text: String,
    find_focus: bool,
    /// The console's predictive completions for what is typed so far (asked again only when
    /// the line changes).
    completions: (String, Vec<String>),
    /// The header menu chosen from `[ MORE ]` in the last frame (it opens in this one).
    header_menu_next: Option<&'static str>,
    /// The board's area as the last frame laid it out (right of the widgets column when that
    /// shows something, left of the console while it is out).
    board_area_now: Rect,
    /// The window's place as last seen (x, y, width, height in physical pixels) and, while it
    /// has not been logged yet, since when it stands there (`note_window`).
    window_seen: ([i32; 4], Option<Instant>),
    /// The header row's boxes in this frame (the status pill keeps clear of them).
    header_taken: Vec<Rect>,
    /// The status pill's word, the word before it and when it changed (they cross-fade).
    pill: (&'static str, &'static str, Instant),
    /// How strongly Sir's voice shows as the tick rim round the emblem (0-1).
    voice_rim: f32,
    /// Music plays and the core shows it: its voice ring takes the PC's real spectrum.
    emblem_music: bool,
    /// How tall the console's foot (the completions and the input line) was in the last frame:
    /// the log above it takes what is left, so the input line is never pushed out of the
    /// console when the completions appear or wrap.
    console_foot: f32,
    /// How much of the edge strips shows (1: all of them; the calm HUD lets them step back).
    calm: f32,
    /// How far night mode has dimmed the HUD (0 day … 1 night).
    night: f32,
    /// The start-up sequence while it plays: since when, whether its sound has played, and
    /// which one it is.
    /// Where the left column (widgets, cards waiting for an answer) stands, while it shows.
    system_column_rect: Option<Rect>,
    boot: Option<(Instant, bool, crate::startup_sequence::Kind)>,
    /// The sequence has just ended: the notifications that waited behind it start their time.
    boot_done: bool,
    /// How far the idle screen has come up (0 in use … 1 idle).
    idle: f32,
    /// The utterance Sir is in: since when, and the loudest his voice was (dBFS).
    mic_speech: Option<(Instant, f32)>,
    /// What is wrong with the way he hears Sir ("TOO QUIET"), and since when it has been shown.
    mic_verdict: Option<(&'static str, Instant)>,
    /// Where he stands as the desktop's background (`wallpaper::tick`), as the last frame saw it.
    backdrop: crate::wallpaper::Standing,
    /// The orb that leaves the background for a corner of the monitor Sir is using.
    corner_orb: crate::wallpaper::Orb,
    /// The monitor the corner orb is out on (its work area) and how large it is there.
    orb_out_on: Option<([i32; 4], i32)>,
    /// What had been said when the corner orb last set out: (Sir's last line, JAI's last line).
    /// Only what is said after that stands beside it.
    orb_words: (String, String, Instant),
    /// Where the emblem stands on the board in this frame (its centre and radius, points).
    emblem_spot: (Pos2, f32, f32),
    /// The console's own window is to take the keyboard as soon as it is up (Sir called it).
    console_wants_keys: bool,
    /// `JAI_DEMO=desk-click`: how far the stand-in for a click on the desktop has got (frames).
    demo_click: u8,
    /// When the corner orb's window came up, and when it was last looked after (`tend_orb`).
    orb_tended: (Instant, Instant),
    /// The corner orb's keys are up (a right click on it), since when.
    orb_menu: Option<Instant>,
    /// Sir's hand was last on the corner orb (its keys, its wheel), when: it does not fly home
    /// under his hand.
    orb_handled: Option<Instant>,
    /// The frames the corner orb's window has stood over the emblem since it landed (0: it
    /// has not just landed).
    orb_landed: u8,
    /// The work area of the monitor Sir was using when the corner orb last looked.
    orb_monitor: Option<[i32; 4]>,
    /// Where Sir holds the desktop clock while he drags it (its top middle).
    clock_drag: Option<Pos2>,
}

/// A running timer's word in the strip and by the orb: its own name ("pasta timer" → "PASTA"),
/// else TIMER or STOPWATCH ("5 minute timer" has no name).
fn timer_word(title: &str, countdown: bool) -> String {
    let name = title.trim().trim_end_matches(" timer").trim_end_matches(" Timer");
    match name.to_lowercase().as_str() {
        n if !n.is_empty() && !n.starts_with(|c: char| c.is_ascii_digit()) && !matches!(n, "timer" | "stopwatch" | "countdown") => name.to_uppercase(),
        _ if countdown => "TIMER".to_string(),
        _ => "STOPWATCH".to_string(),
    }
}

/// What the corner orb's window shows in a frame.
struct CornerOrbContent {
    /// The orb's diameter (physical pixels).
    orb_px: f32,
    /// What he is at for longer: Some(share done) for a timer, Some(None) for a search.
    progress: Option<Option<f32>>,
    /// The second and third running timers' shares: thinner rings inside the first.
    more_rings: Vec<f32>,
    /// The plate beside the orb is up.
    words: bool,
    /// The plate stands left of the orb (the orb rests in the right half of its monitor).
    words_left: bool,
    /// The plate shows his keys (a right click on the orb), not what is being said.
    menu: bool,
    said: String,
    answered: String,
    timer: Option<String>,
    state: String,
    live_words: bool,
    /// A question of his that waits for yes or no.
    question: Option<String>,
    /// The orders he offers for one he did not know.
    choices: Vec<String>,
    /// Game mode is on (its badge under the orb).
    game: bool,
}

/// What Sir did with the corner orb or the plate beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OrbClick {
    Nothing,
    /// A click on the orb: the console to type to him.
    Console,
    /// A double click: the board comes back.
    Board,
    /// He took hold of it: its window follows his hand.
    Drag,
    Yes,
    No,
    /// One of the orders offered (0-based).
    Choice(usize),
    /// A right click on the orb: its keys come up beside it, or go.
    Menu,
    /// His windows in front of the other programs' (a key of the menu).
    Windows,
    /// He stops listening for ten minutes (a key of the menu).
    Quiet,
    /// The wheel over the orb: larger (true) or smaller.
    Wheel(bool),
}

/// The wheel over the corner orb: what was turned before (`turned`, in notches) and this
/// frame's turns (a wheel's notches; a touchpad's points, 40 to a notch as egui counts a line).
/// A step, larger (true) or smaller, once a whole notch is turned, and what is left over. (A
/// frame with any turn in it was a whole step: a touchpad ran the orb to its largest at once.)
fn wheel_turn(turned: f32, events: &[egui::Event]) -> (f32, Option<bool>) {
    let turned = turned
        + events
            .iter()
            .map(|e| match e {
                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } => delta.y / 40.0,
                egui::Event::MouseWheel { delta, .. } => delta.y,
                _ => 0.0,
            })
            .sum::<f32>();
    if turned.abs() >= 1.0 { (0.0, Some(turned > 0.0)) } else { (turned, None) }
}

/// Looks after an orb's window, the corner orb's or the floating one's (both are titled
/// `wallpaper::ORB_TITLE` and never out at once): its black is see-through, beside a game
/// (`gaming`) it takes no click, and it shows itself without the focus once it has drawn.
fn tend_orb_window(ctx: &egui::Context, viewport: ViewportId, gaming: bool) {
    let drawn = ctx.cumulative_pass_nr_for(viewport) > 0;
    #[cfg(windows)]
    crate::win32::run_detached(move || {
        if let Some(hwnd) = crate::win32::own_window(crate::wallpaper::ORB_TITLE) {
            crate::win32_desktop::tend_orb(hwnd, gaming, drawn);
        }
    });
    // (KDE draws its see-through parts itself, from the window's own alpha.)
    #[cfg(not(windows))]
    if let Some(hwnd) = wm::own_window(crate::wallpaper::ORB_TITLE) {
        wm::skip_taskbar(hwnd, true);
        let _ = wm::click_through(hwnd, gaming);
        if drawn && !wm::is_visible(hwnd) {
            ctx.send_viewport_cmd_to(viewport, ViewportCommand::Visible(true));
        }
    }
}

/// "Back to the board": he is the desktop, so the desktop is shown, every other window
/// minimised. (Never from a test: Sir's windows are his.)
fn show_the_board(rt: &Handle) {
    if crate::platform::hands_off() {
        return;
    }
    rt.spawn(async {
        use crate::winctl::{manage, WindowAction, WindowTarget};
        if let Err(e) = manage(WindowAction::MinimizeAll, WindowTarget::Named(String::new())).await {
            warn!("[HUD] Could not show the desktop: {e}");
        }
    });
}

/// The corner orb's window: the emblem in its disc with the ring of what he is at, and beside
/// it the plate with what is being said, his question or the orders he offers.
fn corner_orb_ui(ui: &mut Ui, emblem: &Core, c: &CornerOrbContent) -> OrbClick {
    let mut did = OrbClick::Nothing;
    let all = ui.max_rect();
    // The orb in the middle of its window, room for the plate on either side of it: the plate
    // changes sides without the window moving under the orb.
    let size = all.height().min(c.orb_px / ui.ctx().pixels_per_point());
    let orb = Rect::from_min_size(pos2(all.center().x - size / 2.0, all.top()), Vec2::splat(size));
    let response = ui.allocate_rect(orb, Sense::click_and_drag());
    if response.drag_started_by(egui::PointerButton::Primary) {
        did = OrbClick::Drag;
    } else if response.double_clicked() {
        did = OrbClick::Board;
    } else if response.clicked() {
        did = OrbClick::Console;
    } else if response.secondary_clicked() {
        did = OrbClick::Menu;
    } else if response.hovered() {
        let id = egui::Id::new("jai_corner_orb_wheel");
        let before = ui.data(|d| d.get_temp::<f32>(id).unwrap_or(0.0));
        let (left, step) = ui.input(|i| wheel_turn(before, &i.events));
        ui.data_mut(|d| d.insert_temp(id, left));
        if let Some(larger) = step {
            did = OrbClick::Wheel(larger);
        }
    }
    // Nothing behind it (Sir, 2026-11-14: "the orb in the corner shouldn't have a background
    // colour"): the window's black is see-through (`win32_desktop::key_out_black`), and the
    // core is painted without the light round its lines, which on black would be a dark veil.
    emblem.paint_in_crisp(&ui.painter().with_clip_rect(orb), orb);
    let t = holo::theme();
    if let Some(share) = c.progress {
        // What runs (a timer, a download) as arcs round it: lines only, as the core is here.
        let (centre, r) = (orb.center(), orb.width() * 0.5 - 3.0);
        let (from, sweep) = match share {
            Some(done) => (-TAU / 4.0, TAU * done.clamp(0.0, 1.0)),
            None => ((ui.input(|i| i.time) as f32 * 2.2) % TAU, TAU * 0.22),
        };
        let mut light = holo::Light::new(ui.painter());
        light.arc(centre, r, from, from + sweep, 2.2, t.hot());
        for (k, done) in c.more_rings.iter().enumerate() {
            light.arc(centre, r - 5.0 * (k + 1) as f32, -TAU / 4.0, -TAU / 4.0 + TAU * done.clamp(0.0, 1.0), 1.4, holo::mix(t.accent(), t.deep(), 0.3 * (k + 1) as f32));
        }
        light.paint(ui.painter());
    }
    if c.game {
        // Game mode's badge: his mind waits in RAM, the game has the whole GPU.
        let width = rail_width(ui.painter(), "GAME · VRAM 0 MB") + 12.0;
        holo_kit::chip(ui.painter(), pos2(orb.center().x - width / 2.0, orb.bottom() - 16.0), "GAME · VRAM 0 MB", t.accent(), true, 1.0);
    }
    response.on_hover_text("Drag to move · click for the console · double-click for the board · right-click for more · wheel for its size");
    if !c.words && !c.menu {
        return did;
    }
    let plate = if c.words_left { Rect::from_min_max(all.min, pos2(orb.left() - 12.0, all.bottom())) } else { Rect::from_min_max(pos2(orb.right() + 12.0, all.top()), all.max) };
    let p = ui.painter().with_clip_rect(plate);
    crate::holo_frame::card(&p, plate, 0.0, 1.0);
    let inner = plate.shrink2(vec2(12.0, 9.0));
    let mut y = inner.top();
    let key = |ui: &mut Ui, at: Pos2, text: &str, lit: bool| -> (bool, f32) {
        let rect = Rect::from_min_size(at, vec2(holo_kit::key_width(ui.painter(), text), holo_kit::KEY_H));
        let response = ui.interact(rect, ui.id().with(("corner_orb_key", text.to_string())), Sense::click());
        holo_kit::key(ui.painter(), rect, text, response.hovered() as u8 as f32, response.is_pointer_button_down_on(), lit, 1.0);
        (response.clicked(), rect.right())
    };
    let head = |y: f32, text: &str| holo::text(&p, pos2(inner.left(), y), Align2::LEFT_TOP, Face::Label, text, RAIL_TEXT, t.accent()).bottom() + 6.0;
    if c.menu {
        // His keys (his choice of 2026-11-14: "Right-click menu"): what a click on the orb does
        // and what Sir would else have to say.
        let mut at = pos2(inner.left(), head(y, "J.A.I."));
        for (text, click) in [("CONSOLE", OrbClick::Console), ("MY WINDOWS", OrbClick::Windows), ("QUIET 10 MIN", OrbClick::Quiet), ("BOARD", OrbClick::Board)] {
            // (A second row once the first is full.)
            if at.x + holo_kit::key_width(ui.painter(), text) > inner.right() {
                at = pos2(inner.left(), at.y + holo_kit::KEY_H + 4.0);
            }
            let (clicked, right) = key(ui, at, text, false);
            if clicked {
                did = click;
            }
            at.x = right + 2.0;
        }
        return did;
    }
    if let Some(label) = &c.timer {
        holo::text(&p, inner.right_bottom(), Align2::RIGHT_BOTTOM, Face::Mono, label, 11.0, t.accent());
    }
    if !c.said.is_empty() {
        let said = holo::galley_within(&p, Face::Body, &c.said, 12.5, t.muted(), inner.width());
        let h = said.size().y;
        p.galley(pos2(inner.left(), y), said, t.muted());
        y += h + 5.0;
    }
    if let Some(question) = &c.question {
        // His question before a guess is carried out, and its two keys.
        let h = holo::wrapped(&p, pos2(inner.left(), y), inner.width(), Face::Body, crate::crash_guard::clip(question, 160), 13.5, t.ink(), 4);
        let row = pos2(inner.left(), (y + h + 7.0).min(inner.bottom() - holo_kit::KEY_H));
        let (yes, right) = key(ui, row, "YES", true);
        let (no, _) = key(ui, pos2(right + 2.0, row.y), "NO", false);
        if yes {
            did = OrbClick::Yes;
        } else if no {
            did = OrbClick::No;
        }
    } else if !c.choices.is_empty() {
        // The orders closest to the one he did not know: a click (or "the first one") takes one.
        let mut at = pos2(inner.left(), head(y, "Did you mean"));
        for (k, label) in c.choices.iter().enumerate().take(3) {
            let row = Rect::from_min_size(at, vec2(inner.width(), holo_ui::ROW_H));
            let response = ui.interact(row, ui.id().with(("corner_orb_choice", k)), Sense::click());
            holo_kit::row(&p, row, &format!("{:02}", k + 1), label, "", 0.0, response.hovered() as u8 as f32, 1.0);
            if response.clicked() {
                did = OrbClick::Choice(k);
            }
            at.y += holo_ui::ROW_H;
        }
    } else if !c.answered.is_empty() {
        holo::wrapped(&p.with_clip_rect(Rect::from_min_max(pos2(inner.left(), y), inner.max)), pos2(inner.left(), y), inner.width(), Face::Body, crate::crash_guard::clip(&c.answered, 220), 13.5, t.ink(), 8);
    } else if c.state != "idle" {
        // (What he is at, and Sir's words as he speaks, as under the emblem.)
        let _ = crate::hud_lines::paint(ui, pos2(inner.center().x, y + 2.0), inner.width(), &c.state, c.live_words, None);
    }
    did
}

/// Mini mode's place among the orb window's anchors (the window is a strip then), and how tall
/// the strip is (points).
const MINI_STRIP: &str = "strip";
const MINI_STRIP_HEIGHT: f32 = 40.0;

/// What the mini strip shows.
struct MiniStrip {
    /// What he is doing, as the status pill words it.
    word: &'static str,
    /// Sir's last words and his last answer.
    sir: String,
    jai: String,
    /// The running timers ("TIMER  04:32").
    timers: Vec<String>,
    clock: String,
}

/// Paints the mini strip over the whole of `ui` (a bar some forty points tall): on the left a
/// small core that turns, his name and what he is doing; in the middle Sir's last words and
/// his last answer; on the right the running timers, the time and the key back to the board.
/// Returns whether that key was clicked.
fn paint_mini_strip(ui: &mut Ui, s: &MiniStrip) -> bool {
    let t = holo::theme();
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    let mid = rect.center().y;
    p.rect_filled(rect, Rounding::ZERO, block_slate());
    holo::rule(&p, pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.right(), rect.bottom() - 1.0), holo::a(t.ink(), 0.20));
    holo::rule(&p, pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.left() + 46.0, rect.bottom() - 1.0), t.accent());
    // The core in small: two arcs that turn against each other round a lamp in the colour of
    // what he is doing.
    let lamp = state_colour(s.word);
    let centre = pos2(rect.left() + 20.0, mid);
    let turn = ui.input(|i| i.time) as f32 * if pill_works(s.word) || s.word == "SPEAKING" { 2.4 } else { 0.5 };
    let mut light = holo::Light::new(&p);
    light.neon_arc(centre, 10.5, turn, turn + 4.2, 1.3, t.accent(), 0.7);
    light.arc(centre, 7.0, -turn * 1.3, -turn * 1.3 + 3.4, 1.0, holo::a(t.ink(), 0.8));
    light.spark(centre, 5.0, holo::lit(lamp, 0.8));
    light.paint(&p);
    p.rect_filled(Rect::from_center_size(centre, vec2(4.0, 4.0)), Rounding::ZERO, lamp);
    let name = holo::text(&p, pos2(centre.x + 18.0, mid), Align2::LEFT_CENTER, Face::Label, "J.A.I.", 10.0, t.ink());
    let mut x = holo::text(&p, pos2(name.right() + 12.0, mid), Align2::LEFT_CENTER, Face::Label, s.word, RAIL_TEXT, lamp).right() + 26.0;
    // On the right: the key back to the board, the time, the running timers.
    let (key, keys_left) = holo_ui::keys_at(ui, egui::Id::new("mini_strip"), rect.right() - 10.0, mid, &[("BOARD", false, "Back to the full board (or say: mini mode off)")], 1.0);
    let mut right = holo::text(&p, pos2(keys_left - 14.0, mid), Align2::RIGHT_CENTER, Face::Figure, &s.clock, 16.0, t.ink()).left() - 18.0;
    for timer in &s.timers {
        right = holo::text(&p, pos2(right, mid), Align2::RIGHT_CENTER, Face::Mono, timer, 11.0, t.accent()).left() - 18.0;
    }
    // In between: his last words and JAI's last answer, each on one line and cut to its room.
    let room = (right - x).max(0.0);
    if room > 180.0 {
        let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let said = |x: f32, tag: &str, tag_ink: Color32, text: &str, ink: Color32, width: f32| {
            let tag = holo::text(&p, pos2(x, mid), Align2::LEFT_CENTER, Face::Label, tag, RAIL_TEXT, tag_ink).right() + 8.0;
            let words = holo::galley_within(&p, Face::Body, &one_line(text), 12.5, ink, width);
            let end = tag + words.size().x;
            p.galley(pos2(tag, holo::snap(mid - words.size().y / 2.0, holo::ppp(&p))), words, ink);
            end
        };
        if !s.sir.trim().is_empty() {
            x = said(x, "YOU", t.faint(), &s.sir, t.muted(), (room * 0.36 - 40.0).max(40.0)) + 24.0;
        }
        if !s.jai.trim().is_empty() && right - x > 90.0 {
            said(x, "J.A.I.", t.accent(), &s.jai, t.ink(), (right - x - 50.0).max(40.0));
        }
    }
    key.is_some()
}

/// Whether a scripted session was asked to show `what` (`JAI_DEMO=boot-long,idle,night`), so
/// that it can be looked at there. Never outside a scripted session.
fn demo(what: &str) -> bool {
    crate::platform::demo(what)
}

/// How long the HUD has been drawing (seconds).
fn hud_age() -> f32 {
    static BORN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BORN.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// A dragged emblem let go this near the middle of the screen (a share of its shorter side)
/// takes the middle.
const EMBLEM_SNAP: f32 = 0.09;

/// How dark night mode makes the HUD (the share of black laid over it).
const NIGHT_DIM: f32 = 0.42;

/// The calm HUD: the edge strips return when the pointer comes this near the top or the bottom
/// of the screen, and stand at this share of their strength otherwise.
const CALM_REACH: f32 = 96.0;
const CALM_REST: f32 = 0.2;

/// Whether the pointer at `at` is near the top or the bottom edge of `screen`.
fn near_strips(at: Pos2, screen: Rect) -> bool {
    screen.contains(at) && (at.y < screen.top() + CALM_REACH || at.y > screen.bottom() - CALM_REACH)
}

/// The emblem's inputs from the HUD's model and the board (`centre_covered`: a block, the
/// spotlight or a tool window reaches into the centre safe zone).
fn emblem_inputs(m: &HudModel, console_open: bool, core_open: bool, centre_covered: bool, idle: bool) -> CoreInputs {
    use crate::hud_board::ScreenState;
    let screen = crate::hud_board::screen_state();
    CoreInputs {
        // Thinking until he speaks: the voice then pulses the front emblem's ticks.
        thinking: crate::thinking_guard::is_thinking() && m.state != "speaking",
        researching: crate::hud_board::researching(),
        signed_off: screen == ScreenState::SignedOff,
        asleep: m.cfg.as_ref().is_some_and(|c| c.sleeping) || crate::vram_guard::is_dormant(),
        console_open,
        core_open,
        centre_covered,
        idle,
        idle_gyro: false,
    }
}

impl HudApp {
    fn new(cc: &eframe::CreationContext<'_>, app: Jai, rt: Handle, model: Arc<Mutex<HudModel>>, restore: Option<[i32; 4]>) -> Self {
        holo::install_fonts(&cc.egui_ctx);
        configure_style(&cc.egui_ctx, app.config().read(|c| c.interface.font_scale));
        crate::theme_store::attach_database(&app.config().read(|c| c.memory_db_path.clone()));
        *lock(&SHARED.ctx) = Some(cc.egui_ctx.clone());
        let hwnd = native_window(cc);
        if hwnd != 0 {
            #[cfg(windows)]
            crate::win32::install_hud_quit_hook(hwnd);
            if let Some(rect) = restore {
                wm::restore_placement(hwnd, rect);
            }
            SHARED.hwnd.store(hwnd, Ordering::Release);
            for command in lock(&SHARED.pending).drain(..) {
                wm::run_detached(move || apply_window_command(hwnd, command));
            }
            // He starts as the desktop's background: the desktop is handed the window before
            // it is first shown, so the board never lies over Sir's windows on its way there.
            if let Some(rect) = background_rect(&app.config().read(|c| c.interface.clone())) {
                crate::wallpaper::arrive(hwnd, rect, cc.egui_ctx.native_pixels_per_point().unwrap_or(1.0));
            }
        }
        info!("[HUD] Native window open (HUD board)");
        crate::start_mark("the window is open");
        crate::camera_feed::refresh_devices();
        Self::build(app, rt, model, hwnd)
    }

    /// The HUD's state for a window that exists (`new`) or for the offscreen HUD, which has
    /// none (`hwnd` 0: nothing here ever touches a native window then).
    fn build(app: Jai, rt: Handle, model: Arc<Mutex<HudModel>>, hwnd: isize) -> Self {
        let iface = app.config().read(|c| c.interface.clone());
        let widget_host = widget_view::WidgetHost { app: app.clone(), rt: rt.clone() };
        let inputs = emblem_inputs(&lock(&model), false, false, false, false);
        let now = Instant::now();
        Self {
            widget_host,
            hub: integrations::Hub::default(),
            workout: workout_view::WorkoutWindow::default(),
            drawer_open: false,
            drawer_tab: DrawerTab::Voice,
            orb_geometry: None,
            palette: Palette::new(&iface),
            app,
            rt,
            model,
            applied: None,
            reactor: Reactor::default(),
            last_frame: now,
            hwnd,
            memory_key: String::new(),
            memory_value: String::new(),
            memory_category: MEMORY_CATEGORIES[0].to_string(),
            memory_importance: 7,
            memory_search: String::new(),
            memory_search_at: None,
            ptt_button: false,
            ptt_key: false,
            ptt_sent: false,
            confirm_shutdown: false,
            confirm_delete_skill: None,
            emblem: Core::new(crate::holo_core::resolve(inputs)),
            board: crate::hud_board::BoardView::default(),
            core: crate::holo_mind::MindView::default(),
            core_open: false,
            console_open: false,
            console_anim: Default::default(),
            console_input: String::new(),
            console_focus: false,
            frames: 0,
            fullscreen_check: now,
            fullscreen_fixes: 0,
            history_open: false,
            history_reports: Vec::new(),
            camera_open: false,
            camera_device: 0,
            camera_texture: None,
            screens_open: false,
            drill: crate::window_drill::enabled().then(crate::window_drill::Drill::new),
            peak_db: -90.0,
            captions: [(String::new(), now), (String::new(), now)],
            comms_offset: Vec2::ZERO,
            centre_covered: false,
            console_slide: 0.0,
            theme_generation: 0,
            console_pinned: false,
            console_at_edge: true,
            last_activity: now,
            find_open: false,
            find_text: String::new(),
            find_focus: false,
            completions: (String::new(), Vec::new()),
            header_menu_next: None,
            board_area_now: Rect::NOTHING,
            console_foot: 46.0,
            calm: 1.0,
            night: 0.0,
            system_column_rect: None,
            boot: None,
            boot_done: false,
            idle: 0.0,
            mic_speech: None,
            mic_verdict: None,
            window_seen: ([0; 4], None),
            header_taken: Vec::new(),
            pill: ("LISTENING", "LISTENING", now),
            voice_rim: 0.0,
            emblem_music: false,
            backdrop: crate::wallpaper::Standing::Off,
            corner_orb: Default::default(),
            orb_out_on: None,
            orb_words: (String::new(), String::new(), now),
            emblem_spot: (Pos2::ZERO, 0.0, 0.0),
            console_wants_keys: false,
            demo_click: 0,
            orb_tended: (Instant::now(), Instant::now()),
            orb_menu: None,
            orb_handled: None,
            orb_landed: 0,
            orb_monitor: None,
            clock_drag: None,
        }
    }

    /// Whether the board is (or is becoming) the desktop's background: then it is no window,
    /// and none of a window's ways (fullscreen, a title bar, a move) is its own.
    fn as_background(&self) -> bool {
        matches!(self.backdrop, crate::wallpaper::Standing::Arriving | crate::wallpaper::Standing::On)
    }

    /// He as the desktop's background (`wallpaper`), always: the board's window is handed to the
    /// desktop once it stands, plain, where it is to be. Refused, he says why and lies under
    /// every other window until the desktop takes him (it is asked again).
    fn sync_background(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        use crate::wallpaper::Standing;
        // Every view of his is a window of its own, in every mode, wherever the HUD has windows
        // to give (`hud_dock::all_own`).
        crate::hud_dock::set_all_own(!ctx.embed_viewports());
        let rect = (self.hwnd != 0 && !ctx.embed_viewports()).then(|| background_rect(iface)).flatten();
        let now = crate::wallpaper::tick(self.hwnd, rect, ctx.native_pixels_per_point().unwrap_or(1.0), crate::hud_feel::game_in_front());
        if now == self.backdrop {
            return;
        }
        if let Standing::Refused(why) = &now {
            lock(&self.model).toast(format!("The desktop would not take me as its background ({why}). I'll ask it again."), true);
            desk::sink(self.hwnd);
        }
        self.backdrop = now;
        if !self.as_background() {
            self.corner_orb = Default::default();
            self.orb_out_on = None;
        }
        ctx.request_repaint();
    }

    /// Files dropped on the HUD (the reference's "+ DROP A FILE"): the last one is what the
    /// conversation is about from then on ("summarize this file", "what's in it?"), named in a
    /// notice and on the console's file line. While files hang over the HUD it says where to
    /// let go.
    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let (dropped, hovering) = ctx.input(|i| (i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect::<Vec<_>>(), !i.raw.hovered_files.is_empty()));
        if hovering {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("jai_file_drop")));
            let t = holo::theme();
            painter.rect_filled(screen, Rounding::ZERO, holo::a(t.void(), 0.55));
            // A slot in the middle of the screen, drawn in light, says where it goes.
            let zone = Rect::from_center_size(screen.center(), vec2(420.0, 120.0));
            let outline = holo::plate(zone, [0.0, 0.0, crate::holo_frame::CUT, 0.0]);
            let mut light = holo::Light::new(&painter);
            light.neon(&[outline[0], outline[1], outline[2], outline[3], outline[4], outline[0]], false, 1.4, t.accent(), 0.9);
            light.paint(&painter);
            holo::corner_ticks(&painter, zone.expand(6.0), crate::holo_frame::TICK, t.ink());
            holo::text(&painter, zone.center(), Align2::CENTER_CENTER, Face::Label, "Drop the file here", 11.0, t.ink());
            ctx.request_repaint();
        }
        let Some(path) = dropped.last() else { return };
        self.app.tools().focus.note_file(path);
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        info!("[HUD] File dropped on the HUD: {}", path.display());
        self.app.notice(format!("FILE LOADED // {name} — ask me about it (\"summarize this file\")"));
    }

    /// The microphone on / off (F1, Ctrl+M, the strip's `[LIVE]` and `F1 MUTE`).
    fn toggle_mic(&mut self) {
        let enabled = !self.app.config_state().mic_enabled;
        self.command(HudCommand::SetMic { enabled });
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let (space, escape, focused) = ctx.input(|i| (i.key_down(Key::Space), i.key_pressed(Key::Escape), i.focused));
        if escape {
            // Esc closes the innermost thing first: a block filling the screen, the console, the
            // spotlight, an overlay, then JAI's speech.
            if crate::hud_dock::exit_fullscreen().is_some() {
            } else if self.find_open {
                self.find_open = false;
                self.find_text.clear();
                crate::hud_dock::set_search("");
            } else if self.core_open {
                self.core_open = false;
            } else if self.console_open && !self.console_pinned {
                self.console_open = false;
            } else if crate::hud_board::minimize() {
            } else if self.history_open || self.screens_open {
                self.history_open = false;
                self.screens_open = false;
            } else if lock(&self.model).state == "idle" && crate::hud_dock::close_top_block(ctx.pointer_hover_pos()) {
                // The top block (under the pointer, else the latest): it leaves by its own
                // [ CLOSE ] in its next frame. While he thinks or speaks, Esc stops him first.
            } else {
                self.command(HudCommand::Interrupt);
            }
        }
        self.ptt_key = space && !typing && focused;
        // Ctrl+1/2/3 reply mode, Ctrl+M microphone, Ctrl+L type to JAI, Ctrl+B briefing.
        let ctrl = egui::Modifiers::COMMAND;
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(ctrl, key));
        for (key, mode) in [(Key::Num1, VoiceResponseMode::MirrorInput), (Key::Num2, VoiceResponseMode::AlwaysSpeak), (Key::Num3, VoiceResponseMode::TextOnly)] {
            if pressed(key) {
                self.command(HudCommand::SetResponseMode { mode });
            }
        }
        // Ctrl+M or F1 (as on the reference board): the microphone on / off.
        if pressed(Key::M) || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F1)) {
            self.toggle_mic();
        }
        if pressed(Key::L) {
            self.console_open = !self.console_open;
            self.console_focus = self.console_open;
        }
        // Ctrl+K: the command palette (the console's line with its completions).
        if pressed(Key::K) {
            self.console_open = true;
            self.console_focus = true;
        }
        // Ctrl+F: search across every open block.
        if pressed(Key::F) {
            self.find_open = true;
            self.find_focus = true;
        }
        if pressed(Key::B) {
            self.command(HudCommand::Chat { text: "Brief me".into() });
        }
        // Ctrl + Plus / Minus / 0: the whole interface zooms and reflows (and stays that way).
        let zoom = if pressed(Key::Plus) || pressed(Key::Equals) {
            Some(1.1)
        } else if pressed(Key::Minus) {
            Some(1.0 / 1.1)
        } else if pressed(Key::Num0) {
            Some(0.0)
        } else {
            None
        };
        if let Some(step) = zoom {
            let mut iface = self.app.config().read(|c| c.interface.clone());
            iface.scale = if step == 0.0 { 1.0 } else { iface.scale * step };
            iface.sanitize();
            self.set_interface(iface);
        }
    }

    /// The emblem's state (`rose_emblem_renderer::resolve_state`): docked only while something
    /// covers the centre of the screen (left of the text console when it is out), the thinking
    /// stack while he thinks, researches, sleeps or is signed off, else the front emblem.
    fn step_emblem(&mut self, m: &HudModel, dt: f32) {
        // Idle: no turn, no speech and no click or key for 12 s (the idle morph may run).
        if m.state != "idle" {
            self.last_activity = Instant::now();
        }
        let idle = self.last_activity.elapsed() > Duration::from_secs(12);
        let inputs = emblem_inputs(m, self.console_open, self.core_open, self.centre_covered, idle);
        self.emblem.set_console_width(self.console_reserve());
        let before = self.emblem.pose();
        self.emblem.follow(inputs);
        if self.emblem.pose() != before {
            info!("[HUD] Emblem: {:?} → {:?}", before, self.emblem.pose());
        }
        // Its mood: what he is doing (and one red flash for an order that failed).
        use crate::holo_core::Mood;
        self.emblem.set_mood(match m.state.as_str() {
            "listening" => Mood::Listening,
            "thinking" => Mood::Thinking,
            "speaking" => Mood::Speaking,
            _ => Mood::Rest,
        });
        if crate::hud_lines::take_fault() {
            self.emblem.flash_fault();
        }
        // His own voice moves his rings, Sir's shows as the white ticks round him (the voice
        // rim's level, while it is on); music gives the real spectrum, a voice its level.
        let music = self.emblem_music.then(crate::pc_audio::bands).map(|bands| {
            let mut out = [0.0; crate::holo_core::BANDS];
            for (i, v) in out.iter_mut().enumerate() {
                *v = bands[i * crate::pc_audio::BANDS / crate::holo_core::BANDS];
            }
            out
        });
        // The time of day (his scale ring is the clock) and the word for what he is doing.
        use chrono::Timelike;
        let now = chrono::Local::now();
        let muted = m.level.as_ref().is_some_and(|l| l.phase == "muted");
        let word = match pill_word(&m.state, muted, inputs.asleep) {
            "LISTENING" if m.state != "listening" => "STANDING BY",
            word => word,
        };
        let feed = crate::holo_core::Feed {
            voice: self.reactor.emblem_voice().max(welcome_level() * 0.6),
            heard: self.voice_rim,
            bands: music,
            busy: inputs.busy(),
            asleep: inputs.asleep,
            layers: crate::mind_activity::layers(Instant::now()),
            day_secs: now.num_seconds_from_midnight() as f32 + now.nanosecond() as f32 / 1e9,
            word,
        };
        self.emblem.step(dt, &feed);
        if crate::mind_activity::active(Instant::now()) {
            request_repaint_soon();
        }
    }

    /// Where the window really is, in the log whenever it has changed and come to rest (in
    /// physical pixels, as the monitors are listed): what a span, a move to another monitor or
    /// a change to fullscreen actually did.
    fn note_window(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        let Some(outer) = ctx.input(|i| i.viewport().outer_rect) else { return };
        let ppp = ctx.pixels_per_point();
        let now = [(outer.left() * ppp).round() as i32, (outer.top() * ppp).round() as i32, (outer.width() * ppp).round() as i32, (outer.height() * ppp).round() as i32];
        let (seen, since) = self.window_seen;
        if seen != now {
            // On the move: noted once it has stood still for half a second.
            self.window_seen = (now, Some(Instant::now()));
        } else if since.is_some_and(|s| s.elapsed() > Duration::from_millis(500)) {
            self.window_seen.1 = None;
            let shape = if self.as_background() {
                "the desktop's background"
            } else if iface.span_all_monitors {
                "one board across every monitor"
            } else if iface.fullscreen {
                "the fullscreen board"
            } else {
                "a window"
            };
            info!("[HUD] Window at {},{} {}×{} ({shape})", now[0], now[1], now[2], now[3]);
        }
    }

    /// The fullscreen board fills its monitor: `Fullscreen(true)` on the first frame, and every
    /// 1.5 s a check that the window really covers the monitor (re-applied when it does not).
    fn enforce_fullscreen(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        self.frames += 1;
        // (As the desktop's background its place is the desktop's affair: `wallpaper::tick`.)
        if self.as_background() || background_rect(iface).is_some() {
            return;
        }
        // "Use all monitors": the window covers the box around every monitor.
        if iface.span_all_monitors && self.hwnd != 0 && (self.frames == 2 || self.fullscreen_check.elapsed() > Duration::from_millis(1500)) {
            self.fullscreen_check = Instant::now();
            let hwnd = self.hwnd;
            if let Some(bounds) = crate::monitors::virtual_bounds(&crate::monitors::list()) {
                let state = wm::window_state(hwnd);
                if state.as_ref().is_none_or(|(r, _, _)| r.iter().zip(bounds).any(|(a, b)| (a - b).abs() > 2)) {
                    let maximised = state.is_some_and(|(_, max, _)| max);
                    #[cfg(not(windows))]
                    if maximised {
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    }
                    wm::run_detached(move || {
                        #[cfg(windows)]
                        if maximised {
                            crate::win32::show_window(hwnd, crate::win32::WindowCmd::Restore);
                        }
                        if let Err(e) = wm::set_window_rect(hwnd, bounds) {
                            warn!("[HUD] Could not span the monitors: {e}");
                        }
                    });
                }
            }
        }
        if !iface.fullscreen {
            return;
        }
        // (A test session's board stays the borderless window that covers its monitor.)
        let quiet = crate::platform::quiet_session();
        if self.frames == 1 && !quiet {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
        }
        if self.hwnd != 0 && self.frames > 3 && self.fullscreen_check.elapsed() > Duration::from_millis(1500) {
            self.fullscreen_check = Instant::now();
            let hwnd = self.hwnd;
            let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
            if !minimized && wm::is_visible(hwnd) && !wm::covers_monitor(hwnd) {
                self.fullscreen_fixes += 1;
                if self.fullscreen_fixes <= 3 || self.fullscreen_fixes.is_multiple_of(20) {
                    warn!("[HUD] The fullscreen board does not cover its monitor; re-applying ({}×)", self.fullscreen_fixes);
                }
                if !quiet {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                }
                let cover = move || {
                    if let Err(e) = wm::cover_monitor(hwnd) {
                        warn!("[HUD] Could not stretch the board over its monitor: {e}");
                    }
                };
                #[cfg(windows)]
                crate::win32::run_detached(cover);
                #[cfg(not(windows))]
                cover();
            } else {
                // It covers the monitor but is still maximised underneath (a maximised window
                // that went fullscreen, F11): a plain window of the same rect, nothing moves.
                #[cfg(windows)]
                if !minimized && crate::win32::window_state(hwnd).is_some_and(|(_, maximised, _)| maximised) {
                    crate::win32::run_detached(move || match crate::win32::unmaximise_in_place(hwnd) {
                        Ok(true) => info!("[HUD] The fullscreen board is no longer maximised underneath"),
                        Ok(false) => {}
                        Err(e) => warn!("[HUD] Could not let go of the board's maximised state: {e}"),
                    });
                }
            }
        }
    }

    /// Pure orb mode: a transparent window with the emblem and nothing else.
    /// Mini mode: the window is a slim strip across the top of its monitor, always in front:
    /// what he is doing, Sir's last words, his last answer, the running timers and the time.
    /// `[ BOARD ]` (or "mini mode off") brings the board back.
    fn mini_strip(&mut self, ctx: &egui::Context, m: &HudModel) {
        let muted = m.level.as_ref().is_none_or(|l| l.phase == "muted") || m.cfg.as_ref().is_some_and(|c| !c.mic_enabled) || crate::listening_lock::is_locked();
        let asleep = m.cfg.as_ref().is_some_and(|c| c.sleeping) || crate::vram_guard::is_dormant();
        let timers = self.running_timers().into_iter().map(|(line, _)| line).collect();
        let last = |who: Speaker| m.messages.iter().rev().find(|l| l.who == who).map(|l| l.text.clone()).unwrap_or_default();
        let strip = MiniStrip { word: pill_word(&m.state, muted, asleep), sir: last(Speaker::Sir), jai: last(Speaker::Jai), timers, clock: chrono::Local::now().format("%H:%M").to_string() };
        let mut leave = false;
        egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| leave = paint_mini_strip(ui, &strip));
        if leave {
            self.app.config().update(|c| c.interface.mini_strip = false);
            show_the_board(&self.rt);
        }
        // (The time, the timers and the little emblem move.)
        ctx.request_repaint_after(Duration::from_millis(if m.state == "idle" { 250 } else { 50 }));
    }

    fn pure_orb(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings) {
        if iface.orb_anchor == MINI_STRIP {
            self.mini_strip(ctx, m);
            return;
        }
        egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
            let rect = ui.max_rect();
            let response = ui.allocate_rect(rect, Sense::click_and_drag());
            if response.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                if iface.orb_anchor != "free" {
                    // It stays where Sir leaves it (a resize keeps its centre).
                    let mut next = iface.clone();
                    next.orb_anchor = "free".into();
                    if let Some((placed, _)) = self.orb_geometry.as_mut() {
                        *placed = (next.orb_size_px, next.orb_anchor.clone(), next.orb_monitor);
                    }
                    self.set_interface(next);
                }
            }
            if response.hovered() {
                let wheel = ui.input(|i| i.smooth_scroll_delta.y);
                if wheel.abs() > 0.5 {
                    let mut next = iface.clone();
                    next.orb_size_px = (iface.orb_size_px * (1.0 + wheel * 0.0015).clamp(0.85, 1.15)).round();
                    next.sanitize();
                    if next.orb_size_px != iface.orb_size_px {
                        self.set_interface(next);
                    }
                }
            }
            if response.double_clicked() {
                let mut next = iface.clone();
                next.orb_mode = false;
                self.set_interface(next);
                show_the_board(&self.rt);
            } else if response.clicked() && crate::vram_guard::is_dormant() {
                // The floating emblem is a wake button during the zero-VRAM standby.
                crate::vram_guard::wake("orb");
            }
            ui.set_opacity(iface.orb_opacity);
            self.emblem.paint_in_crisp(&ui.painter().with_clip_rect(rect), rect);
            // A silent quick command's badge, briefly, under the emblem.
            if let Some((label, ok, until)) = &m.quick {
                if *until > Instant::now() {
                    let at = pos2(rect.center().x, rect.bottom() - 14.0);
                    holo_kit::chip(ui.painter(), pos2(at.x - rail_width(ui.painter(), label) / 2.0 - 6.0, at.y - 7.0), label, if *ok { holo::theme().accent() } else { holo::theme().alert() }, true, 1.0);
                }
            }
            response.on_hover_text("Drag to move · wheel to resize · double-click for the board");
        });
    }

    /// The whole HUD board canvas.
    fn canvas(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings, dt: f32) {
        egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
            // The empty canvas moves the frameless window (not the fullscreen board, nor the one
            // that spans the monitors); a double-click toggles fullscreen.
            // (As the desktop's background the empty canvas is the desktop's: a click on it is a
            // click on the desktop, and only his keys are his.)
            if !self.as_background() {
                let canvas = ui.interact(ui.max_rect(), ui.id().with("hud_canvas"), Sense::click_and_drag());
                if canvas.drag_started_by(egui::PointerButton::Primary) && !iface.fullscreen && !iface.span_all_monitors {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                // (Not on the board across every monitor: there a double click ended the span,
                // which no one who double-clicks an empty spot means. In Sir's log of 2026-11-13
                // the span he had just asked for was gone a second and a half later with no
                // order in between; whether a double click did it is not known, it is the one
                // way an empty spot could.)
                if canvas.double_clicked() && !iface.span_all_monitors {
                    let mut next = iface.clone();
                    next.fullscreen = !iface.fullscreen;
                    self.set_interface(next);
                }
            }
            // Everything of the HUD proper is laid out on its home screen: the window, or Sir's
            // first monitor while one board spans them all.
            let screen = home_screen(ctx);

            let system = self.system_column(ctx, m, screen);
            // While the text console is out the board reflows left of it (its reserved corner
            // cell, where the emblem docks, too).
            self.console_anim.target_open = self.console_open;
            self.console_slide = self.console_anim.step(dt);
            if self.console_anim.moving() {
                ctx.request_repaint();
            }
            let area = board_area(screen, if system { SYSTEM_COLUMN + 16.0 } else { 0.0 }, self.console_reserve());
            self.board_area_now = area;
            for cmd in std::mem::take(&mut *lock(&CORE_REQUESTS)) {
                use crate::holo_mind::CoreCommand;
                match cmd {
                    CoreCommand::Open(open) => self.core_open = open,
                    other => {
                        self.core_open = true;
                        self.core.apply(other);
                    }
                }
            }
            // The emblem stays in the middle of the screen. It leaves it for a report on the
            // board (whose blocks are laid out around its corner), for a feature panel on the
            // centre stage (the radar, the disk map, the Earth) and when Sir tells it to get out
            // of the way; any other window never moves it (windows open around it, and one Sir
            // drags over it simply covers it). The core sphere docks it on its own.
            let board_rects = crate::hud_board::visible_rects(area);
            // (As the desktop's background a covered emblem has its orb in the corner instead.)
            self.centre_covered = iface.orb_aside || crate::hud_dock::centre_covered(screen, &board_rects) || crate::hud_dock::stage_covers_centre(screen) || (!self.as_background() && stage_window_covers_centre(ctx, screen));
            // The board fades out while the core sphere fades in over it.
            let core_t = ctx.animate_bool_with_time(egui::Id::new("hud_core_view"), self.core_open, crate::holo_motion::ARRIVE);
            if core_t < 1.0 {
                let (board_click, board_moving) = ui.scope(|ui| {
                    ui.set_opacity(1.0 - core_t);
                    self.board.paint(ui, area, &self.rt)
                })
                .inner;
                match board_click {
                    // A video tile clicked in the spotlight: the board has already opened it.
                    Some(crate::hud_board::BoardClick::Watched(url)) => info!("[HUD] Video tile → {url}"),
                    // A key of the suggestion bar: its tool call, as it stands. (The board's own
                    // actions teach the bar where they are carried out; the others here.)
                    Some(crate::hud_board::BoardClick::Suggested(kind, key)) => {
                        info!("[HUD] Suggestion [ {} ] → {} {}", key.label, key.tool, key.args);
                        if key.tool != "hud_board" {
                            crate::hud_suggestions::learned(kind, key.label);
                        }
                        self.run_calls(vec![(key.tool, key.args)]);
                    }
                    _ => {}
                }
                if board_moving {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
            }
            if core_t > 0.0 {
                // The core view: the complex core large, whichever core stands on the board.
                let look = crate::holo_core::CoreLook { alpha: core_t, ..self.emblem.look() };
                let active = crate::mind_activity::recent(Instant::now());
                self.core.show(ui, area, &look, &crate::mind_activity::LAYERS, &active);
                if core_t < 1.0 {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
            }

            // The music: while something plays on the PC the emblem moves with it (he only
            // listens to the sound; never while a game is in front, never to his own voice).
            // (A scripted session listens only when it was asked to show the music: `JAI_DEMO=music`.)
            // (Also while a game is in front: music beside a game is what Sir listens to. It
            // then draws at a lower rate.)
            let music_on = iface.music_visuals && !windows_still(ctx) && (!crate::platform::in_test_session() || demo("music"));
            crate::pc_audio::want(crate::pc_audio::Use::Spectrum, music_on);
            crate::music_lyrics::want(music_on, crate::hud_feel::lyrics(), &self.rt);
            let (bass, _) = if music_on { crate::pc_audio::bass_and_level() } else { (0.0, 0.0) };
            // (While a music player is making sound: what else the PC plays is not music. Not by
            // how loud it is: a quiet passage, or a player's channel set low, is music too. Seen
            // live 2026-11-12: the ring came and went with the song's loudness.)
            let music = music_on && crate::music_lyrics::playing() && m.state != "speaking";
            self.emblem.set_music(if music { bass } else { 0.0 });
            // The emblem (drawn over the board; its corner cell is kept free), wherever Sir put
            // it: on any monitor of a board that reaches over several.
            self.emblem.set_rooms(&monitor_regions(ctx));
            self.emblem.glide(screen, dt);
            let frame = self.emblem.frame(screen);
            // (Its stage: the home screen and the monitor it stands on.)
            let stage = screen.union(self.emblem.room_at(screen, frame.centre));
            // (While the orb is out in a corner the background has no emblem: it has left it.)
            if self.corner_orb.is_home() {
                self.emblem.paint(&ui.painter_at(stage), screen);
            }
            self.emblem_spot = (frame.centre, frame.radius, frame.rest);
            *lock(&CANVAS_ORIGIN) = Some((frame.centre, frame.rest, frame.dock, self.reactor.emblem_voice()));
            *lock(&BOARD_ORIGIN) = ctx.input(|i| i.viewport().inner_rect).map(|r| r.min);
            // (Said once, a moment after the start: where the emblem stands, in the log.)
            if self.frames == 150 {
                info!("[HUD] Emblem at {:.0},{:.0} (radius {:.0}) on a screen of {:.0}×{:.0} at {:.0},{:.0}", frame.centre.x, frame.centre.y, frame.radius, screen.width(), screen.height(), screen.left(), screen.top());
            }
            // New windows open around the orb and the board's blocks, never on them.
            let mut taken = board_rects;
            taken.extend(self.system_column_rect.filter(|_| system));
            crate::hud_dock::publish_obstacles(Some(Rect::from_center_size(frame.centre, Vec2::splat(frame.radius * 2.3))), taken);
            let docked = crate::hud_dock::docked_centre(screen, self.console_reserve());
            crate::hud_dock::publish_docked_spot(Some(Rect::from_center_size(docked, Vec2::splat(crate::holo_core::DOCKED_DIAMETER + 8.0))));
            let hit = Rect::from_center_size(frame.centre, vec2(frame.radius * 1.6, frame.radius * 1.6));
            let emblem = ui.interact(hit, ui.id().with("hud_emblem"), Sense::click_and_drag());
            // Drag the emblem anywhere on the board; it stays there (and windows open around it).
            // (Near the middle of the screen it takes the middle: its home. No lines are drawn
            // for it any more: Sir wants no alignment help on his screen, 2026-11-13.)
            let middle = screen.center();
            let near_middle = |p: Pos2| (p - middle).length() <= EMBLEM_SNAP * screen.width().min(screen.height());
            if let Some(p) = emblem.interact_pointer_pos().filter(|_| emblem.dragged_by(egui::PointerButton::Primary)) {
                // (On any monitor: near the middle of the one it is over, it takes that middle.)
                let room = self.emblem.room_at(screen, p);
                let p = if (p - room.center()).length() <= EMBLEM_SNAP * room.width().min(room.height()) { room.center() } else { p };
                self.emblem.drag_to(vec2((p.x - screen.left()) / screen.width(), (p.y - screen.top()) / screen.height()));
                ctx.request_repaint();
            }
            if emblem.drag_stopped() {
                if let Some(f) = self.emblem.custom_home() {
                    let at = screen.min + vec2(f.x * screen.width(), f.y * screen.height());
                    let mut next = iface.clone();
                    next.orb_board_pos = if near_middle(at) { None } else { Some([f.x, f.y]) };
                    info!("[HUD] Emblem home: {}", match next.orb_board_pos { None => "the middle of the screen".to_string(), Some([x, y]) => format!("Sir's spot at {:.0} %, {:.0} % of the screen", x * 100.0, y * 100.0) });
                    self.set_interface(next);
                }
            }
            if emblem.double_clicked() {
                let mut next = iface.clone();
                next.orb_board_pos = None;
                self.set_interface(next);
            }
            if emblem.clicked() {
                if crate::vram_guard::is_dormant() {
                    crate::vram_guard::wake("emblem");
                } else if matches!(crate::hud_board::screen_state(), crate::hud_board::ScreenState::SignedOff) {
                    crate::hud_board::wake();
                }
            }

            // He heard his name: the emblem answers with a ripple (his silent "yes?").
            if let Some(age) = crate::hud_lines::woke_age().filter(|_| frame.dock < 0.5) {
                if crate::holo_core::ripple(&ui.painter_at(stage), frame.centre, frame.radius, age) {
                    ctx.request_repaint();
                }
            }
            if iface.voice_rim {
                self.voice_rim(&ui.painter_at(stage), m, &frame, dt);
            } else {
                self.voice_rim = 0.0;
            }
            // The voice lock turned an order away: the emblem says why nothing happens.
            if let Some(strength) = crate::hud_lines::turned_away_strength().filter(|_| frame.dock < 0.5) {
                crate::hud_lines::paint_lock_label(&ui.painter_at(stage), frame.centre, frame.radius, strength);
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            // The lines under the emblem: his words while he speaks, what JAI is working on,
            // the song's line, and the offer of an order heard without his name.
            // (Under the desktop clock, where that stands under the emblem.)
            let lines_top = if frame.dock < 0.5 { crate::hud_lines::top_under(frame.centre, frame.radius) + vec2(0.0, self.clock_room(iface, frame.rest)) } else { pos2(screen.center().x, screen.bottom() - TELEMETRY_H - 110.0) };
            // The music's spectrum is the core's own voice ring (when Sir's voice is not there).
            self.emblem_music = music && frame.dock < 0.5 && self.voice_rim < 0.05;
            if let Some(next) = music_frame(self.emblem_music, crate::wallpaper::hidden() && self.corner_orb.is_home(), crate::hud_feel::game_in_front()) {
                ctx.request_repaint_after(next);
            }
            // Under it: the line being sung (lyrics on), else what plays.
            let lyric = crate::hud_feel::lyric_line()
                .or_else(|| (crate::hud_feel::lyrics() && crate::music_lyrics::none_found()).then(|| "no synced lyrics for this song".to_string()))
                .or_else(|| music.then(crate::music_lyrics::new_title_line).flatten());
            if let Some(order) = crate::hud_lines::paint(ui, lines_top, (screen.width() * 0.5).min(760.0), &m.state, iface.live_words, lyric.as_deref()) {
                info!("[HUD] The offered order was taken: '{order}'");
                self.command(HudCommand::Chat { text: order });
            }
            if iface.comms_block {
                self.comms_block(ui, m, screen);
            }
            // (The calm HUD: the strips step back while nothing happens; the pill stays.)
            let calm = self.calm_level(ctx, m, iface, screen, dt);
            ui.scope(|ui| {
                ui.set_opacity(calm);
                self.header_strip(ui, ctx, iface, screen);
                header_rule(&ui.painter_at(screen), screen, self.reactor.emblem_voice());
            });
            if iface.status_pill {
                self.status_pill(ui, m, screen);
            }
            ui.scope(|ui| {
                ui.set_opacity(calm);
                self.telemetry_strip(ui, m, screen, dt);
            });
            self.screen_card(ui, screen, &frame);
            paint_frost(ctx, screen);
        });
        // His console, the history and the camera in windows of their own (he is the desktop's
        // background) are windows to Sir like any other: "close all windows" closes them.
        let pieces = pieces_in_windows(ctx);
        if PIECES_CLOSE.swap(false, Ordering::AcqRel) && pieces {
            self.console_open = false;
            self.history_open = false;
            if std::mem::take(&mut self.camera_open) {
                crate::camera_feed::stop();
                self.camera_texture = None;
            }
        }
        PIECES_UP.store(if pieces { self.console_open as u8 + self.history_open as u8 + self.camera_open as u8 } else { 0 }, Ordering::Release);
        self.text_console(ctx, m);
        // The search across the blocks, opened with Ctrl+F or by voice ("find voltage on the board").
        if self.find_open || !crate::hud_dock::search().is_empty() {
            self.find_open = true;
            self.find_bar(ctx);
        }
        if self.history_open {
            self.history_overlay(ctx, m);
        }
        if self.camera_open {
            self.camera_block(ctx);
        }
        if self.screens_open {
            self.screens_block(ctx);
        }
    }

    /// The start-up sequence (on the first frames of a start that has one) and the idle screen
    /// (once nothing was touched or said for five minutes), both over the whole home screen.
    fn presence(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings, dt: f32) {
        // A scripted session shows what it is asked to (`JAI_DEMO=boot-long,idle,night`: the
        // sequence at its start, the idle screen from its 20th to its 32nd second, night mode
        // from its 34th to its 46th), so each can be looked at; otherwise it shows none of them.
        let age = hud_age();
        let demo = |what: &str| !windows_still(ctx) && demo(what);
        let still = windows_still(ctx) || crate::platform::in_test_session();
        let home = home_screen(ctx);
        let frame = self.emblem.frame(home);
        if self.frames <= 2 && self.boot.is_none() {
            use crate::startup_sequence::Kind;
            let shown = [("boot-quick", Kind::Quick), ("boot-short", Kind::Short), ("boot-long", Kind::Long)].into_iter().find(|(name, _)| demo(name)).map(|(_, kind)| kind);
            let planned = crate::startup_sequence::current().filter(|_| iface.startup_sequence && !still).map(|plan| plan.kind);
            if let Some(kind) = shown.or(planned) {
                self.boot = Some((Instant::now(), false, kind));
            }
        }
        if let Some((started, sounded, kind)) = self.boot {
            // Any key or click skips it.
            let skipped = ctx.input(|i| i.pointer.any_click() || !i.keys_down.is_empty());
            if !sounded && started.elapsed() >= kind.ignition() {
                self.boot = Some((started, true, kind));
                crate::tts::play_cue(if kind == crate::startup_sequence::Kind::Long { crate::jai_voice_dsp::Earcon::Spotlight } else { crate::jai_voice_dsp::Earcon::Materialize });
            }
            if skipped || !crate::startup_sequence::paint(ctx, home, frame.centre, frame.radius, kind, started.elapsed()) {
                self.boot = None;
                self.boot_done = true;
                self.last_activity = Instant::now();
            }
        }
        // (As the desktop's background he never dims to the idle screen: nobody touches a
        // background, and it would be the idle screen for good.)
        let idle = self.boot.is_none() && !self.as_background() && ((!still && crate::idle_screen::is_idle(iface.idle_screen, self.last_activity.elapsed(), &m.state)) || (demo("idle") && (20.0..32.0).contains(&age)));
        if idle || self.as_background() {
            crate::idle_screen::refresh(&self.app, &self.rt);
        }
        // As the background: the day beside the emblem (his choice of 2026-11-13).
        if self.as_background() && self.boot.is_none() {
            // (Beside the emblem on the monitor it stands on.)
            self.day_panel(ctx, self.emblem.room_at(home, frame.centre), frame.centre, frame.radius);
            // (Not while the core view has the middle: the clock stood on the sphere.)
            if iface.desk_clock && !self.core_open {
                self.desk_clock(ctx, iface, home, &frame);
            }
        }
        let target = if idle { 1.0 } else { 0.0 };
        // (Back from a long rest: what Windows took out of memory meanwhile comes back in one
        // go, not page by page under the first thing Sir opens.)
        #[cfg(windows)]
        if !idle && self.idle >= 1.0 {
            crate::win32::warm_own_code("back from a long rest");
        }
        if self.idle != target {
            // It comes up slowly and is gone at once.
            self.idle = crate::holo_motion::approach(self.idle, target, dt, if idle { 1.2 } else { crate::holo_motion::ARRIVE });
            ctx.request_repaint();
        }
        // (Its ring stands where the emblem is; with the emblem in its corner, in the middle.)
        let (ring_at, ring) = if frame.dock < 0.5 { (frame.centre, frame.radius) } else { (home.center(), home.width().min(home.height()) * 0.17) };
        crate::idle_screen::paint(ctx, home, ring_at, ring, self.idle);
    }

    /// The calm HUD: how much of the header strip, the telemetry strip and the key hints shows.
    /// All of them while he has something to do, a menu or a drawer is open, or the pointer is
    /// near the top or the bottom of the screen; else they step back.
    fn calm_level(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings, screen: Rect, dt: f32) -> f32 {
        let near = ctx.pointer_hover_pos().is_some_and(|at| near_strips(at, screen));
        let open = ctx.memory(|mem| mem.any_popup_open()) || self.drawer_open || self.screens_open || self.history_open || self.find_open;
        let wanted = !iface.calm_hud || windows_still(ctx) || near || open || m.state != "idle";
        let target = if wanted { 1.0 } else { CALM_REST };
        if windows_still(ctx) {
            self.calm = target;
        } else if self.calm != target {
            self.calm = crate::holo_motion::approach(self.calm, target, dt, crate::holo_motion::LEAVE);
            ctx.request_repaint();
        }
        self.calm
    }

    /// The strip flush in the top-right corner, as on the reference board: `[THEME] [HISTORY] [VIEW]
    /// [CLEAR VIEW] [CONSOLE] [CAMERA] CAMERA SOURCE [DEFAULT CAMERA v] [SCREENS]` — thin outlined boxes, the
    /// camera source a wide picker. `[VIEW]` holds the navigation (home, board, 3D core, sim lab,
    /// telemetry, schedule, text console, LLM reboot), the window and the settings; every view
    /// also answers to its name by voice.
    fn header_strip(&mut self, ui: &mut Ui, ctx: &egui::Context, iface: &InterfaceSettings, screen: Rect) {
        use crate::hud_dock::HudViewMode as V;
        let painter = ui.painter_at(screen);
        let devices = crate::camera_feed::devices();
        let source = devices.get(self.camera_device).map(|d| d.to_uppercase()).filter(|_| self.camera_device > 0).unwrap_or_else(|| "DEFAULT CAMERA".into());
        let source = crate::crash_guard::clip(&source, 24).to_string();
        let (reboot_label, rebooting) = crate::llm_reboot::button_label(&crate::llm_reboot::phase());
        let lit = HeaderState {
            phone: pairing_view::is_open(),
            core: self.core_open,
            features: features_view::is_open(),
            // (It glows too while there is a release Sir has not opened the list for.)
            changes: crate::changes::is_open() || crate::changes::unseen(),
            history: self.history_open,
            console: self.console_open,
            rebooting,
            camera: self.camera_open,
            screens: self.screens_open,
            mic_muted: !self.app.config_state().mic_enabled || crate::listening_lock::is_locked(),
            voice: VoiceLock::of(iface.voice_lock, crate::speaker_id::guests_welcome(), crate::hud_feel::voice_known()),
            dictating: crate::kit_typing::dictating(),
            lock_left: crate::listening_lock::remaining(),
        };
        let items = header_items(&lit, reboot_label.trim_start_matches("[ ").trim_end_matches(" ]"), &source);
        let y = screen.top() + 10.0;
        let mut clicked: Option<&str> = None;
        // The pickers' popups hang off their own buttons: egui closes a popup on any click outside
        // its anchor, so a separate anchor closed it in the frame it opened.
        let mut anchors: Vec<(&str, egui::Response)> = Vec::new();
        let theme_id = ui.id().with("hud_theme_menu");
        let view_id = ui.id().with("hud_view_menu");
        let source_id = ui.id().with("hud_source_menu");
        let modules_id = ui.id().with("hud_modules_menu");
        let more_id = ui.id().with("hud_more_menu");
        // A menu chosen from `[ MORE ]` opens a frame later: in the frame of the click egui
        // would close it again (a click outside a popup's anchor closes it).
        if let Some(menu) = self.header_menu_next.take() {
            let popup = match menu {
                "theme" => theme_id,
                "modules" => modules_id,
                "source" => source_id,
                _ => view_id,
            };
            ui.memory_mut(|mem| mem.open_popup(popup));
        }
        let row = header_layout(&items, screen.left() + 12.0, screen.right() - 12.0, y, |t| rail_width(&painter, t));
        self.header_taken = row.placed.iter().map(|(_, r)| *r).chain(row.more).collect();
        let t = holo::theme();
        let lit_by = |response: &egui::Response| ctx.animate_bool_with_time(response.id.with("rail_hot"), response.hovered(), crate::holo_motion::TOUCH);
        // On a screen too narrow for the whole rail: `MORE` holds the words that are not on
        // the row (solid while one of them is).
        if let Some(rect) = row.more {
            let response = ui.interact(rect, ui.id().with(("hud_strip", "more")), Sense::click());
            let lit = row.hidden.iter().any(|id| items.iter().any(|i| i.id == *id && i.on));
            rail_key(&painter, rect, "MORE", lit_by(&response), lit, t.accent());
            if response.clicked() {
                ui.memory_mut(|mem| mem.toggle_popup(more_id));
            }
            egui::popup::popup_below_widget(ui, more_id, &response, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(190.0);
                for id in row.hidden.iter().copied() {
                    let Some(item) = items.iter().find(|i| i.id == id) else { continue };
                    if holo_ui::item(ui, item.text.as_str(), item.on).clicked() {
                        if matches!(id, "theme" | "modules" | "view" | "source") {
                            if id == "source" {
                                crate::camera_feed::refresh_devices();
                            }
                            self.header_menu_next = Some(id);
                        } else {
                            clicked = Some(id);
                        }
                    }
                }
            });
            // The menus of the words it holds hang off it.
            for id in row.hidden.iter().copied().filter(|id| matches!(*id, "theme" | "modules" | "view" | "source")) {
                anchors.push((id, response.clone()));
            }
        }
        paint_seams(&painter, &row);
        for (id, rect) in row.placed {
            let item = items.iter().find(|i| i.id == id).expect("laid-out item");
            if !item.boxed {
                holo::text(&painter, rect.left_center(), Align2::LEFT_CENTER, Face::Label, item.text.as_str(), RAIL_TEXT, t.faint());
                continue;
            }
            if crate::window_drill::enabled() {
                crate::window_drill::mark(&format!("header:{id}"), rect);
            }
            // (A scripted session's stand-in for a click on the desktop aims at this key.)
            if id == "changes" && demo("desk-click") {
                *lock(&DEMO_KEY) = Some(rect);
            }
            let response = ui.interact(rect, ui.id().with(("hud_strip", id)), Sense::click());
            let hot = lit_by(&response);
            if id == "source" {
                rail_picker(&painter, rect, item.text.as_str(), hot);
            } else {
                // A muted microphone reads in the alert colour whatever the theme.
                rail_key(&painter, rect, item.text.as_str(), hot, item.on, if id == "mic" { t.alert() } else { t.accent() });
            }
            if response.clicked() {
                clicked = Some(id);
            }
            // `THEME`: a click cycles the presets, a right-click shows them all as swatches.
            let response = if id == "theme" { response.on_hover_text("Click: next theme · right-click: all themes") } else { response };
            if id == "theme" && response.secondary_clicked() {
                ui.memory_mut(|mem| mem.toggle_popup(theme_id));
            }
            if matches!(id, "theme" | "modules" | "view" | "source") {
                anchors.push((id, response));
            }
        }
        let anchor = |id: &str| anchors.iter().find(|(a, _)| *a == id).map(|(_, r)| r.clone());
        match clicked {
            Some("theme") => self.apply_palette(ctx, iface, crate::holo_theme::next_preset(&iface.hud_palette)),
            Some("phone") => pairing_view::toggle(),
            Some("core") => core_command(crate::holo_mind::CoreCommand::Open(!self.core_open)),
            Some("modules") => ui.memory_mut(|mem| mem.toggle_popup(modules_id)),
            Some("features") => features_view::toggle(),
            Some("changes") => crate::changes::toggle(),
            Some("reboot") if !rebooting => crate::llm_reboot::reboot_from_button(&self.rt),
            Some("history") => {
                self.history_open = !self.history_open;
                if self.history_open {
                    let db = self.app.config().read(|c| c.memory_db_path.clone());
                    self.history_reports = crate::hud_board::list_reports(&db, 12);
                }
            }
            Some("view") => ui.memory_mut(|mem| mem.toggle_popup(view_id)),
            Some("clear") => {
                // Silent: the blocks collapse and the emblem folds home (no speech, no flush).
                crate::hud_board::sign_off();
                crate::tts::play_earcon(crate::jai_voice_dsp::Earcon::PowerDown);
            }
            Some("console") => {
                self.console_open = !self.console_open;
                self.console_focus = self.console_open;
            }
            Some("camera") => {
                self.camera_open = !self.camera_open;
                if self.camera_open {
                    crate::camera_feed::start(self.camera_device);
                } else {
                    crate::camera_feed::stop();
                    self.camera_texture = None;
                }
            }
            Some("source") => {
                crate::camera_feed::refresh_devices();
                ui.memory_mut(|mem| mem.toggle_popup(source_id));
            }
            Some("screens") => self.screens_open = !self.screens_open,
            Some("mic") if lit.dictating && !lit.mic_muted => {
                // Dictating: one click ends it (the microphone stays as it is).
                let _ = crate::kit_typing::dictation(&serde_json::json!({ "action": "stop" }));
            }
            Some("mic") => {
                // Muted (or "stop listening"): one click and he listens again.
                let unmute = lit.mic_muted;
                if crate::listening_lock::is_locked() {
                    crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
                }
                self.command(HudCommand::SetMic { enabled: unmute });
            }
            Some("voice") => {
                // Guests in: one click and he answers to Sir alone again. Else the lock itself.
                let args = if crate::speaker_id::guests_welcome() { serde_json::json!({ "voice_guest": false }) } else { serde_json::json!({ "voice_lock": !iface.voice_lock }) };
                let app = self.app.clone();
                self.rt.spawn(async move {
                    let _ = app.tools().execute("configure_interface", &args).await;
                });
            }
            _ => {}
        }
        // "Stop listening": the lock and how to lift it, under the strip.
        if let Some(status) = crate::listening_lock::status_line() {
            painter.text(pos2(screen.right() - 12.0, y + 30.0), Align2::RIGHT_TOP, status, strip_font(), crate::holo::theme().alert());
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        // `THEME` right-click: the presets and Sir's own themes as swatches, then the studio.
        if let Some(button) = anchor("theme") {
            let mut picked: Option<&'static str> = None;
            let mut load: Option<String> = None;
            let mut create = false;
            egui::popup::popup_below_widget(ui, theme_id, &button, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(230.0);
                holo_ui::head(ui, "THEME");
                for (name, _, look) in crate::holo_theme::PRESETS.iter() {
                    if theme_row(ui, look, &crate::holo_theme::label(name), iface.hud_palette == *name).clicked() {
                        picked = Some(name);
                    }
                }
                let own = crate::theme_store::custom_themes();
                if !own.is_empty() {
                    holo_ui::head(ui, "YOURS");
                }
                for theme in own {
                    if theme_row(ui, &theme.theme(), &theme.name, iface.hud_palette == theme.key()).clicked() {
                        load = Some(theme.name.clone());
                    }
                }
                holo_ui::rule(ui);
                if holo_ui::item(ui, "+ Create a theme", false).clicked() {
                    create = true;
                }
            });
            if let Some(name) = picked {
                self.apply_palette(ctx, iface, name);
            }
            if let Some(name) = load {
                self.configure_look(iface, serde_json::json!({ "load_theme": name }));
            }
            if create {
                theme_studio::open();
            }
        }
        // `MODULES`: one click to each Stark module (the tool runs directly, no language model).
        if let Some(button) = anchor("modules") {
            let mut run: Option<&[(&'static str, &'static str)]> = None;
            egui::popup::popup_below_widget(ui, modules_id, &button, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(330.0);
                holo_ui::head(ui, "MODULES");
                for (label, calls) in MODULES {
                    if holo_ui::item(ui, *label, false).clicked() {
                        run = Some(calls);
                    }
                }
                if holo_ui::item(ui, "DISCORD // AUDIO ROUTING — VOICE INTO THE CALL", false).clicked() {
                    discord_view::open();
                }
            });
            if let Some(calls) = run {
                self.run_tools(calls);
            }
        }
        // `CAMERA SOURCE`: the capture devices.
        if let Some(source_button) = anchor("source") {
            egui::popup::popup_below_widget(ui, source_id, &source_button, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(source_button.rect.width().max(200.0));
                holo_ui::head(ui, "CAMERA SOURCE");
                if devices.is_empty() {
                    ui.label(RichText::new("No capture devices found.").color(strip_muted()));
                }
                for (i, name) in devices.iter().enumerate() {
                    let label = if i == 0 { format!("Default — {name}") } else { name.clone() };
                    if holo_ui::item(ui, label, self.camera_device == i).clicked() {
                        self.camera_device = i;
                        if crate::camera_feed::is_running() {
                            crate::camera_feed::stop();
                            let device = i;
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_millis(400));
                                crate::camera_feed::start(device);
                            });
                        }
                    }
                }
            });
        }
        // `VIEW`: the navigation, the window and the settings.
        let Some(view_button) = anchor("view") else { return };
        let windows = self.app.tools().widgets.windows();
        let sims = !self.app.tools().stark.simlab.list().is_empty();
        let open = |id: &str| windows.iter().any(|w| w.id == id);
        let board = crate::hud_board::is_active();
        let home = !board && !self.core_open && !self.console_open && !sims && windows.is_empty();
        let nav: [(V, bool); 7] = [
            (V::Home, home),
            (V::Board, board),
            (V::CoreArchitectureSphere, self.core_open),
            (V::SimLab, sims),
            (V::Telemetry, open("system_telemetry")),
            (V::Schedule, open("schedule")),
            (V::TextConsole, self.console_open),
        ];
        let mut chosen: Option<V> = None;
        let wake_word = self.app.config_state().wake_word_required;
        let dormant = crate::vram_guard::is_dormant();
        egui::popup::popup_below_widget(ui, view_id, &view_button, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(270.0);
            let item = |ui: &mut Ui, text: &str| holo_ui::item(ui, text, false).clicked();
            holo_ui::head(ui, "NAVIGATE");
            for (mode, on) in nav {
                if holo_ui::item(ui, mode.label(), on).clicked() {
                    chosen = Some(mode);
                }
            }
            holo_ui::head(ui, "ON THE CANVAS");
            // (Each a switch: what it is now shows on it.)
            let mut next = iface.clone();
            for (label, on) in [("Comms block", &mut next.comms_block), ("Status plate", &mut next.status_pill), ("Voice ring", &mut next.voice_rim), ("Sound cues", &mut next.sound_cues)] {
                if holo_ui::item_toggle(ui, label, *on).clicked() {
                    *on = !*on;
                }
            }
            if item(ui, "Tidy the windows") {
                crate::hud_dock::retile_all(block_bounds(screen));
            }
            if item(ui, "Find in blocks (Ctrl+F)") {
                self.find_open = true;
                self.find_focus = true;
            }
            let ghosts = crate::workspace_canvas::ghost_count();
            if ghosts > 0 && item(ui, &format!("Un-ghost all ({ghosts})")) {
                crate::workspace_canvas::unghost_all();
            }
            holo_ui::head(ui, "HE HEARS AND THINKS");
            if holo_ui::item_toggle(ui, "Wake word needed", wake_word).clicked() {
                self.command(HudCommand::SetWakeWordRequired { required: !wake_word });
            }
            if item(ui, if dormant { "Graphics memory: standby — wake him" } else { "Graphics memory: clear it now" }) {
                if dormant {
                    crate::vram_guard::wake("HUD view menu");
                } else {
                    crate::vram_guard::flush_with_reason("cleared from the HUD");
                }
            }
            holo_ui::head(ui, "WINDOW");
            if item(ui, "The core alone") {
                next.orb_mode = true;
            }
            if next != *iface {
                self.set_interface(next);
            }
            holo_ui::head(ui, "SETTINGS");
            if item(ui, "All settings — search") {
                settings_panel::open(!settings_panel::is_open());
            }
            if item(ui, "Voice, devices, memory") {
                self.drawer_open = !self.drawer_open;
            }
            if item(ui, "Integrations hub") {
                self.hub.open = !self.hub.open;
            }
            holo_ui::rule(ui);
            if item(ui, "Shut down J.A.I.") {
                self.confirm_shutdown = true;
            }
        });
        if let Some(mode) = chosen {
            self.switch_view(mode, true);
        }
    }

    /// Switches the HUD to the preset `name`: board, blocks, emblem and core cross-fade from the
    /// next frame on (`theme_store::follow`), the settings windows take its accent, and a toast
    /// names it.
    /// A `configure_interface` change straight from a HUD control, with its outcome as a toast.
    fn configure_look(&self, iface: &InterfaceSettings, args: serde_json::Value) {
        match crate::interface::configure(iface, &args) {
            Ok(done) => {
                self.app.notice(format!("THEME // {}", done.summary.to_uppercase()));
                self.set_interface(done.settings);
            }
            Err(e) => self.app.notice(format!("THEME // {e}")),
        }
    }

    fn apply_palette(&mut self, ctx: &egui::Context, iface: &InterfaceSettings, name: &str) {
        let mut next = iface.clone();
        next.hud_palette = name.to_string();
        self.set_interface(next);
        self.app.notice(format!("THEME // {} ACTIVE", crate::holo_theme::label(name)));
        ctx.request_repaint();
    }

    /// Runs tools straight from a HUD control, in order, without the language model; each
    /// result's first line arrives as a toast.
    fn run_tools(&self, calls: &[(&'static str, &'static str)]) {
        self.run_calls(calls.iter().map(|(name, args)| (*name, serde_json::from_str(args).unwrap_or_default())).collect());
    }

    fn run_calls(&self, calls: Vec<(&'static str, serde_json::Value)>) {
        let (tools, app) = (self.app.tools().clone(), self.app.clone());
        self.rt.spawn(async move {
            for (name, args) in calls {
                let o = tools.execute(name, &args).await;
                let line = o.output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Done.");
                app.notice(format!("{} // {}", name.replace('_', " ").to_uppercase(), crate::crash_guard::clip(line, 140)));
            }
        });
    }

    /// Bottom left: the microphone as a row of cells with its peak held as a tick, `LVL` and
    /// `PK` in dB, the `LIVE` / `MUTE` / `SLEEP` chip (click: microphone on / off; its tooltip
    /// carries the VRAM guard, the listening mode and the state) and the last thing Sir said,
    /// inline. A silent command's badge or a signal shows after it; the key hints stand right.
    fn telemetry_strip(&mut self, ui: &mut Ui, m: &HudModel, screen: Rect, dt: f32) {
        let painter = ui.painter_at(screen);
        let t = holo::theme();
        let y = screen.bottom() - 10.0 - TELEMETRY_H;
        let mid = y + TELEMETRY_H / 2.0;
        let level = m.level.as_ref();
        let muted = level.is_none_or(|l| l.phase == "muted") || m.cfg.as_ref().is_some_and(|c| !c.mic_enabled);
        let lvl = level.map(|l| l.rms_db).unwrap_or(-90.0).max(-90.0);
        self.peak_db = if lvl > self.peak_db { lvl } else { (self.peak_db - 9.0 * dt).max(lvl) };
        let sleeping = m.cfg.as_ref().is_some_and(|c| c.sleeping);
        let rect = paint_strip_meter(&painter, pos2(screen.left() + 14.0, y), lvl, self.peak_db, muted, sleeping);
        let (vram, _, _) = crate::vram_guard::badge();
        let wake = m.cfg.as_ref().is_none_or(|c| c.wake_word_required);
        let ptt = m.cfg.as_ref().is_some_and(|c| c.ptt_active);
        let mode = if ptt { "PTT" } else if wake { "WAKE WORD" } else { "HANDS-FREE" };
        let state = if m.state.is_empty() { "IDLE".to_string() } else { state_name(&m.state).to_uppercase() };
        let engine = m.cfg.as_ref().map(|c| format!("\n{} · {} · STT {}", c.hardware_profile, c.llm, c.stt_model)).unwrap_or_default();
        let hover = format!("Click: microphone on / off (Ctrl+M)\n{vram}\nMODE: {mode} · STATE: {state}{engine}\nVIEW holds the mode switch and the GPU clear.");
        if ui.interact(rect, ui.id().with("hud_live"), Sense::click()).on_hover_text(hover).clicked() {
            let enabled = !self.app.config_state().mic_enabled;
            self.command(HudCommand::SetMic { enabled });
        }
        let mut x = rect.right() + 14.0;
        // `CALL · LISTENING` / `CALL · TALKING` while JAI is in Sir's Discord call.
        if let Some(call) = crate::discord_bridge::status_label() {
            let rect = holo_kit::chip(&painter, pos2(x, y), call.trim_start_matches('[').trim_end_matches(']'), t.accent(), false, 1.0);
            ui.interact(rect, ui.id().with("hud_call"), Sense::hover()).on_hover_text("JAI hears the others in your Discord call (not your microphone). Say \"stop listening to the call\" to end it.");
            x = rect.right() + 14.0;
        }
        // `F1 MUTE · ESC INTERRUPT` on the right (each also a button), left of
        // the docked core's corner; only where it fits.
        let hints = key_hints(&painter, key_hints_right(screen, self.console_reserve()), mid);
        let hints_left = hints.first().map_or(f32::INFINITY, |(_, r)| r.left());
        let room_right = if hints_left > x + 160.0 { hints_left - 24.0 } else { screen.right() - crate::hud_dock::DOCK_INSET * 2.0 - 40.0 };
        let line = |x: &mut f32, face: Face, size: f32, s: &str, colour: Color32| {
            *x = holo::text(&painter, pos2(*x, mid), Align2::LEFT_CENTER, face, s, size, colour).right() + 14.0;
        };
        // What he said last, inline (fading out with the caption timer).
        if let Some(said) = m.messages.iter().rev().find(|l| l.who == Speaker::Sir && !l.faded) {
            let heard = heard_line(&said.text, ((room_right - x).max(0.0) / 6.4) as usize);
            if !heard.is_empty() {
                line(&mut x, Face::Body, 12.5, &heard, holo::a(t.ink(), 0.75));
            }
        }
        if let Some((path, alert, until)) = &m.signal {
            if *until > Instant::now() {
                line(&mut x, Face::Label, RAIL_TEXT, &format!("SIGNAL: {path}"), if *alert { t.alert() } else { t.accent() });
            }
        }
        if let Some((label, ok, until)) = &m.quick {
            if *until > Instant::now() {
                line(&mut x, Face::Label, RAIL_TEXT, label, if *ok { t.accent() } else { t.alert() });
            }
        }
        // The key hints, where the line left them room.
        if x + 10.0 > hints_left {
            return;
        }
        for (n, (label, rect)) in hints.iter().enumerate() {
            let response = ui.interact(rect.expand(3.0), ui.id().with(("hud_key_hint", n)), Sense::click());
            paint_key_hint(&painter, *rect, label, response.hovered());
            let response = response.on_hover_text(["Microphone on / off (F1, Ctrl+M)", "Stop JAI speaking (Esc)"][n]);
            if response.clicked() {
                match n {
                    0 => self.toggle_mic(),
                    _ => self.command(HudCommand::Interrupt),
                }
            }
        }
    }

    /// The state plate, top centre: `LISTENING` in plain ink, the waits (`THINKING`, …) in the
    /// accent with a bar of light running its foot, `SPEAKING` a solid block, `MUTED` in the alert
    /// colour, `STANDBY` stepped back. A new word cross-fades in; where the top rail reaches the
    /// centre it stands left of it.
    fn status_pill(&mut self, ui: &mut Ui, m: &HudModel, screen: Rect) {
        let muted = m.level.as_ref().is_none_or(|l| l.phase == "muted") || m.cfg.as_ref().is_some_and(|c| !c.mic_enabled) || crate::listening_lock::is_locked();
        let asleep = m.cfg.as_ref().is_some_and(|c| c.sleeping) || crate::vram_guard::is_dormant();
        let word = pill_word(&m.state, muted, asleep);
        if word != self.pill.0 {
            self.pill = (word, self.pill.0, Instant::now());
        }
        let t = (self.pill.2.elapsed().as_secs_f32() / PILL_FADE).min(1.0);
        if t < 1.0 {
            ui.ctx().request_repaint();
        }
        let painter = ui.painter_at(screen);
        let Some(rect) = pill_rect(&painter, screen, &self.header_taken) else { return };
        if t < 1.0 {
            paint_pill(&painter, rect.center(), self.pill.1, 1.0 - t);
        }
        paint_pill(&painter, rect.center(), word, t);
        ui.interact(rect, ui.id().with("hud_status_pill"), Sense::hover()).on_hover_text(pill_tip(word));
    }

    /// The card under the emblem after JAI looked at the screen (`screen_context`), as in the
    /// reference video: `● VIDEO DETECTED`, what it was, and its quick actions (a click asks
    /// him); it leaves by itself after a few seconds (the pointer on it keeps it).
    fn screen_card(&mut self, ui: &mut Ui, screen: Rect, frame: &crate::holo_core::CoreFrame) {
        let rect = screen_card_rect(screen, frame);
        // (The pointer on it holds it, fully shown.)
        let Some((card, age)) = crate::screen_context::card(ui.rect_contains_pointer(rect)) else { return };
        ui.ctx().request_repaint_after(Duration::from_millis(40));
        let alpha = crate::screen_context::card_alpha(age);
        let painter = ui.painter_at(screen);
        let t = holo::theme();
        crate::holo_frame::card(&painter, rect, 0.0, alpha);
        let left = rect.left() + 12.0;
        holo_kit::chip(&painter, pos2(left, rect.top() + 8.0), &format!("{} DETECTED", card.kind), t.accent(), true, alpha);
        // (A long window title is shortened to the card's width.)
        let line = holo::galley_within(&painter, Face::Body, &card.line, 13.0, holo::a(t.ink(), alpha), rect.width() - 24.0);
        painter.galley(pos2(left, rect.top() + 26.0), line, t.ink());
        let mut x = left;
        for (n, (word, ask)) in card.actions.iter().enumerate() {
            let key = Rect::from_min_size(pos2(x, rect.bottom() - 6.0 - holo_kit::KEY_H), vec2(holo_kit::key_width(&painter, word), holo_kit::KEY_H));
            let response = ui.interact(key, ui.id().with(("hud_screen_card", n)), Sense::click()).on_hover_text(*ask);
            holo_kit::key(&painter, key, word, response.hovered() as u8 as f32, response.is_pointer_button_down_on(), false, alpha);
            x = key.right() + 6.0;
            if response.clicked() {
                crate::screen_context::dismiss();
                self.command(HudCommand::Chat { text: (*ask).to_string() });
            }
        }
    }

    /// While Sir speaks, his voice shows as a ring of radial ticks round the emblem, as on the
    /// reference orb (only at home, not docked; the emblem's own rings follow JAI's voice).
    fn voice_rim(&mut self, painter: &egui::Painter, m: &HudModel, frame: &crate::holo_core::CoreFrame, dt: f32) {
        let phase = m.level.as_ref().map_or("muted", |l| l.phase);
        // How well he hears Sir: from half a second into an utterance, by the loudest his voice
        // was and the room's own noise. The ring turns amber and says what is wrong.
        match m.level.as_ref().filter(|_| matches!(phase, "pre_speech" | "speaking" | "hangover") && m.state != "speaking") {
            Some(l) => {
                let (since, peak) = self.mic_speech.map_or((Instant::now(), l.rms_db), |(since, peak)| (since, peak.max(l.rms_db)));
                self.mic_speech = Some((since, peak));
                if since.elapsed() > Duration::from_millis(600) {
                    match crate::hud_lines::mic_verdict(peak, to_db(l.noise_floor)) {
                        Some(verdict) => self.mic_verdict = Some((verdict, Instant::now())),
                        // (His voice came up: the label goes with it.)
                        None => self.mic_verdict = None,
                    }
                }
            }
            None => self.mic_speech = None,
        }
        const VERDICT_STAYS: f32 = 3.0;
        let verdict = self.mic_verdict.filter(|(_, at)| at.elapsed().as_secs_f32() < VERDICT_STAYS);
        if let Some((word, at)) = verdict.filter(|_| frame.dock < 0.5) {
            let strength = (1.0 - (at.elapsed().as_secs_f32() - (VERDICT_STAYS - 0.4)).max(0.0) / 0.4).clamp(0.0, 1.0);
            crate::hud_lines::paint_mic_label(painter, frame.centre, frame.radius, word, strength);
            painter.ctx().request_repaint_after(Duration::from_millis(50));
        }
        let target = rim_target(&m.state, phase, self.reactor.mic, frame.dock);
        let rate: f32 = if target > self.voice_rim { 0.55 } else { 0.9 };
        self.voice_rim += (target - self.voice_rim) * (1.0 - rate.powf(dt * 60.0));
        // (The core draws the ticks themselves: `step_emblem` hands it this level.)
        if self.voice_rim < 0.01 {
            self.voice_rim = 0.0;
            return;
        }
        painter.ctx().request_repaint();
    }

    /// `COMMS // LIVE RESPONSE`: the latest exchange as a sharp block in the band above the
    /// telemetry strip, so every reply is seen as well as heard (a text-only reply too). It fades
    /// `CAPTION_SECS` after the last word; the text console and HISTORY keep everything.
    fn comms_block(&mut self, ui: &mut Ui, m: &HudModel, screen: Rect) {
        let sir = m.messages.iter().rev().find(|l| l.who == Speaker::Sir && !l.faded).map(|l| l.text.clone()).unwrap_or_default();
        let jai = m.messages.iter().rev().find(|l| l.who == Speaker::Jai);
        let (reply, streaming, at) = jai.map(|l| (l.text.clone(), l.streaming, l.at.clone())).unwrap_or_default();
        let now = Instant::now();
        for (slot, text) in self.captions.iter_mut().zip([&sir, &reply]) {
            if slot.0 != *text {
                *slot = (text.clone(), now);
            }
        }
        let fresh = |k: usize| streaming && k == 1 || now.duration_since(self.captions[k].1).as_secs_f32() <= CAPTION_SECS;
        let heard = if fresh(0) { self.captions[0].0.as_str() } else { "" };
        let answer = if fresh(1) { self.captions[1].0.as_str() } else { "" };
        let Some(block) = crate::hud_board::dialogue_block(heard, answer, streaming, &at) else { return };
        let newest = now.duration_since(self.captions[0].1.max(self.captions[1].1)).as_secs_f32();
        let fade = if streaming { 1.0 } else { (1.0 - (newest - CAPTION_SECS + 2.0).max(0.0) / 2.0).clamp(0.0, 1.0) };
        let (width, centre_x) = comms_placement(screen, self.console_reserve());
        let bottom = screen.bottom() - 12.0 - TELEMETRY_H - 6.0;
        let rect = Rect::from_min_size(pos2(centre_x - width / 2.0, bottom - (CAPTION_BAND - 8.0)), vec2(width, CAPTION_BAND - 8.0)).translate(self.comms_offset);
        // (The pass is read before the write: egui's lock is not re-entrant, see `no_context_call_waits_inside_the_contexts_own_lock`.)
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("jai_tour_comms"), (pass, rect)));
        // Like every block, it moves with the mouse.
        let drag = ui.interact(rect, egui::Id::new("hud_comms_drag"), Sense::drag());
        if drag.dragged() {
            self.comms_offset += drag.drag_delta();
        }
        let mut painter = ui.painter_at(screen);
        painter.set_opacity(fade);
        // A click on the heard words opens the hearing log, that line ready to be put right.
        if let Some(row) = crate::hud_board::paint_dialogue(&painter, rect, &block) {
            let heard = ui.interact(row, egui::Id::new("hud_comms_heard"), Sense::click()).on_hover_text("Misheard? Click to tell me what you meant");
            if heard.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if heard.clicked() {
                crate::kit_voice::correct_last();
            }
        }
    }

    /// Ctrl+F: one line that searches every open block (the board's tiles and the in-app
    /// windows). What carries the words stays lit, the rest steps back; Esc or `[ X ]` ends it.
    fn find_bar(&mut self, ctx: &egui::Context) {
        // Changed by voice since the last frame ("find voltage", "clear the search").
        let live = crate::hud_dock::search();
        if live != self.find_text.trim().to_lowercase() {
            if live.is_empty() {
                self.find_open = false;
                self.find_text.clear();
                return;
            }
            self.find_text = live;
        }
        let screen = home_screen(ctx);
        let mut close = false;
        egui::Area::new(egui::Id::new("hud_find_bar")).order(egui::Order::Foreground).fixed_pos(pos2(screen.center().x - FIND_BAR_WIDTH / 2.0, screen.top() + 12.0 + HEADER_H + 10.0)).show(ctx, |ui| {
            Frame::none().fill(crate::holo::theme().glass()).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO).inner_margin(Margin::symmetric(10.0, 6.0)).show(ui, |ui| {
                ui.set_width(FIND_BAR_WIDTH - 20.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("FIND IN BLOCKS").font(strip_font()).color(crate::hud_board::active_colour()));
                    let input = crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.find_text).desired_width(220.0).font(FontId::proportional(14.0)).hint_text("voltage, deadline …"));
                    if self.find_focus {
                        input.request_focus();
                        self.find_focus = false;
                    }
                    if input.changed() {
                        crate::hud_dock::set_search(&self.find_text);
                    }
                    let needle = crate::hud_dock::search();
                    let hits = crate::hud_dock::search_hits().len() + crate::hud_board::search_hits(&needle).len();
                    let tally = if needle.is_empty() { "TYPE TO SEARCH".to_string() } else { format!("{hits} BLOCK{}", if hits == 1 { "" } else { "S" }) };
                    ui.label(RichText::new(tally).font(strip_font()).color(strip_muted()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close = holo_ui::key(ui, "X").on_hover_text("End the search (Esc)").clicked();
                    });
                });
            });
        });
        if close {
            self.find_open = false;
            self.find_text.clear();
            crate::hud_dock::set_search("");
        }
    }

    /// What the console keeps free on the right: its width while it sits against the right edge,
    /// nothing once Sir has dragged it elsewhere (or detached it).
    fn console_reserve(&self) -> f32 {
        if self.console_at_edge { CONSOLE_WIDTH * self.console_slide } else { 0.0 }
    }

    /// The text console: a free window like every block (drag its title row, resize its edges)
    /// that opens against the right edge when that is free, else where the window packer finds
    /// room; `[ PIN ]` keeps it in front, `[ DETACH ]` makes it its own borderless OS window.
    /// The transcript reads as a terminal log (`[USER // 14:02:11]`), with the `[ SPEAK REPLIES |
    /// SILENT TEXT MODE ]` toggle and an input line (Enter or `[ TRANSMIT -> ]`).
    fn text_console(&mut self, ctx: &egui::Context, m: &HudModel) {
        if let request @ (1 | 2) = CONSOLE_REQUEST.swap(0, Ordering::AcqRel) {
            self.console_open = request == 1;
            self.console_focus = self.console_open;
        }
        if let Some(text) = lock(&CONSOLE_PREFILL).take() {
            self.console_input = text;
        }
        let slide = self.console_slide;
        if slide <= 0.0 {
            crate::hud_dock::forget(CONSOLE_ID);
            return;
        }
        let speaking = m.cfg.as_ref().is_some_and(|c| c.response_mode == VoiceResponseMode::AlwaysSpeak);
        let mut act = ConsoleActions::default();
        if crate::hud_dock::all_own() {
            // (Closed: it plays its exit, then goes.)
            if !detached_keep(ctx, ViewportId::from_hash_of("jai_text_console"), self.console_open) {
                return;
            }
            self.console_at_edge = false;
            let builder = egui::ViewportBuilder::default().with_title("Text Console — J.A.I.").with_app_id("jai").with_inner_size([CONSOLE_WIDTH, 640.0]).with_min_inner_size([320.0, 260.0]).with_decorations(false);
            let builder = for_test_session(builder);
            // Asked for to type in (his key, a click on the corner orb): once its window is
            // up it takes the keyboard. (His windows come up without it; this one is the
            // exception, and only when Sir himself called it. Never in a scripted session.)
            if self.console_focus {
                self.console_wants_keys = !crate::platform::in_test_session();
            }
            let console = ViewportId::from_hash_of("jai_text_console");
            if self.console_wants_keys && lock(&DETACHED_MOTIONS).revealed.contains(&console) {
                self.console_wants_keys = false;
                self.console_focus = true;
                ctx.send_viewport_cmd_to(console, ViewportCommand::Focus);
            }
            ctx.show_viewport_immediate(ViewportId::from_hash_of("jai_text_console"), builder, |ctx, _| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    act.close = true;
                }
                egui::CentralPanel::default().frame(Frame::none().fill(block_slate()).inner_margin(Margin::same(12.0))).show(ctx, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.console_body(ui, ctx, m, speaking, true, &mut act);
                });
                paint_window_border(ctx);
            });
        } else {
            let screen = home_screen(ctx);
            let top = screen.top() + 12.0 + HEADER_H + 10.0;
            let height = (screen.bottom() - 12.0 - TELEMETRY_H - 10.0 - top).max(220.0);
            let bounds = Rect::from_min_max(pos2(screen.left() + 8.0, top), pos2(screen.right() - 8.0, screen.bottom() - 12.0 - TELEMETRY_H - 4.0));
            let area = egui::Id::new("hud_text_console");
            let edge = Rect::from_min_size(pos2(screen.right() - 20.0 - CONSOLE_WIDTH, top), vec2(CONSOLE_WIDTH, height));
            // First open: against the right edge, unless a window already sits there.
            let at = if ctx.memory(|mem| mem.area_rect(area)).is_none() && crate::hud_dock::block_rects().iter().any(|r| r.intersects(edge)) {
                crate::hud_dock::place_new_window(bounds, &monitor_regions(ctx), edge.size(), None, edge.min)
            } else {
                edge.min
            };
            // (Its border is drawn inside it below: whole at any DPI, also flush with an edge.)
            let frame = Frame::none().fill(crate::holo::theme().glass()).rounding(Rounding::ZERO).inner_margin(Margin::same(12.0));
            let shown = egui::Window::new("TEXT // CONSOLE")
                .id(area)
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .frame(frame)
                .default_pos(at)
                .default_size(edge.size() - vec2(26.0, 26.0))
                .min_size([300.0, 220.0])
                // Never larger than its part of the screen: content that asks for more for a
                // frame (the completions appearing) must not make the console grow past the
                // telemetry strip for good, where its input line would be cut off.
                .max_size(bounds.size() - vec2(26.0, 26.0))
                .constrain_to(bounds)
                .order(if self.console_pinned { egui::Order::Foreground } else { egui::Order::Middle })
                .show(ctx, |ui| {
                    ui.set_opacity(slide);
                    ui.set_min_size(ui.available_size());
                    self.console_body(ui, ctx, m, speaking, false, &mut act);
                });
            if let Some(inner) = shown {
                let rect = inner.response.rect;
                self.console_at_edge = rect.right() >= screen.right() - 40.0;
                let painter = ctx.layer_painter(inner.response.layer_id);
                crate::holo::outline(&painter, &crate::holo::plate(rect, [0.0; 4]), crate::hud_board::border_colour().gamma_multiply(slide));
                crate::hud_board::brackets(&painter, rect.shrink(0.75), crate::hud_board::bracket_colour().gamma_multiply(slide));
                crate::hud_dock::publish_block(CONSOLE_ID, "Text console", rect);
                publish_console_rect(Some(rect));
            }
        }
        if act.close {
            self.console_open = false;
        }
        if act.pin {
            self.console_pinned = !self.console_pinned;
        }
        if let Some(mode) = act.mode {
            self.command(HudCommand::SetResponseMode { mode });
        }
        if let Some(text) = act.send {
            self.console_input.clear();
            self.command(HudCommand::Chat { text });
        }
    }

    /// The console's content, in the HUD or in its own window (`detached`: the title row moves
    /// the window).
    fn console_body(&mut self, ui: &mut Ui, ctx: &egui::Context, m: &HudModel, speaking: bool, detached: bool, act: &mut ConsoleActions) {
        // In its window it wears the bar every window of his wears; on the board a row of its own.
        if detached {
            act.close |= detached_bar(ui, ctx, CONSOLE_ID, "Text // console");
        } else {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new("TEXT // CONSOLE").font(strip_font()).color(crate::hud_board::active_colour())).sense(Sense::drag()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let pinned = self.console_pinned;
                    act.close |= holo_ui::key_on(ui, "CLOSE", false).on_hover_text("Close (Esc)").clicked();
                    act.pin |= holo_ui::key_on(ui, "PIN", pinned).on_hover_text("Keep it in front (Esc leaves it open)").clicked();
                });
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let segment = |ui: &mut Ui, text: &str, on: bool| holo_ui::key_on(ui, text, on).clicked();
            if segment(ui, "SPEAK REPLIES", speaking) && !speaking {
                act.mode = Some(VoiceResponseMode::AlwaysSpeak);
            }
            if segment(ui, "SILENT TEXT MODE", !speaking) && speaking {
                act.mode = Some(VoiceResponseMode::MirrorInput);
            }
        });
        ui.add_space(6.0);
        // The reference's file box: the file "this file" means right now (one dropped anywhere
        // on the HUD, or the last one he found or edited; none once the conversation has let
        // it go).
        match self.app.tools().focus.last_file() {
            Some(path) => {
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                ui.label(RichText::new(format!("FILE // {}", crate::crash_guard::clip(&name, 48))).font(strip_font()).color(crate::hud_board::active_colour())).on_hover_text(path.display().to_string());
            }
            None => {
                ui.label(RichText::new("+ DROP A FILE ON THE HUD · NO FILE LOADED").font(strip_font()).color(strip_muted()));
            }
        }
        ui.add_space(8.0);
        let log_height = (ui.available_height() - self.console_foot).max(60.0);
        crate::holo_block::faded(ui, egui::ScrollArea::vertical().max_height(log_height).auto_shrink([false, false]).stick_to_bottom(true), |ui| {
            for line in m.messages.iter().filter(|l| !l.text.trim().is_empty()) {
                let (who, colour) = match line.who {
                    Speaker::Sir => ("USER", strip_muted()),
                    Speaker::Jai => ("J.A.I.", crate::hud_board::active_colour()),
                    Speaker::System => ("SYSTEM", strip_muted()),
                };
                ui.label(RichText::new(format!("[{who} // {}]", line.at)).font(strip_font()).color(colour));
                let ink = if line.faded { strip_muted() } else { strip_ink() };
                let text = if line.streaming { format!("{} ▌", line.text.trim()) } else { line.text.trim().to_string() };
                ui.add(egui::Label::new(RichText::new(text).size(13.5).color(ink)).wrap());
                ui.add_space(6.0);
            }
        });
        let foot_top = ui.cursor().top();
        ui.add_space(6.0);
        // Predictive completion: what Sir most likely means by what he has typed so far (his
        // own recent requests first, then JAI's command vocabulary). Tab takes the first.
        if self.completions.0 != self.console_input {
            let recent: Vec<String> = m.messages.iter().rev().filter(|l| l.who == Speaker::Sir).take(40).map(|l| l.text.clone()).collect();
            self.completions = (self.console_input.clone(), crate::command_palette::complete(&self.console_input, &recent, 4));
        }
        if let Some(text) = console_foot(ui, &mut self.console_input, &self.completions.1, &mut self.console_focus) {
            act.send = Some(text);
        }
        // What the foot took this frame is what the log leaves it in the next one. A foot that
        // grew (the completions appeared) overflowed the console in this pass: it is laid out
        // again at once. One that shrank only left a gap for a frame.
        let foot = ui.cursor().top() - foot_top;
        if (foot - self.console_foot).abs() > 0.5 {
            if foot > self.console_foot {
                ctx.request_discard("the console's foot grew");
            } else {
                ctx.request_repaint();
            }
            self.console_foot = foot;
        }
    }

    /// The left column of system blocks: confirmations, message cards, the latest clip and
    /// JAI's live widgets. Returns whether anything is shown.
    fn system_column(&mut self, ctx: &egui::Context, m: &HudModel, screen: Rect) -> bool {
        let power: Vec<PowerCardView> = m.messages.iter().filter_map(|l| l.power.clone()).filter(|c| c.status == "pending").collect();
        let cards: Vec<MessageCardView> = m.messages.iter().filter_map(|l| l.card.clone()).filter(|c| c.status == "pending").collect();
        let clip = m.messages.iter().rev().take(6).find_map(|l| l.clip.clone());
        let widgets: Vec<crate::widgets::DynamicWidgetSpec> = self.app.tools().widgets.widgets().into_iter().filter(|w| w.dock == crate::widgets::Dock::Panel).collect();
        if power.is_empty() && cards.is_empty() && clip.is_none() && widgets.is_empty() {
            self.system_column_rect = None;
            return false;
        }
        let p = self.palette;
        let top = screen.top() + 12.0 + HEADER_H + 14.0;
        let room = screen.height() - top - 90.0;
        let blocks = |this: &mut Self, ui: &mut Ui| {
            ui.set_width(SYSTEM_COLUMN);
            column_scroll(ui, room, |ui| {
                for card in &power {
                    crate::holo_ui::titled_plate(ui, "SYSTEM POWER // CONFIRMATION", "PENDING", |ui| this.power_card(ui, &p, card));
                }
                for card in &cards {
                    crate::holo_ui::titled_plate(ui, "WHATSAPP // MESSAGE CARD", "PENDING", |ui| this.message_card(ui, &p, card));
                }
                if let Some(clip) = &clip {
                    crate::holo_ui::titled_plate(ui, "INSTANT REPLAY // CLIP", &format!("{} S", clip.seconds), |ui| clip_card(ui, &p, clip));
                }
                for spec in &widgets {
                    let label = format!("WIDGET // {}", spec.title.to_uppercase());
                    crate::holo_ui::titled_plate(ui, &label, short_type_upper(spec), |ui| widget_view::widget_bare(ui, &p, spec, &this.widget_host));
                }
            });
        };
        // As the desktop's background the column stands in a window of its own, where it would
        // stand on the board: as tall as its blocks, Sir's to move (a drag on its ground).
        if pieces_in_windows(ctx) {
            const MARGIN: f32 = 10.0;
            let place = Rect::from_min_size(board_to_screen(ctx, pos2(screen.left() + 24.0, top)), vec2(SYSTEM_COLUMN, 220.0)).expand(MARGIN);
            piece_window(ctx, ViewportId::from_hash_of("jai_system_column"), "System \u{2014} J.A.I.", place, |ui| {
                window_ground(ui);
                let used = Frame::none().inner_margin(Margin::same(MARGIN)).show(ui, |ui| blocks(self, ui)).response.rect;
                if (used.height() - ui.ctx().screen_rect().height()).abs() > 2.0 {
                    ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(vec2(SYSTEM_COLUMN + 2.0 * MARGIN, used.height())));
                }
            });
            self.system_column_rect = None;
            return false;
        }
        let column = egui::Area::new(egui::Id::new("hud_system_column")).default_pos(pos2(screen.left() + 24.0, top)).order(egui::Order::Middle).show(ctx, |ui| blocks(self, ui));
        // (Where the column stands: new blocks open beside it, not over it. Seen live
        // 2026-11-12: a widget in the column under a block that opened in the same corner.)
        self.system_column_rect = Some(column.response.rect);
        true
    }

    /// `[ HISTORY ]`: saved reports (click to put one on the board) and the conversation.
    fn history_overlay(&mut self, ctx: &egui::Context, m: &HudModel) {
        let screen = home_screen(ctx);
        let size = vec2(560.0f32.min(screen.width() - 48.0), (screen.height() * 0.72).max(240.0));
        let mut present: Option<i64> = None;
        let mut close = false;
        let mut open_window: Option<String> = None;
        let closed = floating_block(ctx, "hud_history", "History", pos2(screen.right() - 24.0 - size.x, screen.top() + 12.0 + HEADER_H + 14.0), vec2(size.x, size.y + 120.0), |ui| {
            {
                ui.set_width(size.x);
                // (An area hands on only the height it had before: `column_scroll`.)
                ui.set_max_height(size.y + 120.0);
                crate::holo_ui::titled_plate(ui, "HISTORY // CONVERSATION & REPORTS", "CLOSE", |ui| {
                    if crate::holo_ui::key(ui, "CLOSE").clicked() {
                        close = true;
                    }
                    crate::holo_block::faded(ui, egui::ScrollArea::vertical().max_height(size.y).auto_shrink([false, true]).stick_to_bottom(true), |ui| {
                        if !self.history_reports.is_empty() {
                            ui.label(RichText::new("REPORTS").font(strip_font()).color(crate::hud_board::active_colour()));
                            for (id, topic, when, blocks, sources) in &self.history_reports {
                                let text = format!("{}  ·  {blocks} BLOCKS · {sources} SOURCES  ·  {}", topic.to_uppercase(), when.get(..16).unwrap_or(when));
                                if crate::holo_ui::key(ui, text).on_hover_text("Put this report on the board").clicked() {
                                    present = Some(*id);
                                }
                            }
                            ui.add_space(8.0);
                        }
                        ui.label(RichText::new("CONVERSATION").font(strip_font()).color(crate::hud_board::active_colour()));
                        for line in m.messages.iter().filter(|l| !l.text.trim().is_empty()) {
                            let (who, colour) = match line.who {
                                Speaker::Sir => ("YOU //", strip_muted()),
                                Speaker::Jai => ("J.A.I. //", crate::hud_board::active_colour()),
                                Speaker::System => ("SYSTEM //", strip_muted()),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(who).font(strip_font()).color(colour));
                                let ink = if line.faded { strip_muted() } else { strip_ink() };
                                ui.add(egui::Label::new(RichText::new(&line.text).size(13.0).color(ink)).wrap());
                                if line.who == Speaker::Jai && crate::holo_ui::key(ui, "OPEN").on_hover_text("This reply in a window of its own").clicked() {
                                    open_window = Some(line.text.clone());
                                }
                            });
                            ui.add_space(4.0);
                        }
                    });
                });
            }
        });
        if close || closed {
            self.history_open = false;
        }
        if let Some(text) = open_window {
            self.command(HudCommand::OpenTextWindow { title: "J.A.I.".into(), text });
        }
        if let Some(id) = present {
            let db = self.app.config().read(|c| c.memory_db_path.clone());
            if let Some(report) = crate::hud_board::load_report_by_id(&db, id) {
                crate::hud_board::present(report);
                self.history_open = false;
            }
        }
    }

    /// `[ CAMERA ]`: the live webcam as a block (top left of the board).
    fn camera_block(&mut self, ctx: &egui::Context) {
        if let Some((seq, frame)) = crate::camera_feed::frame() {
            if self.camera_texture.as_ref().is_none_or(|(s, _)| *s != seq) {
                match self.camera_texture.as_mut() {
                    Some((s, t)) => {
                        t.set((*frame).clone(), egui::TextureOptions::LINEAR);
                        *s = seq;
                    }
                    None => self.camera_texture = Some((seq, ctx.load_texture("hud_camera", (*frame).clone(), egui::TextureOptions::LINEAR))),
                }
            }
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        let screen = home_screen(ctx);
        let devices = crate::camera_feed::devices();
        let name = devices.get(self.camera_device).cloned().unwrap_or_else(|| "DEFAULT CAMERA".into()).to_uppercase();
        let status = crate::camera_feed::status();
        let mut close = false;
        let closed = floating_block(ctx, "hud_camera", "Camera", pos2(screen.center().x - 200.0, screen.top() + 12.0 + HEADER_H + 14.0), vec2(400.0, 330.0), |ui| {
            {
                ui.set_width(400.0);
                crate::holo_ui::titled_plate(ui, &format!("CAMERA // {name}"), &status, |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 225.0), Sense::hover());
                    match &self.camera_texture {
                        Some((_, t)) => {
                            ui.painter().image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            holo::corner_ticks(ui.painter(), rect, 6.0, holo::a(strip_ink(), 0.75));
                        }
                        None => {
                            holo::outline(ui.painter(), &holo::plate(rect, [0.0; 4]), holo::a(strip_ink(), 0.14));
                            holo_kit::empty(ui.painter(), rect, if status == "STARTING" { "Connecting to the camera…" } else { status.as_str() }, 1.0);
                        }
                    }
                    if crate::holo_ui::key(ui, "CAMERA OFF").clicked() {
                        close = true;
                    }
                });
            }
        });
        if close || closed {
            self.camera_open = false;
            crate::camera_feed::stop();
            self.camera_texture = None;
        }
    }

    /// `[ SCREENS ]`: the monitors, sending the HUD to one of them, and one board across all.
    /// As the desktop's background: the background over every monitor or the first, and his
    /// emblem on the one he is sent to.
    fn screens_block(&mut self, ctx: &egui::Context) {
        let screen = home_screen(ctx);
        let monitors = crate::monitors::list();
        let iface = self.app.config().read(|c| c.interface.clone());
        let background = self.as_background();
        let spanning = if background { iface.background_all_monitors } else { iface.span_all_monitors };
        // The monitor the HUD is on (none of them alone while it spans them); as the background,
        // the one his emblem stands on.
        let ppp = ctx.pixels_per_point();
        let here = if background {
            emblem_monitor(self.app.config(), &monitors)
        } else {
            ctx.input(|i| i.viewport().inner_rect).filter(|_| !spanning).and_then(|r| crate::monitors::monitor_at(&monitors, (r.center().x * ppp) as i32, (r.center().y * ppp) as i32))
        };
        let width = SCREENS_WIDTH.min(screen.width() - 32.0);
        let mut picked = ScreensPick::default();
        // Always at the home screen's top right, also after the HUD changed monitors.
        egui::Area::new(egui::Id::new("hud_screens")).fixed_pos(pos2(screen.right() - 24.0 - width, screen.top() + 12.0 + HEADER_H + 14.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.set_width(width);
            picked = screens_menu(ui, &monitors, spanning, here);
        });
        if let Some(n) = picked.send {
            if background {
                // His emblem goes there; the background first spreads over every monitor when
                // that one is not part of it yet.
                if !spanning && home_monitor(&monitors).is_some_and(|first| first.index != n) {
                    self.app.config().update(|c| c.interface.background_all_monitors = true);
                }
                if let Err(e) = send_emblem_to_monitor(self.app.config(), &monitors, n) {
                    lock(&self.model).toast(e, true);
                }
            // The fullscreen board (also one that spans the monitors) goes there at once; a
            // plain window is moved as any window is. No language model either way.
            } else if !send_board_to_monitor(self.app.config(), n) {
                self.run_calls(vec![("move_window_to_monitor", serde_json::json!({ "app_or_window_name": "jai", "target_monitor": n }))]);
            }
        }
        if let Some(on) = picked.span.filter(|on| *on != spanning) {
            let mut next = iface.clone();
            if background {
                next.background_all_monitors = on;
            } else {
                next.span_all_monitors = on;
                next.fullscreen = !on;
            }
            self.set_interface(next);
        }
        if picked.close {
            self.screens_open = false;
        }
    }

    /// Settings → Look: the essentials, and the full Interface window.
    fn look_card(&mut self, ui: &mut Ui, iface: &InterfaceSettings) {
        card(ui, "LOOK", |_| {}, |ui| {
            let mut s = iface.clone();
            crate::holo_ui::toggle(ui, &mut s.animations, "Animations");
            ui.add_space(4.0);
            if crate::holo_ui::key(ui, "All settings…").clicked() {
                settings_panel::open(true);
            }
            muted(ui, "Or just ask: “JAI, orb mode”, “mini mode”, “clear the board”.");
            if &s != iface {
                s.sanitize();
                self.set_interface(s);
            }
        });
    }

    /// A tool run from a click (no speech): its result shows in the view it opens.
    fn run_tool(&self, name: &'static str, args: serde_json::Value) {
        let tools = self.app.tools().clone();
        self.rt.spawn(async move {
            let out = tools.execute(name, &args).await;
            if !out.success {
                warn!("[HUD] {name}: {}", out.output);
            }
            request_repaint();
        });
    }

    /// The navigation dock (and "go to the sim lab" / "home view"): `toggle` (a click) closes a
    /// view that is already up; a spoken request only ever opens it.
    fn switch_view(&mut self, mode: crate::hud_dock::HudViewMode, toggle: bool) {
        use crate::hud_dock::{all_own, HudViewMode as V};
        info!("[HUD] Navigation: {}{}", mode.id(), if toggle { " (click)" } else { "" });
        *lock(&VIEW_ZOOM) = Some(Instant::now());
        let tools = self.app.tools().clone();
        let lab = &tools.stark.simlab;
        let engine = &tools.widgets;
        let in_app_sims: Vec<String> = lab.list().into_iter().map(|s| s.id).filter(|_| !all_own()).collect();
        let window_open = |id: &str| engine.windows().iter().any(|w| w.id == id);
        match mode {
            V::Home => {
                self.core_open = false;
                self.console_open = false;
                self.history_open = false;
                self.screens_open = false;
                if crate::hud_board::is_active() {
                    crate::hud_board::sign_off();
                }
                // Blocks on the board close; his windows stay where Sir put them.
                for id in &in_app_sims {
                    lab.close(id);
                }
                for w in engine.windows().into_iter().filter(|_| !all_own()) {
                    engine.window_closed_by_user(&w.id);
                }
                pairing_view::close_in_app();
            }
            V::Board => {
                self.core_open = false;
                if crate::hud_board::is_active() {
                    if toggle {
                        crate::hud_board::sign_off();
                    }
                } else if crate::hud_board::restore().is_none() {
                    let db = self.app.config().read(|c| c.memory_db_path.clone());
                    match crate::hud_board::load_report(&db, None) {
                        Some(report) => {
                            crate::hud_board::present(report);
                        }
                        None => info!("[HUD] HUD BOARD: no report to restore yet"),
                    }
                }
            }
            V::CoreArchitectureSphere => self.core_open = if toggle { !self.core_open } else { true },
            V::SimLab => {
                if toggle && !in_app_sims.is_empty() {
                    for id in &in_app_sims {
                        lab.close(id);
                    }
                } else if lab.list().is_empty() {
                    self.run_tool("launch_interactive_simulation", serde_json::json!({ "sim_type": "function", "formula_or_params": "x^2 - 2x - 3", "title": "Function plotter" }));
                }
            }
            V::Telemetry | V::Schedule => {
                let (id, view) = if mode == V::Telemetry { ("system_telemetry", "system_stats") } else { ("schedule", "schedule") };
                if toggle && window_open(id) {
                    engine.window_closed_by_user(id);
                } else if !window_open(id) {
                    self.run_tool("open_jai_view", serde_json::json!({ "view": view }));
                }
            }
            V::TextConsole => {
                self.console_open = if toggle { !self.console_open } else { true };
                self.console_focus = self.console_open;
            }
        }
    }

    /// The windows of `admin::WINDOWS` that are up; on another PC of Sir's, those its account may
    /// not show close (one just opened asks for his admin sign-in). On his main PC only while the
    /// admin window shows which are open.
    fn gate_windows(&mut self) {
        if !crate::fleet::is_client(&self.app.config().read(|c| c.main_pc.clone())) && !admin_view::is_open() {
            return;
        }
        let tools = self.app.tools().clone();
        let widgets: Vec<String> = tools.widgets.windows().into_iter().map(|w| w.id).collect();
        let widget = |id: &str| widgets.iter().any(|w| w == id);
        let up: Vec<&'static str> = crate::admin::WINDOWS
            .iter()
            .map(|w| w.id)
            .filter(|&id| match id {
                "admin" => admin_view::is_open(),
                "accounts" => accounts_view::is_open(),
                "account_report" => crate::kit::text_is_up(id),
                "notebook" => crate::kit::text_is_up(crate::kit_notes::NOTEBOOK),
                "settings" => settings_panel::is_open(),
                "feature_lab" => features_view::is_open(),
                "discord" => discord_view::is_open(),
                "system_stats" => widget("system_telemetry"),
                "schedule" | "contacts" => widget(id),
                "workout_finance" => widget(id) || self.workout.open,
                "simulation_lab" => !tools.stark.simlab.list().is_empty(),
                "mobile_qr" => pairing_view::is_open(),
                "theme_studio" => theme_studio::is_open(),
                "mixer" => mixer_view::is_open(),
                "hotkeys" => hotkeys_view::is_open(),
                "screens" => self.screens_open,
                "integrations_hub" => self.hub.open,
                "devices" => self.drawer_open,
                "missed" => crate::hud_notify::missed_is_open(),
                "voice_setup" => crate::voice_setup::active(),
                "changes" => crate::changes::is_open(),
                panel => crate::jai_views::panel_is_open(panel),
            })
            .collect();
        for id in crate::admin::gate(self.app.config(), &self.rt, &up) {
            info!("[ADMIN] The {id} window is not this PC's: closed");
            match id {
                "admin" => admin_view::set_open(false),
                "accounts" => accounts_view::close(),
                "account_report" => crate::kit::close_text(id),
                "notebook" => crate::kit::close_text(crate::kit_notes::NOTEBOOK),
                "settings" => settings_panel::open(false),
                "feature_lab" => features_view::set_open(false),
                "discord" => discord_view::close(),
                "system_stats" | "schedule" | "contacts" | "workout_finance" => {
                    let _ = tools.widgets.close_window(if id == "system_stats" { "system_telemetry" } else { id });
                    self.workout.open &= id != "workout_finance";
                }
                "simulation_lab" => {
                    for sim in tools.stark.simlab.list() {
                        tools.stark.simlab.close(&sim.id);
                    }
                }
                "mobile_qr" => pairing_view::toggle(),
                "theme_studio" => theme_studio::close(),
                "mixer" => mixer_view::close(),
                "hotkeys" => hotkeys_view::close(),
                "screens" => self.screens_open = false,
                "integrations_hub" => self.hub.open = false,
                "devices" => self.drawer_open = false,
                "missed" => crate::hud_notify::close_missed(),
                "voice_setup" => crate::voice_setup::cancel(),
                "changes" => crate::changes::open(false),
                panel => crate::jai_views::shut_panel(panel),
            }
        }
    }

    /// Dialogs, the settings and hub windows, JAI's extra windows, push-to-talk and the frame
    /// rate: everything after the HUD's own canvas.
    fn after_body(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        let iface = iface.clone();
        mark("dialogs");
        self.dialogs(ctx);
        if WORKOUT_REQUEST.swap(false, Ordering::AcqRel) {
            self.workout.open = true;
        }
        self.hub.poll_request();
        if SCREENS_REQUEST.swap(false, Ordering::AcqRel) {
            self.screens_open = true;
        }
        if DEVICES_REQUEST.swap(false, Ordering::AcqRel) {
            self.drawer_open = true;
        }
        // On another PC of Sir's: a window its account may not show closes (`admin`).
        self.gate_windows();
        if self.hub.open {
            self.hub.show(ctx, &self.palette, &self.app, &self.rt);
        }
        // His windows of their own that are on their way somewhere ("tidy the windows", "move
        // it to my second monitor"): one step of their glide.
        if crate::own_windows::step() {
            ctx.request_repaint();
        }
        // (Those Sir pinned on top stay there, and where each stands is kept for its next
        // opening: looked after every couple of seconds.)
        crate::own_windows::tend();
        // JAI's tool views: ROSE blocks inside the HUD, or their own OS windows when detached.
        crate::hud_dock::begin_frame();
        // "Move it to the middle", "full screen the Earth": where his views go, this frame.
        apply_arrangements(ctx);
        mark("his windows");
        widget_view::show_windows(ctx, &self.widget_host);
        // The Simulation Lab's windows (60 FPS while they animate).
        mark("simulation lab");
        simlab_view::show_simulations(ctx, &self.widget_host);
        // The phone pairing window (QR code), when open.
        mark("small views");
        pairing_view::show(ctx, &self.widget_host);
        // The Feature Lab (`[ FEATURES ]`), when open.
        features_view::show(ctx, &self.app, &self.rt);
        discord_view::show(ctx, self.app.tools(), &self.rt);
        admin_view::show(ctx, &self.app, &self.rt);
        // Orders a block gave (the hearing log's corrected words): carried out as if typed,
        // after what the misheard words changed in the last two minutes is taken back.
        for (text, undo_first) in std::mem::take(&mut *lock(&ORDERS)) {
            let (app, tools, model) = (self.app.clone(), self.app.tools().clone(), Arc::clone(&self.model));
            self.rt.spawn(async move {
                if undo_first {
                    let _ = tools.execute("undo_last_action", &serde_json::json!({ "within_s": 120 })).await;
                }
                if let Some(reply) = app.handle_command(HudCommand::Chat { text }).await {
                    lock(&model).apply(reply);
                    request_repaint();
                }
            });
        }
        // The Theme Studio; `[ SAVE THEME ]` saves and applies the theme.
        if let Some(t) = theme_studio::show(ctx) {
            let hex = crate::interface::hex;
            let spec = serde_json::json!({ "name": t.name, "core_primary": hex(t.core_primary), "core_secondary": hex(t.core_secondary), "window_border": hex(t.window_border), "background_tint": hex(t.background_tint) });
            self.configure_look(&iface, serde_json::json!({ "create_theme": spec }));
        }
        // The settings panel ("open settings", VIEW → ALL SETTINGS): what it sets and opens.
        match settings_panel::show(ctx, &iface) {
            Some(settings_panel::Out::Set(next)) => self.set_interface(*next),
            Some(settings_panel::Out::Open(tool)) => {
                use settings_panel::Tool;
                match tool {
                    Tool::Devices => self.drawer_open = true,
                    Tool::ThemeStudio => show_theme_studio(true),
                    Tool::Screens => self.screens_open = true,
                    Tool::Hub => self.hub.open = true,
                    Tool::FeatureLab => show_features(true),
                    Tool::Mixer => show_mixer(true),
                    Tool::Hotkeys => show_hotkeys(),
                    Tool::VoiceSetup => crate::hud_feel::apply(&serde_json::json!({"voice_setup": true})),
                    Tool::Missed => crate::hud_feel::apply(&serde_json::json!({"missed": true})),
                }
            }
            None => {}
        }
        // "What did I miss?": the notifications of the last hours.
        crate::hud_notify::show(ctx);
        // The kit's blocks: the notebook, the hearing log, the coach, the privacy board, the
        // changes list and its explanations.
        crate::kit::show(ctx);
        // "Show your windows": their list.
        self.windows_list(ctx);
        // "Learn my voice": the set-up's block (with his voice's level as it comes in).
        crate::voice_setup::show(ctx, self.reactor.mic);
        // The live subtitles' bar, while they are on.
        crate::live_subtitles::show(ctx, &self.app, &self.rt);
        // The dashboard on an upright monitor the board is not on, behind the desktop's icons.
        // (He as the whole desktop's background is the board's own window: `sync_background`.
        // Then the upright monitor is part of him, and the dashboard is drawn on it there.)
        let state = lock(&self.model).state.clone();
        let own_backdrop = iface.portrait_dashboard && background_rect(&iface).is_none() && !self.as_background();
        if let Some(crate::desktop_backdrop::Refused(why)) = crate::desktop_backdrop::show(ctx, own_backdrop && !windows_still(ctx), self.hwnd, iface.span_all_monitors, true, true, &state) {
            lock(&self.model).toast(why, true);
            // (Off, or the refusal would come again with every frame.)
            self.app.config().update(|c| c.interface.portrait_dashboard = false);
        }
        mark("mixer");
        mixer_view::show(ctx, &self.rt);
        hotkeys_view::show(ctx, self.app.config());
        accounts_view::show(ctx);
        login_view::show(ctx, &self.rt, self.app.config());
        // The network radar, the disk map and the Earth, when open.
        mark("radar");
        radar_view::show(ctx, &self.rt);
        mark("disk map");
        disk_map_view::show(ctx, &self.rt);
        mark("earth");
        earth_view::show(ctx, &self.rt);
        // Pictures, live cameras and street view, when open.
        mark("pictures");
        media_view::show(ctx, &self.rt);
        // The 3D studio, when open (and its GPU memory let go once closed).
        mark("3D studio");
        model3d_view::show(ctx, &self.rt);
        mark("small views");
        departures_view::show(ctx, &self.rt); // live layers (C1)
        // The spatial workspace: floating glass windows.
        crate::workspace_canvas::show(ctx, &self.app, &self.rt);
        self.workout.show(ctx, &self.palette, &self.app, &self.rt);
        // The views Sir put away ("minimise it"), as chips over the bottom strip.
        block_tray(ctx);
        self.tour_outline(ctx);
        mark("end");

        let ptt = self.ptt_button || self.ptt_key;
        if ptt != self.ptt_sent {
            self.ptt_sent = ptt;
            self.command(HudCommand::Ptt { active: ptt });
        }

        // While something moves (the emblem unfolds or docks, blocks cascade, JAI listens,
        // thinks or speaks, the camera runs) the HUD draws at the display's own rate when Sir
        // is at it, at 60 FPS when he is elsewhere; 30 FPS at rest; never an unthrottled loop.
        let (busy, pointer, attended) = {
            let m = lock(&self.model);
            let badge = m.quick.as_ref().is_some_and(|(_, _, until)| *until > Instant::now());
            let busy = matches!(m.state.as_str(), "listening" | "thinking" | "speaking") || badge;
            let (pointer, attended) = ctx.input(|i| (i.pointer.any_down() || i.pointer.is_decidedly_dragging(), i.focused || i.pointer.has_pointer()));
            (busy, pointer, attended)
        };
        let moving = self.emblem.is_animating() || crate::hud_board::researching() || self.camera_open;
        let active = busy || pointer || moving || iface.orb_mode || welcome_level() > 0.0 || self.reactor.voice > 0.02;
        // At rest with nobody looking closely (no key or pointer for minutes, or the models
        // in standby for a game) a third of the frames is enough: the hours he is away.
        let fps = if active || !iface.animations { target_fps(iface.animations, active, attended, display_hz()) } else { rest_fps(nobody_watching()) };
        // (As the desktop's background: `background_fps`. The corner orb asks for its own
        // frames while it is out, the pointer over the desktop for one with every move.)
        let fps = if self.as_background() { background_fps(fps, active, crate::wallpaper::hidden() && self.corner_orb.is_home(), crate::hud_feel::game_in_front()) } else { fps };
        ctx.request_repaint_after(Duration::from_secs_f32(1.0 / fps));
        board_due_in(Duration::from_secs_f32(1.0 / fps));
    }
}

/// The fastest the HUD ever draws.
pub const MAX_FPS: f32 = 180.0;

/// When the music's spectrum on the emblem wants its next frame: never while nobody sees the
/// emblem (`unseen`: a window or a game covers it, the orb at home), else 60 a second, 25 beside
/// a game.
fn music_frame(music: bool, unseen: bool, gaming: bool) -> Option<Duration> {
    (music && !unseen).then(|| Duration::from_millis(if gaming { 40 } else { 16 }))
}

/// His frame rate as the desktop's background, from the board's own (`fps`). `unseen`: a
/// window covers his emblem and the orb is at home, so he draws for nobody: twelve frames a
/// second are plenty, and two beside a game, as at rest beside one. Seen, he draws as the
/// board does: at twelve his rings went round in steps (Sir, 2026-11-14: "some animations are
/// stuttery"; until then every frame at rest was held to twelve a second, seen or not).
pub fn background_fps(fps: f32, active: bool, unseen: bool, gaming: bool) -> f32 {
    match (unseen, gaming) {
        (true, true) => 2.0,
        (true, false) => fps.min(12.0),
        (false, true) if !active => 2.0,
        _ => fps,
    }
}

/// The HUD's frame rate: with animations off 8; at rest 60; while something moves 60 — or the
/// display's own refresh rate (60 … `MAX_FPS`) when Sir is at the HUD (it has the focus or the
/// pointer), so dragging a block is as smooth as his monitor is fast.
pub fn target_fps(animations: bool, active: bool, attended: bool, display_hz: f32) -> f32 {
    match (animations, active, attended) {
        (false, ..) => 8.0,
        (true, false, _) => 60.0,
        (true, true, false) => 60.0,
        (true, true, true) => display_hz.clamp(60.0, MAX_FPS),
    }
}

/// The HUD's frame rate at rest: 60 while Sir is at the PC (the rings and clock turn without a
/// stutter), 10 when nobody is looking closely.
pub fn rest_fps(unwatched: bool) -> f32 {
    if unwatched {
        10.0
    } else {
        60.0
    }
}

/// After this long without a key or the pointer, nobody is taken to be watching.
const AWAY_AFTER_SECS: u64 = 120;

/// Whether nobody is looking closely: no key or pointer for two minutes, or the models are in
/// standby for a game. Windows is asked once a second; a scripted session is always watched
/// (its frames are counted on).
fn nobody_watching() -> bool {
    static STARTED: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    static ASK_AGAIN_MS: AtomicU64 = AtomicU64::new(0);
    static UNWATCHED: AtomicBool = AtomicBool::new(false);
    let now = STARTED.elapsed().as_millis() as u64;
    if now >= ASK_AGAIN_MS.load(Ordering::Relaxed) {
        ASK_AGAIN_MS.store(now + 1_000, Ordering::Relaxed);
        let unwatched = !crate::platform::in_test_session() && (crate::vram_guard::is_dormant() || crate::platform::idle_seconds().is_some_and(|idle| idle >= AWAY_AFTER_SECS));
        UNWATCHED.store(unwatched, Ordering::Relaxed);
    }
    UNWATCHED.load(Ordering::Relaxed)
}

/// The fastest connected monitor's refresh rate (Hz); 60 until it is known. Windows is asked
/// every 30 s on a thread of its own: the display driver can take a frame or more to answer
/// (12 ms measured), which the HUD's thread must never wait for.
fn display_hz() -> f32 {
    use std::sync::atomic::AtomicU32;
    static STARTED: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    static HZ: AtomicU32 = AtomicU32::new(60);
    static ASK_AGAIN_MS: AtomicU64 = AtomicU64::new(0);
    let now = STARTED.elapsed().as_millis() as u64;
    // Only the HUD's thread calls this, so one asker at a time.
    if now >= ASK_AGAIN_MS.load(Ordering::Relaxed) {
        ASK_AGAIN_MS.store(now + 30_000, Ordering::Relaxed);
        std::thread::Builder::new()
            .name("display-hz".into())
            .spawn(|| {
                if let Some(hz) = crate::monitors::fastest_refresh_hz() {
                    HZ.store(hz, Ordering::Relaxed);
                }
            })
            .ok();
    }
    HZ.load(Ordering::Relaxed) as f32
}

// ----------------------------------------------------------- arranging his views on command ----
// `window_commands` says what Sir wants ("move it to the middle", "full screen the Earth",
// "minimise it", "move the radar to my second monitor", "put them side by side"); here, in the
// HUD's frame, where the screens are known, a block glides there (`hud_dock`) and a window of
// its own is moved by `ViewportCommand`s.

/// The OS window of view `id` while it is a window of its own.
fn viewport_for_block(id: &str) -> ViewportId {
    match id {
        "earth" => ViewportId::from_hash_of("jai_earth"),
        "network_radar" => ViewportId::from_hash_of("jai_network_radar"),
        "disk_map" => ViewportId::from_hash_of("jai_disk_map"),
        "features" => ViewportId::from_hash_of("jai_feature_lab"),
        "audio_mixer" => ViewportId::from_hash_of("jai_audio_mixer"),
        "hotkeys" => ViewportId::from_hash_of("jai_hotkeys"),
        "theme_studio" => ViewportId::from_hash_of("jai_theme_studio"),
        "discord_audio" => ViewportId::from_hash_of("jai_discord_audio"),
        "pairing" => ViewportId::from_hash_of("jai_satellite_pairing"),
        // (An order for the 3D window found none: these had no entry.)
        "model3d" => ViewportId::from_hash_of("jai_model3d"),
        "departures" => ViewportId::from_hash_of("jai_departures"),
        "image_viewer" => ViewportId::from_hash_of("jai_image_viewer"),
        "webcams" => ViewportId::from_hash_of("jai_webcams"),
        "street_view" => ViewportId::from_hash_of("jai_street_view"),
        // The second, third … picture block (`gui_media::show`).
        id if id.starts_with("image_viewer_") => ViewportId::from_hash_of(format!("jai_{id}")),
        id => match id.strip_prefix("sim:") {
            Some(sim) => ViewportId::from_hash_of(("jai_simlab", sim)),
            None => ViewportId::from_hash_of(("jai_window", id)),
        },
    }
}

/// Windows of his own on their way somewhere: the window, from where to where (its top left, in
/// its own points) and since when.
static OS_GLIDES: Mutex<Vec<(ViewportId, Pos2, Pos2, Instant)>> = Mutex::new(Vec::new());

/// Each window on its way somewhere one step further (eased like a block's glide).
fn step_os_glides(ctx: &egui::Context) {
    let mut glides = lock(&OS_GLIDES);
    if glides.is_empty() {
        return;
    }
    glides.retain(|(viewport, from, to, since)| {
        let t = (since.elapsed().as_secs_f32() / crate::hud_dock::GLIDE.as_secs_f32()).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        ctx.send_viewport_cmd_to(*viewport, ViewportCommand::OuterPosition((*from + (*to - *from) * eased).round()));
        t < 1.0
    });
    ctx.request_repaint();
}

/// Window `viewport` (now at `outer`, its client area `inner`) goes to `to` (its own points):
/// its new size at once (a window resized in every frame flickers), then it glides there.
fn os_glide(ctx: &egui::Context, viewport: ViewportId, outer: Rect, inner: Rect, to: Rect) {
    let border = outer.size() - inner.size();
    if (to.size() - outer.size()).length() > 0.5 {
        ctx.send_viewport_cmd_to(viewport, ViewportCommand::InnerSize((to.size() - border).max(Vec2::splat(120.0))));
    }
    let mut glides = lock(&OS_GLIDES);
    glides.retain(|(other, ..)| *other != viewport);
    if windows_still(ctx) {
        ctx.send_viewport_cmd_to(viewport, ViewportCommand::OuterPosition(to.min.round()));
    } else {
        glides.push((viewport, outer.min, to.min, Instant::now()));
    }
    ctx.request_repaint();
}

/// The windows of his seen in full screen since Sir put them there.
static SEEN_FULL: Mutex<Vec<ViewportId>> = Mutex::new(Vec::new());

/// A window of his that has left full screen by other means (Esc, Windows, closed) is no longer
/// what "exit full screen" means.
fn forget_ended_fullscreens(ctx: &egui::Context) {
    let Some(ids) = crate::window_commands::fullscreen_windows() else { return };
    let mut seen = lock(&SEEN_FULL);
    for id in ids {
        let viewport = viewport_for_block(&id);
        match ctx.input(|i| i.raw.viewports.get(&viewport).map(|v| v.fullscreen)) {
            Some(Some(true)) => {
                if !seen.contains(&viewport) {
                    seen.push(viewport);
                }
            }
            // (Not yet in full screen: Windows takes a frame or two.)
            Some(_) if !seen.contains(&viewport) => {}
            _ => {
                seen.retain(|v| *v != viewport);
                crate::window_commands::note_window_fullscreen(&id, false);
            }
        }
    }
}

/// The arrangements waiting for this frame: carried out, or kept for the next frame while their
/// view is still coming up (dropped at their deadline).
fn apply_arrangements(ctx: &egui::Context) {
    step_os_glides(ctx);
    forget_ended_fullscreens(ctx);
    if !crate::window_commands::pending() {
        return;
    }
    for arrangement in crate::window_commands::take_pending() {
        if arrange(ctx, &arrangement) {
            continue;
        }
        if Instant::now() < arrangement.deadline {
            crate::window_commands::requeue(arrangement);
            ctx.request_repaint_after(Duration::from_millis(40));
        } else {
            warn!("[HUD] '{}' was not up in time to {}", arrangement.id, arrangement.action.name());
        }
    }
}

/// Carries out one arrangement; false when its view is not up yet (or must first come back
/// from the tray or into the HUD).
fn arrange(ctx: &egui::Context, a: &crate::window_commands::Arrangement) -> bool {
    use crate::hud_dock::{self as dock, Whereabouts};
    use crate::window_commands::Action;
    // (A view that is still opening may be known by its kind only: `sim:`.)
    let known = |id: &str| match dock::whereabouts(id) {
        Whereabouts::Closed => dock::find_open(id),
        _ => Some(id.to_string()),
    };
    let Some(id) = known(&a.id) else { return false };
    let other = a.other.as_deref().map(|o| known(o).unwrap_or_else(|| o.to_string()));
    let done = match dock::whereabouts(&id) {
        Whereabouts::Closed => return false,
        // Put away: brought back first (with its entrance); the rest follows once it is up.
        Whereabouts::Tray => {
            if matches!(a.action, Action::Minimize | Action::ExitFullscreen) {
                return true;
            }
            dock::restore(&id);
            a.action == Action::Restore
        }
        Whereabouts::Board => arrange_block(ctx, &id, other.as_deref(), a).unwrap_or(true),
        Whereabouts::Window => arrange_window(ctx, &id, other.as_deref(), a).unwrap_or(true),
    };
    if done {
        info!("[HUD] Arranged '{id}': {}", a.action.name());
    }
    done
}

/// A block glides (or, without motion, jumps) from `from` to `to`.
fn send_block(ctx: &egui::Context, id: &str, from: Rect, to: Rect) {
    if windows_still(ctx) {
        crate::hud_dock::place_at(id, to);
    } else {
        crate::hud_dock::glide_to(id, from, to);
    }
}

/// One arrangement for a block on the board. Some(false): not yet (try again next frame); None:
/// nothing to do.
fn arrange_block(ctx: &egui::Context, id: &str, other: Option<&str>, a: &crate::window_commands::Arrangement) -> Option<bool> {
    use crate::hud_dock::{self as dock, Whereabouts};
    use crate::window_commands::{self as wc, Action};
    let Some(rect) = dock::block_rect(id) else { return Some(false) };
    // A full-screen block is arranged from where it goes back to.
    let full = dock::fullscreen_home(id);
    let current = full.unwrap_or(rect);
    let regions = block_regions(ctx);
    let area = region_at(&regions, current.center());
    match a.action {
        Action::Move | Action::Resize => {
            let position = a.position.or((a.action == Action::Move && a.size.is_none()).then_some(wc::Position::Centre));
            let to = wc::target_rect(area, current, position, a.size);
            if full.is_some() {
                dock::exit_fullscreen();
            }
            send_block(ctx, id, rect, to);
        }
        Action::Fullscreen => {
            if full.is_none() {
                dock::enter_fullscreen(id, rect, home_screen(ctx));
            }
        }
        Action::ExitFullscreen => {
            if full.is_none() {
                return None;
            }
            dock::exit_fullscreen();
        }
        Action::Maximize => {
            if full.is_some() {
                dock::exit_fullscreen();
            }
            if !dock::is_maximized(id) {
                dock::toggle_maximize(id, area);
            }
        }
        Action::Restore => {
            if full.is_some() {
                dock::exit_fullscreen();
            } else if dock::is_maximized(id) {
                dock::toggle_maximize(id, area);
            }
        }
        Action::Minimize => {
            dock::minimize(id);
        }
        Action::ToMonitor => return Some(block_to_monitor(ctx, id, rect, a)),
        Action::SideBySide | Action::Swap => {
            let other = other.filter(|o| *o != id)?;
            match dock::whereabouts(other) {
                Whereabouts::Board => {}
                Whereabouts::Tray => {
                    dock::restore(other);
                    return Some(false);
                }
                Whereabouts::Window | Whereabouts::Closed => return None,
            }
            let other_rect = dock::block_rect(other)?;
            if dock::fullscreen_block().is_some_and(|f| f == id || f == other) {
                dock::exit_fullscreen();
            }
            if a.action == Action::SideBySide {
                let (left, right) = wc::side_by_side(area);
                // (Whichever stands further left keeps the left.)
                let ((l, l_from), (r, r_from)) = if rect.center().x <= other_rect.center().x { ((id, rect), (other, other_rect)) } else { ((other, other_rect), (id, rect)) };
                send_block(ctx, l, l_from, left);
                send_block(ctx, r, r_from, right);
            } else {
                send_block(ctx, id, rect, other_rect);
                send_block(ctx, other, other_rect, rect);
            }
        }
    }
    Some(true)
}

/// "Move the radar to my second monitor" for a block on the board: on a HUD that spans that
/// monitor it glides over to its part of the board (a board on one screen has nowhere to send
/// it). False: not yet.
fn block_to_monitor(ctx: &egui::Context, id: &str, rect: Rect, a: &crate::window_commands::Arrangement) -> bool {
    let Some(monitor) = a.monitor.as_ref() else { return true };
    let monitors = crate::monitors::recent();
    let ppp = ctx.pixels_per_point().max(0.1);
    let origin = ctx.input(|i| i.viewport().inner_rect.map_or(Pos2::ZERO, |r| r.min)).to_vec2();
    let centre = (rect.center() + origin) * ppp;
    let here = crate::monitors::monitor_at(&monitors, centre.x as i32, centre.y as i32);
    let Some(target) = crate::monitors::resolve(monitor, here, &monitors).and_then(|n| monitors.iter().find(|m| m.index == n)) else {
        warn!("[HUD] No monitor '{monitor}' for '{id}'");
        return true;
    };
    let points = |r: [i32; 4]| Rect::from_min_max(pos2(r[0] as f32, r[1] as f32) / ppp, pos2(r[2] as f32, r[3] as f32) / ppp);
    // Part of this HUD: a glide over to that monitor's part of the board.
    let part = points(target.rect).translate(-origin);
    if let Some(region) = block_regions(ctx).into_iter().find(|r| part.contains(r.center())) {
        send_block(ctx, id, rect, crate::window_commands::fit(Rect::from_center_size(region.center(), rect.size()), region));
    }
    true
}

/// One arrangement for a view in a window of its own. Some(false): not yet; None: nothing to do.
fn arrange_window(ctx: &egui::Context, id: &str, other: Option<&str>, a: &crate::window_commands::Arrangement) -> Option<bool> {
    use crate::hud_dock::{self as dock, Whereabouts};
    use crate::window_commands::{self as wc, Action};
    let viewport = viewport_for_block(id);
    let Some(info) = ctx.input(|i| i.raw.viewports.get(&viewport).cloned()) else { return Some(false) };
    let send = |command| ctx.send_viewport_cmd_to(viewport, command);
    let full = info.fullscreen == Some(true) || wc::fullscreen_view().as_deref() == Some(id);
    match a.action {
        Action::Fullscreen => {
            send(ViewportCommand::Fullscreen(true));
            wc::note_window_fullscreen(id, true);
        }
        Action::ExitFullscreen => {
            if !full {
                return None;
            }
            send(ViewportCommand::Fullscreen(false));
            wc::note_window_fullscreen(id, false);
        }
        Action::Maximize => send(ViewportCommand::Maximized(true)),
        Action::Minimize => send(ViewportCommand::Minimized(true)),
        Action::Restore => {
            if full {
                send(ViewportCommand::Fullscreen(false));
                wc::note_window_fullscreen(id, false);
            } else if info.minimized == Some(true) {
                send(ViewportCommand::Minimized(false));
            } else if info.maximized == Some(true) {
                send(ViewportCommand::Maximized(false));
            }
        }
        Action::Move | Action::Resize | Action::ToMonitor | Action::SideBySide | Action::Swap => {
            let outer = info.outer_rect?;
            let inner = info.inner_rect.unwrap_or(outer);
            // The window's own points (a monitor of another scale than the HUD's has its own).
            let ppp = (info.native_pixels_per_point.unwrap_or(1.0) * ctx.zoom_factor()).max(0.1);
            let monitors = crate::monitors::recent();
            let centre = outer.center() * ppp;
            let here = crate::monitors::monitor_at(&monitors, centre.x as i32, centre.y as i32);
            let work = |n: usize| monitors.iter().find(|m| m.index == n).map(|m| Rect::from_min_max(pos2(m.work[0] as f32, m.work[1] as f32) / ppp, pos2(m.work[2] as f32, m.work[3] as f32) / ppp));
            if full {
                send(ViewportCommand::Fullscreen(false));
                wc::note_window_fullscreen(id, false);
            }
            match a.action {
                Action::Move | Action::Resize => {
                    let area = work(here?)?;
                    let position = a.position.or((a.action == Action::Move && a.size.is_none()).then_some(wc::Position::Centre));
                    os_glide(ctx, viewport, outer, inner, wc::target_rect(area, outer, position, a.size));
                }
                Action::ToMonitor => {
                    let n = crate::monitors::resolve(a.monitor.as_ref()?, here, &monitors)?;
                    let area = work(n)?;
                    // (Across monitors in one step: a window passing onto a screen of another
                    // scale mid-way would take the rest of the way in that screen's points.)
                    let to = wc::fit(Rect::from_center_size(area.center(), outer.size()), area);
                    send(ViewportCommand::OuterPosition(to.min.round()));
                    if (to.size() - outer.size()).length() > 0.5 {
                        send(ViewportCommand::InnerSize((to.size() - (outer.size() - inner.size())).max(Vec2::splat(120.0))));
                    }
                }
                _ => {
                    let other = other.filter(|o| *o != id)?;
                    match dock::whereabouts(other) {
                        Whereabouts::Window => {}
                        Whereabouts::Tray => {
                            dock::restore(other);
                            return Some(false);
                        }
                        Whereabouts::Board | Whereabouts::Closed => return None,
                    }
                    let other_viewport = viewport_for_block(other);
                    let other_info = ctx.input(|i| i.raw.viewports.get(&other_viewport).cloned())?;
                    let other_outer = other_info.outer_rect?;
                    let other_inner = other_info.inner_rect.unwrap_or(other_outer);
                    if a.action == Action::SideBySide {
                        let (left, right) = wc::side_by_side(work(here?)?);
                        let (me, them) = if outer.center().x <= other_outer.center().x { (left, right) } else { (right, left) };
                        os_glide(ctx, viewport, outer, inner, me);
                        os_glide(ctx, other_viewport, other_outer, other_inner, them);
                    } else {
                        os_glide(ctx, viewport, outer, inner, other_outer);
                        os_glide(ctx, other_viewport, other_outer, other_inner, outer);
                    }
                }
            }
        }
    }
    Some(true)
}

/// The views Sir put away ("minimise it"): one chip each over the bottom strip of the home
/// screen; a click brings the view back (as "restore it" does).
fn block_tray(ctx: &egui::Context) {
    let tray = crate::hud_dock::minimized_blocks();
    if tray.is_empty() {
        return;
    }
    let home = home_screen(ctx);
    let at = pos2(home.left() + 24.0, home.bottom() - 12.0 - TELEMETRY_H - 8.0 - 26.0);
    egui::Area::new(egui::Id::new("jai_block_tray")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        ui.horizontal(|ui| {
            for (id, title) in &tray {
                let name = title.split(" // ").next().unwrap_or(title).trim().to_uppercase();
                // A closed block waits here dim for ten minutes; one put away stands bright.
                let parked = crate::hud_dock::is_parked(id);
                let chip = ui
                    .scope(|ui| {
                        ui.set_opacity(if parked { 0.5 } else { 1.0 });
                        holo_ui::key(ui, name.as_str())
                    })
                    .inner
                    .on_hover_text(if parked { "Closed. Click within ten minutes to bring it back." } else { "Put away. Click, or say \"restore it\", to bring it back." });
                if chip.clicked() {
                    crate::hud_dock::restore(id);
                }
            }
        });
    });
}

/// The bottom strip's level meter: its width and how many cells it has.
const METER_W: f32 = 94.0;
const METER_CELLS: usize = 24;

/// The strip's chip: its word, its colour and whether it is solid. `LIVE` is a solid block of
/// the accent (he hears), `MUTE` one of the alert colour, `SLEEP` an outline.
fn strip_badge(muted: bool, sleeping: bool) -> (&'static str, Color32, bool) {
    let t = holo::theme();
    if muted {
        ("MUTE", t.alert(), true)
    } else if sleeping {
        ("SLEEP", t.muted(), false)
    } else {
        ("LIVE", t.accent(), true)
    }
}

/// The left end of the bottom strip from `at` (its top-left): the microphone as a row of cells
/// lit to its level (the alert colour where it clips) with the peak held as a tick, `LVL` and
/// `PK` in dB, and the `LIVE` / `MUTE` / `SLEEP` chip. Returns the chip's rect.
fn paint_strip_meter(painter: &egui::Painter, at: Pos2, lvl: f32, peak_db: f32, muted: bool, sleeping: bool) -> Rect {
    let t = holo::theme();
    let (mut x, mid) = (at.x, at.y + TELEMETRY_H / 2.0);
    let meter = Rect::from_min_size(pos2(x, mid - 4.0), vec2(METER_W, 8.0));
    holo_kit::cells(painter, meter, if muted { 0.0 } else { db_fraction(lvl) }, METER_CELLS, if lvl > -3.0 { t.alert() } else { t.accent() }, 1.0);
    let peak_x = holo::snap(x + METER_W * db_fraction(peak_db), holo::ppp(painter));
    holo::rule(painter, pos2(peak_x, mid - 6.0), pos2(peak_x, mid + 6.0), holo::a(t.ink(), 0.85));
    x += METER_W + 12.0;
    for (label, value) in [("LVL", lvl), ("PK", peak_db)] {
        let l = holo::text(painter, pos2(x, mid), Align2::LEFT_CENTER, Face::Label, label, RAIL_TEXT, t.muted());
        x = holo::text(painter, pos2(l.right() + 5.0, mid), Align2::LEFT_CENTER, Face::Mono, &format!("{value:.1} dB"), 11.0, t.ink()).right() + 14.0;
    }
    let (badge, ink, solid) = strip_badge(muted, sleeping);
    holo_kit::chip(painter, pos2(x, at.y), badge, ink, solid, 1.0)
}

/// One theme in a menu: its three lights as leaning swatches and its name, marked while it
/// is the one shown.
fn theme_row(ui: &mut Ui, look: &crate::holo_theme::Theme, label: &str, active: bool) -> egui::Response {
    let (row, response) = ui.allocate_exact_size(vec2(ui.available_width().max(226.0), holo_ui::ROW_H), Sense::click());
    let hot = ui.ctx().animate_bool_with_time(response.id.with("rail_hot"), response.hovered(), crate::holo_motion::TOUCH);
    let painter = ui.painter_at(row);
    let t = holo::theme();
    let lift = hot.max(active as u8 as f32 * 0.7);
    if lift > 0.01 {
        painter.rect_filled(row, Rounding::ZERO, holo::a(t.ink(), 0.07 * lift));
    }
    if active {
        painter.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), Rounding::ZERO, t.accent());
    }
    for (k, colour) in [look.accent(), look.deep(), look.ink()].into_iter().enumerate() {
        let swatch = Rect::from_min_size(pos2(row.left() + 10.0 + k as f32 * 13.0, row.center().y - 5.0), vec2(12.0, 10.0));
        holo::fill(&painter, &holo_kit::leaning(swatch), colour);
    }
    holo::text(&painter, pos2(row.left() + 58.0, row.center().y), Align2::LEFT_CENTER, Face::Label, label, RAIL_TEXT, if active { t.accent() } else { holo::mix(t.muted(), t.ink(), 0.6 + 0.4 * hot) });
    response
}

/// What Sir said, as the strip shows it: one line, at most `max_chars`.
fn heard_line(text: &str, max_chars: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if max_chars < 8 {
        return String::new();
    }
    if line.chars().count() <= max_chars {
        line
    } else {
        format!("{}…", crate::crash_guard::clip(&line, max_chars - 1).trim_end())
    }
}

/// One item of the header strip: an outlined box, or a plain label (`boxed` false).
#[derive(Debug, Clone, PartialEq)]
struct HeaderItem {
    id: &'static str,
    text: String,
    on: bool,
    boxed: bool,
}

/// The strip's items, left to right.
/// Which header boxes glow (their panel or block is open, or the reboot runs).
#[derive(Default)]
struct HeaderState {
    phone: bool,
    core: bool,
    features: bool,
    /// The changes list is up.
    changes: bool,
    history: bool,
    console: bool,
    rebooting: bool,
    camera: bool,
    screens: bool,
    /// The microphone is off or the privacy lock holds ("stop listening").
    mic_muted: bool,
    /// Whose voice he answers to.
    voice: VoiceLock,
    /// Dictation is on: what is said is typed into the app in front.
    dictating: bool,
    /// "Don't listen for ten minutes": how long until he listens again by himself.
    lock_left: Option<Duration>,
}

/// How long is left, as the header's box says it: "45 S", "9 MIN", "1 H 20".
fn left_words(left: Duration) -> String {
    let secs = left.as_secs();
    match secs {
        0..=89 => format!("{} S", secs.max(1)),
        90..=3599 => format!("{} MIN", (secs + 30) / 60),
        _ => format!("{} H {:02}", secs / 3600, (secs % 3600) / 60),
    }
}

/// Whose voice he answers to, as the header strip says it.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum VoiceLock {
    /// The lock is on and he knows Sir's voice.
    #[default]
    Yours,
    /// The lock is on and he is still learning the voice (he answers to his name meanwhile).
    Learning,
    /// Guests may speak for a while.
    Guests,
    /// The lock is off.
    Anyone,
}

impl VoiceLock {
    fn of(lock_on: bool, guests: bool, voice_known: bool) -> Self {
        match (lock_on, guests, voice_known) {
            (false, ..) => Self::Anyone,
            (true, true, _) => Self::Guests,
            (true, false, true) => Self::Yours,
            (true, false, false) => Self::Learning,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Yours => "VOICE: YOURS",
            Self::Learning => "VOICE: LEARNING",
            Self::Guests => "VOICE: GUESTS",
            Self::Anyone => "VOICE: ANYONE",
        }
    }

    /// Others are answered too: the box is lit, as an open door is.
    fn open(self) -> bool {
        matches!(self, Self::Guests | Self::Anyone)
    }
}

fn header_items(lit: &HeaderState, reboot: &str, source: &str) -> Vec<HeaderItem> {
    let item = |id: &'static str, text: &str, on: bool, boxed: bool| HeaderItem { id, text: text.to_string(), on, boxed };
    vec![
        item("theme", "THEME", false, true),
        item("phone", "PHONE QR", lit.phone, true),
        item("core", "CORE", lit.core, true),
        item("modules", "MODULES", false, true),
        item("features", "FEATURES", lit.features, true),
        item("changes", "CHANGES", lit.changes, true),
        item("history", "HISTORY", lit.history, true),
        item("view", "VIEW", false, true),
        item("clear", "CLEAR VIEW", false, true),
        item("console", "CONSOLE", lit.console, true),
        item("reboot", reboot, lit.rebooting, true),
        item("camera", "CAMERA", lit.camera, true),
        item("source_label", "CAMERA SOURCE", false, false),
        item("source", source, false, true),
        item("screens", "SCREENS", lit.screens, true),
        item("voice", lit.voice.label(), lit.voice.open(), true),
        // (While he types what is said, the box says so and is lit: one click ends it.)
        // (Told not to listen for a while: the box counts the time down.)
        item("mic", &match (lit.mic_muted, lit.lock_left) {
            (true, Some(left)) => format!("MIC: MUTED {}", left_words(left)),
            (true, None) => "MIC: MUTED".to_string(),
            _ if lit.dictating => "MIC: DICTATING".to_string(),
            _ => "MIC: ON".to_string(),
        }, lit.mic_muted || lit.dictating, true),
    ]
}

/// `[ MODULES ]`: each Stark module and the tool calls (name, JSON arguments) that open it.
const MODULES: &[(&str, &[(&str, &str)])] = &[
    ("HOLOMAT — MATH · PHYSICS · CHEM LAB", &[("open_jai_view", r#"{"view": "simulation_lab"}"#)]),
    ("CHRONOS — WEBUNTIS · ITSLEARNING · CALENDAR", &[("open_jai_view", r#"{"view": "schedule"}"#)]),
    ("MEDIA & AUDIO MIXER — PLAYLISTS · APP VOLUMES", &[("control_audio_mixer", r#"{"action": "list"}"#), ("control_music", r#"{"action": "list_playlists"}"#)]),
    ("SENTINEL & AEGIS — TELEMETRY · VRAM · SERVERS", &[("open_jai_view", r#"{"view": "system_stats"}"#), ("server_deck", "{}")]),
    ("DEEP RESEARCH AGENT", &[("manage_deep_research", r#"{"action": "open_workspace"}"#)]),
    ("TOOL FORGE — SELF-CREATED TOOLS", &[("list_forged_tools", "{}")]),
    ("MOTO & WEATHER RADAR — ROUTE · RAIN", &[("check_ride_and_commute_status", "{}"), ("get_precise_location_weather", "{}")]),
    ("DISCORD & WAVE LINK BRIDGE", &[("discord_call", r#"{"action": "status"}"#)]),
];

/// The camera picker is at least this wide (a field, not a button).
const SOURCE_PICKER_W: f32 = 190.0;

/// The header strip as it fits a screen.
struct HeaderRow {
    /// The items on the row and their rects, left to right.
    placed: Vec<(&'static str, Rect)>,
    /// The boxes the row had no room for, left to right: they are in the `[ MORE ]` menu.
    hidden: Vec<&'static str>,
    /// The `[ MORE ]` box, where the row begins, when anything is hidden.
    more: Option<Rect>,
    /// Where a hairline stands between two groups of boxes (x).
    seams: Vec<f32>,
}

/// Where a group of the strip begins: his windows, the view, the console, the devices. Groups
/// stand `HEADER_GROUP_GAP` apart with a hairline between them; boxes within one 6 px.
const HEADER_GROUPS: &[&str] = &["modules", "view", "console", "camera"];
const HEADER_GAP: f32 = 6.0;
const HEADER_GROUP_GAP: f32 = 22.0;

fn gap_before(id: &str) -> f32 {
    if HEADER_GROUPS.contains(&id) {
        HEADER_GROUP_GAP
    } else {
        HEADER_GAP
    }
}

/// The hairlines between the strip's groups.
fn paint_seams(p: &egui::Painter, row: &HeaderRow) {
    let Some((_, first)) = row.placed.first() else { return };
    for x in &row.seams {
        holo::rule(p, pos2(*x, first.top() + 5.0), pos2(*x, first.bottom() - 5.0), holo::theme().faint());
    }
}

/// The items' rects on one row between `left` and `right`, laid out right to left (each box its
/// text width plus 14 px, 6 px apart); `measure` gives a text's width. On a screen too narrow
/// for all of them (an upright monitor) the leftmost ones leave the row for the `[ MORE ]`
/// menu: nothing is ever drawn off the screen's edge.
fn header_layout(items: &[HeaderItem], left: f32, right: f32, y: f32, measure: impl Fn(&str) -> f32) -> HeaderRow {
    let widths: Vec<f32> = items
        .iter()
        .map(|item| {
            if !item.boxed {
                measure(&item.text) + 2.0
            } else if item.id == "source" {
                (measure(&item.text) + 34.0).max(SOURCE_PICKER_W)
            } else {
                measure(&item.text) + 14.0
            }
        })
        .collect();
    let more_w = measure("MORE") + 14.0;
    // The row from item `k` on, with the menu's box before it unless it is the whole strip.
    let needs = |k: usize| widths[k..].iter().sum::<f32>() + items[k + 1..].iter().map(|i| gap_before(i.id)).sum::<f32>() + if k > 0 { more_w + HEADER_GAP } else { 0.0 };
    let first = (0..items.len()).find(|k| needs(*k) <= right - left).unwrap_or(items.len());
    let mut x = right;
    let (mut placed, mut seams) = (Vec::new(), Vec::new());
    for (k, (item, w)) in items.iter().zip(&widths).enumerate().skip(first).rev() {
        placed.push((item.id, Rect::from_min_size(pos2(x - w, y), vec2(*w, HEADER_H))));
        let gap = if k > first { gap_before(item.id) } else { HEADER_GAP };
        if gap > HEADER_GAP {
            seams.push(x - w - gap / 2.0);
        }
        x -= w + gap;
    }
    placed.reverse();
    seams.reverse();
    HeaderRow {
        placed,
        hidden: items[..first].iter().filter(|item| item.boxed).map(|item| item.id).collect(),
        more: (first > 0).then(|| Rect::from_min_size(pos2(x - more_w, y), vec2(more_w, HEADER_H))),
        seams,
    }
}

/// The COMMS block's width and centre: half the screen (360-820 px), centred in what the text
/// console leaves free, and never under the docked emblem's corner (`hud_dock::DOCK_INSET` plus its
/// radius on the right).
fn comms_placement(screen: Rect, console_width: f32) -> (f32, f32) {
    let free_right = screen.right() - console_width.max(0.0);
    let free_width = (free_right - screen.left()).max(0.0);
    let corner = crate::hud_dock::DOCK_INSET + crate::holo_core::DOCKED_DIAMETER / 2.0 + 12.0;
    let width = (free_width * 0.5).clamp(360.0, 820.0).min(free_width - 2.0 * corner).max(240.0);
    let centre_x = (screen.left() + free_right) / 2.0;
    (width, centre_x)
}

thread_local! {
    /// The parts of the frame being drawn, each with when it began (`mark`).
    static FRAME_PARTS: std::cell::RefCell<Vec<(&'static str, Instant)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A part of the frame begins (the GUI thread's): a slow frame's log line names the parts that
/// took it, so a freeze is found by reading the log, not by guessing.
pub(crate) fn mark(part: &'static str) {
    FRAME_PARTS.with(|p| p.borrow_mut().push((part, Instant::now())));
}

/// The parts of the frame that ended at `end` and what each took (ms), the longest first.
fn frame_parts(end: Instant) -> Vec<(&'static str, f32)> {
    let marks = FRAME_PARTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    parts_of(&marks, end)
}

fn parts_of(marks: &[(&'static str, Instant)], end: Instant) -> Vec<(&'static str, f32)> {
    let mut parts: Vec<(&'static str, f32)> = Vec::new();
    for (k, (name, at)) in marks.iter().enumerate() {
        let until = marks.get(k + 1).map_or(end, |next| next.1);
        let ms = until.saturating_duration_since(*at).as_secs_f32() * 1000.0;
        match parts.iter_mut().find(|(n, _)| n == name) {
            Some(part) => part.1 += ms,
            None => parts.push((name, ms)),
        }
    }
    parts.sort_by(|a, b| b.1.total_cmp(&a.1));
    parts
}

/// A frame slower than this is a hitch he sees: logged, and a failure in the verifiers.
pub const SLOW_FRAME_MS: f64 = 50.0;

/// Frame-time diagnostics: a frame over `SLOW_FRAME_MS` is logged with the parts that took
/// most of it (at most every 5 s), and with `JAI_LOG=jai=debug` the frame rate every 2 s.
fn frame_stats(took: Duration, parts: &[(&'static str, f32)]) {
    let mut clock = lock(&FRAME_CLOCK);
    if took.as_secs_f64() * 1000.0 > SLOW_FRAME_MS && clock.last_slow.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
        clock.last_slow = Some(Instant::now());
        let named: Vec<String> = parts.iter().take(3).filter(|(_, ms)| *ms >= 1.0).map(|(name, ms)| format!("{name} {ms:.0}")).collect();
        warn!("[HUD] Slow frame: {} ms ({})", took.as_millis(), if named.is_empty() { "outside the HUD's own parts".to_string() } else { named.join(", ") });
    }
    if clock.recent.len() >= FRAME_HISTORY {
        clock.recent.pop_front();
    }
    clock.recent.push_back(took.as_secs_f32() * 1000.0);
    let (since, frames, worst) = clock.window.get_or_insert((Instant::now(), 0, Duration::ZERO));
    *frames += 1;
    *worst = (*worst).max(took);
    if since.elapsed() >= Duration::from_secs(2) {
        tracing::debug!("[HUD] {:.1} FPS, slowest frame {} ms", *frames as f32 / since.elapsed().as_secs_f32(), worst.as_millis());
        clock.window = None;
    }
}

/// How many frame times are kept for `frame_times`.
const FRAME_HISTORY: usize = 240;

struct FrameClock {
    /// The HUD's own work in each of the last frames (ms).
    recent: std::collections::VecDeque<f32>,
    last_slow: Option<Instant>,
    /// The debug log's 2 s window: (since, frames, slowest).
    window: Option<(Instant, u32, Duration)>,
}

static FRAME_CLOCK: Mutex<FrameClock> = Mutex::new(FrameClock { recent: std::collections::VecDeque::new(), last_slow: None, window: None });

/// The HUD's own work per frame over its last frames: (frames, median ms, average ms, worst
/// ms). None before the first frame.
pub fn frame_times() -> Option<(usize, f64, f64, f64)> {
    let costs: Vec<f64> = lock(&FRAME_CLOCK).recent.iter().map(|ms| *ms as f64).collect();
    frame_summary(&costs).map(|(median, average, worst)| (costs.len(), median, average, worst))
}

/// Frame costs (ms) as the self-tests judge them: (median, average, worst). The median is what
/// a frame costs the HUD: a frame's clock runs on while the scheduler gives the core to another
/// program, which only ever adds (2026-11-08, beside Minecraft with the verifier below normal
/// priority: the average rose from 0.7 to 5.8 ms and single frames to 25 ms, the HUD's work
/// the same), and a HUD that is slow itself has a slow median too.
pub fn frame_summary(costs: &[f64]) -> Option<(f64, f64, f64)> {
    let mut sorted = costs.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let median = match n {
        0 => return None,
        n if n % 2 == 1 => sorted[n / 2],
        n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
    };
    Some((median, costs.iter().sum::<f64>() / n as f64, sorted[n - 1]))
}

/// What Sir did with an in-app block this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockAction {
    pub close: bool,
}

/// How many sent lines the console remembers (across restarts too).
const CONSOLE_HISTORY_KEPT: usize = 60;

/// What Sir sent from the console, oldest first, and where the arrow keys stand in it (None:
/// on the line he is writing, which is kept while he looks back).
#[derive(Default)]
struct ConsoleHistory {
    sent: Vec<String>,
    at: Option<usize>,
    draft: String,
    loaded: bool,
}

static CONSOLE_HISTORY: Mutex<ConsoleHistory> = Mutex::new(ConsoleHistory { sent: Vec::new(), at: None, draft: String::new(), loaded: false });

impl ConsoleHistory {
    /// The lines of earlier sessions (the kit's store), once.
    fn load(&mut self) {
        if !self.loaded {
            self.loaded = true;
            if let Some(serde_json::Value::Array(lines)) = crate::kit::get("console", "history") {
                let mut kept: Vec<String> = lines.iter().filter_map(|l| l.as_str().map(str::to_string)).collect();
                kept.append(&mut self.sent);
                self.sent = kept;
            }
        }
    }

    /// A line was sent: it is the newest (once, however often it was sent).
    fn push(&mut self, line: &str) {
        self.load();
        self.sent.retain(|l| l != line);
        self.sent.push(line.to_string());
        let over = self.sent.len().saturating_sub(CONSOLE_HISTORY_KEPT);
        self.sent.drain(..over);
        (self.at, self.draft) = (None, String::new());
        let _ = crate::kit::put("console", "history", &serde_json::json!(self.sent));
    }

    /// One line back (arrow up). None: there is none further back.
    fn back(&mut self, current: &str) -> Option<String> {
        self.load();
        let at = match self.at {
            None => self.sent.len().checked_sub(1)?,
            Some(0) => return None,
            Some(n) => n - 1,
        };
        if self.at.is_none() {
            self.draft = current.to_string();
        }
        self.at = Some(at);
        self.sent.get(at).cloned()
    }

    /// One line forward (arrow down); past the newest, the line he was writing.
    fn forward(&mut self) -> Option<String> {
        let at = self.at?;
        if at + 1 < self.sent.len() {
            self.at = Some(at + 1);
            self.sent.get(at + 1).cloned()
        } else {
            self.at = None;
            Some(std::mem::take(&mut self.draft))
        }
    }
}

/// The console's remembered lines are forgotten (the privacy board's key).
pub(crate) fn forget_console_history() {
    let mut history = lock(&CONSOLE_HISTORY);
    (history.sent, history.at, history.draft, history.loaded) = (Vec::new(), None, String::new(), true);
    crate::kit::clear("console");
}

/// The caret of a single-line field goes behind its last character (the line was replaced
/// from outside: a completion, a recalled line).
fn caret_to_end(ctx: &egui::Context, id: egui::Id, line: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let end = egui::text::CCursor::new(line.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ctx, id);
    }
}

/// What Sir did in the text console this frame.
#[derive(Default)]
struct ConsoleActions {
    send: Option<String>,
    mode: Option<VoiceResponseMode>,
    close: bool,
    pin: bool,
}

/// The text console's id among the HUD's windows.
const CONSOLE_ID: &str = "console";

/// The console's input line's egui id: fixed, whatever stands above it.
pub(crate) const CONSOLE_INPUT_ID: &str = "jai_console_input";

/// Where the text console stands on the HUD this frame and stood in the last one (None: not on
/// the HUD).
static CONSOLE_RECT: Mutex<(Option<Rect>, Option<Rect>)> = Mutex::new((None, None));

/// The console drew at `rect` this frame; None at the start of a frame (what it drew in the last
/// one is kept for whatever is drawn before it).
fn publish_console_rect(rect: Option<Rect>) {
    let mut known = lock(&CONSOLE_RECT);
    match rect {
        Some(r) => known.0 = Some(r),
        None => *known = (None, known.0),
    }
}

/// Where the text console stands on the HUD (the pop-up cards keep off it: a card over its input
/// line took the clicks meant for the line). Drawn before the console, the last frame's place.
pub(crate) fn console_rect() -> Option<Rect> {
    let known = lock(&CONSOLE_RECT);
    known.0.or(known.1)
}

/// The console's foot: the completions for what Sir has typed (Tab takes the first, a click
/// takes one) and the input line with `[ TRANSMIT -> ]`. What he sent (Enter or the button).
///
/// "The text input field sometimes refuses my input" (2026-11-09) had three causes here:
/// - The line had no id of its own, so egui gave it one from its place among the widgets, and
///   the completions row appears above it (or goes) with nearly every key. With it the line's id
///   changed, egui took it for another widget and dropped the focus: the next keys went nowhere
///   (also through the extra pass a new layer or the foot growing asks for). It has a fixed id.
/// - Tab, which takes the completion, also moved egui's focus on to the next widget (the chips
///   or TRANSMIT): the line keeps it (`lock_focus`).
/// - TRANSMIT took the focus off the line: after sending, by Enter or the button, the line has
///   it again for the next command (JAI thinking or speaking never takes it).
fn console_foot(ui: &mut Ui, line: &mut String, completions: &[String], focus: &mut bool) -> Option<String> {
    // Arrow up and down walk through what Sir sent before (this session's lines and the last
    // sessions'), as a shell does; the line he was writing waits below the newest.
    let input_id = egui::Id::new(CONSOLE_INPUT_ID);
    // Escape empties a line that has something on it (and leaves the history where it began).
    if ui.memory(|m| m.has_focus(input_id)) && !line.is_empty() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        line.clear();
        let mut history = lock(&CONSOLE_HISTORY);
        (history.at, history.draft) = (None, String::new());
    }
    if ui.memory(|m| m.has_focus(input_id)) {
        let recalled = if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowUp)) {
            lock(&CONSOLE_HISTORY).back(line)
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowDown)) {
            lock(&CONSOLE_HISTORY).forward()
        } else {
            None
        };
        if let Some(text) = recalled {
            *line = text;
            caret_to_end(ui.ctx(), input_id, line);
        }
    }
    if !completions.is_empty() {
        let mut taken: Option<String> = None;
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab)) {
            taken = completions.first().cloned();
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            for (n, text) in completions.iter().enumerate() {
                if holo_ui::key_on(ui, text.as_str(), n == 0).on_hover_text(if n == 0 { "Tab completes the line with this" } else { "Click to use this" }).clicked() {
                    taken = Some(text.clone());
                }
            }
        });
        if let Some(text) = taken {
            *line = text;
            *focus = true;
            caret_to_end(ui.ctx(), input_id, line);
        }
        ui.add_space(4.0);
    }
    let mut sent = None;
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 150.0).max(120.0);
        let input = Frame::none()
            .stroke(stroke(1.0, crate::hud_board::border_colour()))
            .rounding(Rounding::ZERO)
            .inner_margin(Margin::symmetric(6.0, 4.0))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(line)
                        .id(egui::Id::new(CONSOLE_INPUT_ID))
                        .lock_focus(true)
                        .desired_width(width)
                        .font(FontId::proportional(14.0))
                        .frame(false)
                        .hint_text("Type to JAI — English, Deutsch…"),
                )
            })
            .inner;
        if *focus {
            input.request_focus();
            *focus = false;
        }
        let enter = input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let transmit = holo_ui::key_on(ui, "TRANSMIT", true).clicked();
        if enter || transmit {
            let text = line.trim().to_string();
            if !text.is_empty() {
                lock(&CONSOLE_HISTORY).push(&text);
                sent = Some(text);
            }
            // Enter and TRANSMIT alike leave the line focused for the next command.
            *focus = true;
        }
    });
    sent
}

/// How tall the header row of an in-app block is.
const BLOCK_HEADER: f32 = 24.0;

/// The glass of an in-app tool block, opaque (a window of its own cannot be see-through).
fn block_slate() -> Color32 {
    let t = crate::holo::theme();
    crate::holo::mix(t.void(), t.ink(), 0.035)
}
/// The ground of a piece of the board in a window of its own: the canvas itself.
fn window_void() -> Color32 {
    crate::holo::theme().void()
}

/// What the HUD had drawn at each animating window's last frame (`next_animation_frame`).
static HUD_TURNS: Mutex<Vec<(ViewportId, u64)>> = Mutex::new(Vec::new());

/// Asks for an animation's next frame. Windows hands its paint messages to JAI's topmost
/// window first, so a window of its own above the HUD that asked for every next frame at once
/// kept the HUD from drawing at all (2026-11-09: behind a turning 3D model the HUD froze and
/// "dock it back" waited 10 s for its next frame). Such a window skips a beat whenever the HUD
/// has not drawn since its own last frame: the two take turns.
pub(super) fn next_animation_frame(ctx: &egui::Context) {
    let (viewport, hud) = (ctx.viewport_id(), ROOT_FRAMES.load(Ordering::Acquire));
    let waiting = viewport != ViewportId::ROOT && {
        let mut turns = lock(&HUD_TURNS);
        match turns.iter_mut().find(|(v, _)| *v == viewport) {
            Some((_, seen)) => std::mem::replace(seen, hud) == hud,
            None => {
                turns.push((viewport, hud));
                false
            }
        }
    };
    if waiting {
        // (egui takes its predicted frame time off: the next frame comes in about 33 ms.)
        ctx.request_repaint_after(Duration::from_millis(50));
    } else {
        ctx.request_repaint();
    }
}

/// A borderless detached window's frame: the theme's 1 px border, inside the window on the pixel
/// grid (whole at any DPI), and the corner brackets (Windows draws no grey frame of its own
/// around a JAI window). It also plays the window's entrance and exit (`WindowMotion`) over
/// what the window drew, and shows the window, created hidden, once it has drawn a frame.
pub(super) fn paint_window_border(ctx: &egui::Context) {
    crate::live_shots::tick(ctx);
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("jai_window_border")));
    let motion = detached_motion(ctx);
    let band = motion_band(screen, &motion);
    // Nothing of it is pure black but what Windows is to see through (`see_through`): over
    // everything it draws (tool tips too), light a breath strong is added; while it comes or
    // goes, on its frame alone. (The rects reach past the screen: their soft edges stay off it.)
    let lifted = Rect::from_min_max(screen.min - vec2(2.0, 2.0), pos2(screen.max.x + 2.0, if band.bottom() < screen.bottom() { band.bottom() } else { screen.max.y + 2.0 }));
    ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::new("jai_window_lift"))).rect_filled(lifted, Rounding::ZERO, Color32::from_rgba_premultiplied(2, 2, 2, 0));
    if motion.moving() {
        ctx.request_repaint();
        // Below the frame: pure black, seen through, so the frame unfolds out of nothing and
        // folds back into it over whatever stands behind. Inside it, below the scan edge: the
        // slate.
        if band.bottom() < screen.bottom() {
            painter.rect_filled(Rect::from_min_max(pos2(screen.min.x - 2.0, band.bottom()), screen.max + vec2(2.0, 2.0)), Rounding::ZERO, Color32::BLACK);
        }
        let shown = motion_content_clip(screen, &motion);
        let unresolved = Rect::from_min_max(pos2(band.min.x, shown.bottom()), band.max);
        if unresolved.height() > 0.0 {
            painter.rect_filled(unresolved, Rounding::ZERO, block_slate());
        }
        if motion.content < 1.0 && shown.height() > 0.0 {
            painter.rect_filled(shown, Rounding::ZERO, block_slate().gamma_multiply(1.0 - motion.content));
        }
        paint_block_frame(&painter, screen, &motion, false, crate::hud_board::border_colour(), 1.0);
    } else {
        crate::holo::outline(&painter, &crate::holo::plate(screen, [0.0; 4]), crate::hud_board::border_colour());
    }
    paint_window_motion(&painter, screen, &motion, true);
    // Esc in a view he put in full screen ("full screen the Earth") brings its window back.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape) && i.viewport().fullscreen == Some(true)) {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
    }
    esc_closes_window(ctx);
    // (Before it first shows itself: from its first frame on screen its black is seen through.)
    see_through(ctx);
    reveal_when_drawn(ctx);
    keep_above_hud(ctx);
}

/// A window of his own shows what stands behind it where it is pure black, from before it
/// first shows itself on (`win32_desktop::see_through_black`). Its entrance and exit draw out
/// of nothing and fold back into it over whatever is behind. (Sir, 2026-11-19: "when the
/// windows are closing the animation is great but there is a black background".)
fn see_through(ctx: &egui::Context) {
    #[cfg(windows)]
    {
        let viewport = ctx.viewport_id();
        // (Looked for in its first frames only: a window that cannot be found stays as it was.)
        if viewport == ViewportId::ROOT || ctx.cumulative_pass_nr() >= 30 || lock(&DETACHED_MOTIONS).see_through.contains(&viewport) {
            return;
        }
        let Some(title) = ctx.input(|i| i.viewport().title.clone()) else { return };
        if crate::win32::own_window(&title).is_some_and(crate::win32_desktop::see_through_black) {
            lock(&DETACHED_MOTIONS).see_through.insert(viewport);
        }
    }
    #[cfg(not(windows))]
    let _ = ctx;
}

/// A view in a window of its own stays above the HUD it came from: Windows keeps an owned window
/// above its owner, so the fullscreen board (raised by a reply, a click on it, its watchdog)
/// never hides it (2026-11-09: a detached 3D model seemed to close; it was behind the board).
fn keep_above_hud(ctx: &egui::Context) {
    let (viewport, hud) = (ctx.viewport_id(), SHARED.hwnd.load(Ordering::Acquire));
    // (As the desktop's background he owns none of them: an owned window has no key in the
    // taskbar and no place in Alt+Tab. Sir, 2026-11-14: "real windows like Discord".)
    let wanted = !crate::wallpaper::in_use();
    if viewport == ViewportId::ROOT || hud == 0 || lock(&DETACHED_MOTIONS).owned.contains(&viewport) == wanted {
        return;
    }
    if ctx.input(|i| i.viewport().title.clone()).is_some_and(|title| wm::own_window_titled(&title, if wanted { hud } else { 0 })) {
        let mut motions = lock(&DETACHED_MOTIONS);
        if wanted {
            motions.owned.insert(viewport);
        } else {
            motions.owned.remove(&viewport);
        }
    }
}

/// A piece of the board (a block of a report, the column of his system blocks) in a window of
/// its own: no frame of Windows', none of his bars (the piece wears its own header), his
/// icon, and as the desktop's background a key in the taskbar and a place in Alt+Tab. It
/// comes up at `place` (screen points) and is Sir's to move, to size and to put in front of
/// other programs from then on; `body` draws it and may start a drag of the window
/// (`ViewportCommand::StartDrag`). Never where the HUD has no windows to give
/// (`Context::embed_viewports`). (Sir, 2026-11-14: "all windows that Jay opens including the
/// briefing should be real windows like Discord", "not only in-app windows".)
pub(crate) fn piece_window(ctx: &egui::Context, viewport: ViewportId, title: &str, place: Rect, body: impl FnOnce(&mut Ui)) -> bool {
    detached_keep(ctx, viewport, true);
    let builder = egui::ViewportBuilder::default().with_title(title).with_app_id("jai").with_decorations(false).with_position(place.min).with_inner_size(place.size()).with_min_inner_size([200.0, 110.0]);
    let (mut body, mut closed) = (Some(body), false);
    ctx.show_viewport_immediate(viewport, for_test_session(builder), |ctx, _class| {
        // (Closed through Windows itself: Alt+F4, its key in the taskbar.)
        closed |= ctx.input(|i| i.viewport().close_requested());
        let Some(body) = body.take() else { return };
        egui::CentralPanel::default().frame(Frame::none().fill(window_void())).show(ctx, body);
        crate::live_shots::tick(ctx);
        HudApp::resize_grips(ctx);
        esc_closes_window(ctx);
        reveal_when_drawn(ctx);
        // (Over the fullscreen board it must stay above it, as his other windows do.)
        keep_above_hud(ctx);
    });
    closed
}

/// A view of the board's own (the history, the camera): a window of its own of `size` that
/// comes up at `at` and is moved by a drag on its ground (an area on the board in a session
/// without windows). True when Sir closed that window through Windows itself.
fn floating_block(ctx: &egui::Context, id: &str, title: &str, at: Pos2, size: Vec2, body: impl FnOnce(&mut Ui)) -> bool {
    if !pieces_in_windows(ctx) {
        egui::Area::new(egui::Id::new(id)).default_pos(at).order(egui::Order::Foreground).show(ctx, body);
        return false;
    }
    const MARGIN: f32 = 10.0;
    let place = Rect::from_min_size(board_to_screen(ctx, at), size).expand(MARGIN);
    piece_window(ctx, ViewportId::from_hash_of(("jai_floating_block", id)), &format!("{title} \u{2014} J.A.I."), place, |ui| {
        window_ground(ui);
        Frame::none().inner_margin(Margin::same(MARGIN)).show(ui, body);
    })
}

/// The piece's window is gone (its piece left the board): nothing of it counts for a next one.
pub(crate) fn piece_window_gone(viewport: ViewportId) {
    lock(&DETACHED_MOTIONS).forget(viewport);
}

/// Whether the board's pieces stand in windows of their own: wherever the HUD has windows to
/// give (all but an offscreen session).
pub(crate) fn pieces_in_windows(ctx: &egui::Context) -> bool {
    !ctx.embed_viewports()
}

/// Where the point `at` of the board (its own points) is on the screen (points).
pub(crate) fn board_to_screen(ctx: &egui::Context, at: Pos2) -> Pos2 {
    ctx.input(|i| i.viewport().inner_rect).map_or(at, |r| r.min + at.to_vec2())
}

/// A window of JAI's own comes up hidden (`for_test_session`) and shows once it has drawn a
/// frame of its own: Windows never shows its empty client area (a white or black flash) first.
/// Called at the end of each of its frames.
fn reveal_when_drawn(ctx: &egui::Context) {
    let viewport = ctx.viewport_id();
    if viewport == ViewportId::ROOT || ctx.cumulative_pass_nr() < 1 {
        return;
    }
    let stands = ctx.input(|i| i.viewport().inner_rect);
    let mut motions = lock(&DETACHED_MOTIONS);
    if motions.revealed.insert(viewport) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        info!("[HUD] A window of its own shows itself, its first frame drawn ({viewport:?})");
    }
    // Where it stands now: the board ties it to the core with a leader, and draws that anew
    // when the window has moved.
    if let Some(rect) = stands {
        if motions.rects.insert(viewport, rect) != Some(rect) {
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }
}

/// Where his windows stand, in the board's own points: those on the board's screen that are
/// still there, not minimised and not on their way out. (In each window's own points: on a
/// monitor of another scale than the board's a window is not on the board's screen.)
fn own_window_rects(ctx: &egui::Context) -> Vec<Rect> {
    let Some(board) = ctx.input(|i| i.viewport().inner_rect) else { return Vec::new() };
    let windows: std::collections::HashMap<ViewportId, bool> = ctx.input(|i| i.raw.viewports.iter().map(|(id, v)| (*id, v.minimized == Some(true))).collect());
    let mut motions = lock(&DETACHED_MOTIONS);
    // (A window gone without its exit, a widget closed by its key or a text window, left where
    // it stood behind: a leader from the core to nothing, seen by Sir on 2026-11-19.)
    motions.rects.retain(|viewport, _| windows.contains_key(viewport));
    let mut rects: Vec<Rect> = motions.rects.iter().filter(|(viewport, _)| !motions.leaving.contains_key(viewport) && windows.get(viewport) == Some(&false)).map(|(_, rect)| rect.translate(-board.min.to_vec2())).filter(|rect| rect.intersects(Rect::from_min_size(Pos2::ZERO, board.size()))).collect();
    // (In one order every frame: a leader drawn over another would flicker.)
    rects.sort_by(|a, b| a.min.x.total_cmp(&b.min.x).then(a.min.y.total_cmp(&b.min.y)));
    rects
}

/// The top row of a view's own window (`detached_bar`); `id` is the view's, for its lamp. True
/// when Sir closed it (once its exit has played).
pub(super) fn detached_title_bar(ui: &mut Ui, ctx: &egui::Context, id: &str, title: &str) -> bool {
    // In a window of its own it is still one of his views, known to the dock by its title:
    // "close the feature lab" and "close all windows" mean it too (`detached_keep` sees to
    // that from the board's frames; this window's own may be seconds apart). Seen live
    // 2026-11-14: only the views of `block` were known to the dock in their windows; the
    // Feature Lab, the mixer or the theme studio in a window closed by their key and by no
    // word, and as the desktop's background they always stand in one.
    lock(&DETACHED_MOTIONS).named.entry(ctx.viewport_id()).or_insert_with(|| (format!("window:{title}"), title.to_string()));
    detached_bar(ui, ctx, id, title)
}

/// The ground of a window of his own moves it: a drag wherever nothing of its content takes
/// it (its title row, its margins, its empty parts). Called before the content is drawn, which
/// then lies on top of it. (Sir, 2026-11-14: "I want to be able to drag the windows JAI
/// creates"; only the letters of their titles took a drag.)
pub(super) fn window_ground(ui: &Ui) {
    let ground = ui.interact(ui.max_rect(), ui.id().with("window_ground"), Sense::click_and_drag());
    // A double-click on its title row: large and back, as on an in-app block's header.
    let title_row = ground.interact_pointer_pos().is_some_and(|at| at.y < ui.max_rect().top() + WINDOW_TITLE_ROW);
    if !ground.drag_started() && !(ground.double_clicked() && title_row) {
        return;
    }
    // A press on one of the window's keys, sliders or lines of text is theirs: egui gives the
    // drag of a press on a key to the ground beneath it, and the window would run off with
    // the click. (Seen live 2026-11-14: `[ CLOSE ]` did nothing in a window that had the focus.)
    let ctx = ui.ctx();
    let under: Vec<egui::Id> = ctx.interaction_snapshot(|s| s.contains_pointer.iter().copied().collect());
    if under.into_iter().any(|id| id != ground.id && ctx.read_response(id).is_some_and(|r| r.sense.click || r.sense.drag)) {
        return;
    }
    if ground.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    } else {
        let maximized = ctx.input(|i| i.viewport().maximized == Some(true));
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}

/// How far down a window of his own its title row reaches (points).
const WINDOW_TITLE_ROW: f32 = 36.0;

/// Esc in a window of his own closes it, as Esc closes the top in-app block, unless Esc only
/// leaves a line he was typing in (focus as of the last frame: egui drops it on that same Esc)
/// or a full screen (`paint_window_border`).
fn esc_closes_window(ctx: &egui::Context) {
    let typed = egui::Id::new("jai_window_typing").with(ctx.viewport_id());
    let was_typing = ctx.data(|d| d.get_temp::<bool>(typed)).unwrap_or(false);
    let focused = ctx.memory(|m| m.focused().is_some());
    ctx.data_mut(|d| d.insert_temp(typed, focused));
    if ctx.viewport_id() != ViewportId::ROOT && !was_typing && ctx.input(|i| i.key_pressed(egui::Key::Escape) && i.viewport().fullscreen != Some(true)) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

/// The key that keeps this window of his own above other programs' windows, lit while it does
/// (`draw` draws it: lit or not). Only while he is the desktop's background, where his windows
/// are windows among the others'. (His choice of 2026-11-14: "Pin on top".)
fn on_top_key(ui: &mut Ui, draw: impl FnOnce(&mut Ui, bool) -> egui::Response) {
    let Some(title) = ui.ctx().input(|i| i.viewport().title.clone()).filter(|_| crate::wallpaper::in_use()) else { return };
    let on = crate::own_windows::is_pinned(&title);
    if draw(ui, on).on_hover_text("Keep this window above other programs' windows").clicked() {
        wm::run_detached(move || {
            crate::own_windows::pin(&title, !on);
        });
    }
}

/// The start of the top row of a window of his own, as every panel wears it: the index tab
/// in the window's corner (its number among his open windows), the title in wide capitals,
/// cut to leave `keys` of the row free, and the lamp of view `id`.
pub(super) fn bar_title(ui: &mut Ui, ctx: &egui::Context, id: &str, title: &str, keys: f32) {
    let viewport = ctx.viewport_id();
    let number = {
        let mut motions = lock(&DETACHED_MOTIONS);
        motions.opened(viewport, ctx.cumulative_pass_nr());
        motions.barred.insert(viewport);
        motions.number(viewport)
    };
    let theme = crate::holo::theme();
    // The tab stands in the window's own corner, outside the room its content has.
    let shell = ui.painter().with_clip_rect(ctx.screen_rect());
    let digits = crate::holo::galley(&shell, crate::holo::Face::Label, &format!("{number:02}"), 8.5, Color32::BLACK);
    let tab = Rect::from_min_size(ctx.screen_rect().min, vec2(digits.size().x + 16.0, 17.0));
    shell.rect_filled(tab, Rounding::ZERO, theme.accent());
    shell.galley(pos2(tab.left() + 8.0, tab.center().y - digits.size().y * 0.5), digits, Color32::BLACK);
    ui.add_space((tab.right() + 10.0 - ui.cursor().left()).max(0.0));
    // (Its letters are not for marking: a drag on them moves the window like the rest of the
    // row.)
    let lettered = crate::holo_block::fitted_title(ui.painter(), title, ts(14.0), theme.ink(), (ui.available_width() - keys).max(60.0));
    let name = ui.add(egui::Label::new(lettered).selectable(false));
    let time = ctx.input(|i| i.time);
    if crate::holo_block::paint_lamp(ui.painter(), pos2(name.rect.right() + 10.0, name.rect.center().y), crate::holo_block::lamp(id), time, 1.0) {
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

/// The top row of a window of his own: its number, title and lamp (`bar_title`),
/// `[ CLOSE ]`, and as the desktop's background `[ TOP ]`. A drag on the row (on any bare
/// ground of the window) moves the window. True when Sir closed it, once its exit has played.
fn detached_bar(ui: &mut Ui, ctx: &egui::Context, id: &str, title: &str) -> bool {
    let mut clicked = false;
    window_ground(ui);
    let viewport = ctx.viewport_id();
    ui.horizontal(|ui| {
        bar_title(ui, ctx, id, title, 190.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let key = |ui: &mut Ui, text: &str, lit: bool| holo_ui::key_on(ui, text, lit);
            clicked = key(ui, "CLOSE", false).clicked();
            on_top_key(ui, |ui, lit| key(ui, "TOP", lit));
        });
    });
    if clicked && windows_still(ctx) {
        return true;
    }
    let mut motions = lock(&DETACHED_MOTIONS);
    if clicked && !motions.leaving.contains_key(&viewport) {
        motions.leaving.insert(viewport, (Instant::now(), true));
        crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Dismiss);
        ctx.request_repaint();
    }
    // The exit its [ CLOSE ] started has played: now it closes.
    let closed = motions.leaving.get(&viewport).is_some_and(|(at, by_hand)| *by_hand && at.elapsed() >= crate::hud_dock::CLOSE_FADE);
    if closed {
        motions.told.entry(viewport).or_insert_with(Instant::now);
    }
    closed
}

/// The detached windows' entrances and exits, by viewport.
#[derive(Default)]
struct DetachedMotions {
    /// When each came up, its viewport's pass counter (which starts again when the window is
    /// opened anew) and when it last drew (a frame held up moves its entrance on).
    shown: std::collections::HashMap<ViewportId, (u64, Instant, Instant)>,
    /// Since when each is leaving, and whether Sir closed it by hand (its `[ CLOSE ]`).
    leaving: std::collections::HashMap<ViewportId, (Instant, bool)>,
    /// Windows shown since they came up hidden (`reveal_when_drawn`).
    revealed: std::collections::HashSet<ViewportId>,
    /// Windows that exist and are still hidden, as the HUD sees them: its frames since, and
    /// since when (counted from the HUD's frame after the window was made).
    hidden: std::collections::HashMap<ViewportId, (u32, Instant)>,
    /// Windows the HUD owns already (`keep_above_hud`).
    owned: std::collections::HashSet<ViewportId>,
    /// When each shown window was last looked at from outside, and how often it had to be
    /// shown again (`keep_shown`).
    looked: std::collections::HashMap<ViewportId, (Instant, u32)>,
    /// What the dock knows a view with his bar by, in its window: its key there and its title
    /// (`detached_title_bar`).
    named: std::collections::HashMap<ViewportId, (String, String)>,
    /// Since when a window's bar has told its module that Sir closed it (its exit had played).
    told: std::collections::HashMap<ViewportId, Instant>,
    /// Where each stands on the screen (points), as of its last frame: the board draws its
    /// leader to it (`own_window_rects`).
    rects: std::collections::HashMap<ViewportId, Rect>,
    /// The windows that wear his bar: their index tabs number them (`number`).
    barred: std::collections::HashSet<ViewportId>,
    /// The windows whose pure black Windows sees through (`see_through`).
    see_through: std::collections::HashSet<ViewportId>,
}

impl DetachedMotions {
    /// When this showing of the window began (now, if it has just come up: then whatever an
    /// earlier showing left behind is gone).
    fn opened(&mut self, viewport: ViewportId, pass: u64) -> Instant {
        let now = Instant::now();
        let at = match self.shown.get(&viewport) {
            Some(&(seen, at, last)) if seen <= pass => crate::hud_dock::stall_shifted(at, last, now, crate::hud_dock::OPEN_ANIM),
            _ => {
                self.leaving.remove(&viewport);
                self.told.remove(&viewport);
                if pass == 0 {
                    self.revealed.remove(&viewport);
                    self.owned.remove(&viewport);
                    self.see_through.remove(&viewport);
                }
                now
            }
        };
        self.shown.insert(viewport, (pass, at, now));
        at
    }

    /// Whether a window that is leaving and is asked for stays: its module closed it and asks
    /// for it again, or Sir closed it by hand, its bar has told its module so, and it is
    /// still asked for a moment later. (2026-11-14: a simulation closed by hand stayed open in
    /// its lab, and its window stood as the exit's last frame for good, a dark pane with a
    /// line of light. Counted from when its module was told, not from the exit: a window that
    /// draws seldom is told late, and is not to be kept for that.)
    fn stays(&self, viewport: ViewportId, now: Instant) -> bool {
        self.leaving.get(&viewport).is_some_and(|(_, by_hand)| !by_hand || self.told.get(&viewport).is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_millis(400)))
    }

    /// The number on this window's index tab: its place among his open windows with a bar,
    /// in the order they came up, as the sheets of a drawing are numbered.
    fn number(&self, viewport: ViewportId) -> usize {
        let came = |v: &ViewportId| self.shown.get(v).map(|(_, at, _)| *at);
        let Some(mine) = came(&viewport) else { return 1 };
        1 + self.barred.iter().filter(|v| **v != viewport && !self.leaving.contains_key(v) && came(v).is_some_and(|at| at < mine)).count()
    }

    fn forget(&mut self, viewport: ViewportId) {
        self.rects.remove(&viewport);
        self.barred.remove(&viewport);
        self.shown.remove(&viewport);
        self.leaving.remove(&viewport);
        self.revealed.remove(&viewport);
        self.hidden.remove(&viewport);
        self.owned.remove(&viewport);
        self.looked.remove(&viewport);
        self.told.remove(&viewport);
        self.see_through.remove(&viewport);
        if let Some((known_as, _)) = self.named.remove(&viewport) {
            crate::hud_dock::window_gone(&known_as);
        }
    }
}

/// How often a window that has shown itself is looked at from outside.
#[cfg(windows)]
const LOOK_EVERY: Duration = Duration::from_millis(400);

/// From the HUD's frame: a window that has shown itself is looked at from outside a few times
/// a second, and one that Windows calls hidden is shown through Windows itself (and through
/// egui again, so that both agree). The log says so the first times: it names a fault that is
/// papered over here, not cured. (Sir's session of 2026-11-13: three explanation windows
/// existed, had drawn, had "shown themselves", and were hidden.)
fn keep_shown(ctx: &egui::Context, motions: &mut DetachedMotions, viewport: ViewportId) {
    #[cfg(windows)]
    {
        // (Windows hides a minimised board's own windows with it and brings them back with it:
        // that is not this fault.)
        if !motions.revealed.contains(&viewport) || motions.leaving.contains_key(&viewport) || ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
            return;
        }
        let now = Instant::now();
        let (last, _) = motions.looked.entry(viewport).or_insert((now, 0));
        if now.duration_since(*last) < LOOK_EVERY {
            return;
        }
        *last = now;
        let Some(title) = ctx.input(|i| i.raw.viewports.get(&viewport).and_then(|v| v.title.clone())) else { return };
        let ctx = ctx.clone();
        crate::win32::run_detached(move || {
            let Some(hwnd) = crate::win32::own_window(&title) else { return };
            // (It closes with its own exit, not with a snapshot of it another program plays.)
            crate::win32_desktop::own_exit(hwnd);
            if crate::win32::is_visible(hwnd) {
                return;
            }
            crate::win32::show_no_activate(hwnd);
            ctx.send_viewport_cmd_to(viewport, ViewportCommand::Visible(true));
            let mut motions = lock(&DETACHED_MOTIONS);
            // (Closed in the meantime: nothing to count.)
            let Some((_, times)) = motions.looked.get_mut(&viewport) else { return };
            *times += 1;
            if *times <= 3 || times.is_multiple_of(50) {
                warn!("[HUD] '{title}' had shown itself and Windows called it hidden: shown through Windows ({times}×)");
            }
        });
    }
    #[cfg(not(windows))]
    let _ = (ctx, motions, viewport);
}

/// From the HUD's frame: a window that exists and has still not shown itself after a few of the
/// HUD's frames and a third of a second (it never drew while hidden) is shown anyway.
fn reveal_overdue(ctx: &egui::Context, motions: &mut DetachedMotions, viewport: ViewportId) {
    if !ctx.input(|i| i.raw.viewports.contains_key(&viewport)) {
        // No such window (the view is on the board): what an earlier showing left behind must
        // not count for the next one. (A view docked and popped out again stayed hidden for
        // good: it was "revealed" from its first time and never drew while hidden.)
        motions.revealed.remove(&viewport);
        motions.hidden.remove(&viewport);
        motions.owned.remove(&viewport);
        return;
    }
    if motions.revealed.contains(&viewport) {
        return;
    }
    let (frames, since) = motions.hidden.entry(viewport).or_insert((0, Instant::now()));
    *frames += 1;
    if *frames <= 2 {
        // (The window is made after the HUD's frame that asked for it: that wait is no delay.)
        *since = Instant::now();
    } else if since.elapsed() > Duration::from_millis(330) {
        motions.revealed.insert(viewport);
        motions.hidden.remove(&viewport);
        ctx.send_viewport_cmd_to(viewport, ViewportCommand::Visible(true));
        // (It has no frame of its own yet to make the board its owner in: done from here, or
        // the fullscreen board could cover it.)
        let hud = SHARED.hwnd.load(Ordering::Acquire);
        let title = ctx.input(|i| i.raw.viewports.get(&viewport).and_then(|v| v.title.clone()));
        if hud != 0 && !crate::wallpaper::in_use() && title.is_some_and(|title| wm::own_window_titled(&title, hud)) {
            motions.owned.insert(viewport);
        }
        info!("[HUD] A window of its own had not drawn after a third of a second: shown from the board ({viewport:?})");
    }
    ctx.request_repaint_after(Duration::from_millis(60));
}

static DETACHED_MOTIONS: std::sync::LazyLock<Mutex<DetachedMotions>> = std::sync::LazyLock::new(Default::default);

/// Whether a detached window's viewport is shown this frame: while its module has it open, and
/// for its exit once its module closed it (by voice, "close the windows", the tray) while it
/// was up. Call it where `show` returns for a closed window.
pub(super) fn detached_keep(ctx: &egui::Context, viewport: ViewportId, open: bool) -> bool {
    let mut motions = lock(&DETACHED_MOTIONS);
    if open {
        // Asked for again while it was leaving: it stays (and shows again, should Windows'
        // own close have hidden it already).
        if motions.stays(viewport, Instant::now()) {
            if motions.told.remove(&viewport).is_some() {
                info!("[HUD] A window Sir closed is still asked for by its view: it is drawn again ({viewport:?})");
            }
            motions.leaving.remove(&viewport);
            ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(true));
        }
        // A view with his bar is one of the dock's windows for as long as it stands, and
        // closed by word it plays its exit as by its `[ CLOSE ]`, in frames it is given for it.
        if let Some((known_as, title)) = motions.named.get(&viewport).cloned() {
            crate::hud_dock::publish_window(&known_as, &title);
            if crate::hud_dock::closing_progress(&known_as).is_some() && !motions.leaving.contains_key(&viewport) {
                motions.leaving.insert(viewport, (Instant::now(), true));
                crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Dismiss);
                info!("[HUD] '{title}' in its own window closes by word");
            }
        }
        if motions.leaving.contains_key(&viewport) {
            ctx.request_repaint_of(viewport);
        }
        reveal_overdue(ctx, &mut motions, viewport);
        keep_shown(ctx, &mut motions, viewport);
        return true;
    }
    match motions.leaving.get(&viewport).copied() {
        Some((at, _)) if at.elapsed() < crate::hud_dock::CLOSE_FADE => {
            ctx.request_repaint();
            true
        }
        Some(_) => {
            motions.forget(viewport);
            false
        }
        // (Its viewport is still there: it was up in the last frame.)
        None if ctx.cumulative_pass_nr_for(viewport) > 0 && motions.shown.contains_key(&viewport) && !windows_still(ctx) => {
            motions.leaving.insert(viewport, (Instant::now(), false));
            info!("[HUD] A window of its own plays its exit ({viewport:?})");
            ctx.request_repaint();
            true
        }
        None => {
            motions.forget(viewport);
            false
        }
    }
}

/// Whether this detached window is playing its exit (its content is drawn on until it ends).
pub(super) fn detached_leaving(ctx: &egui::Context) -> bool {
    lock(&DETACHED_MOTIONS).leaving.get(&ctx.viewport_id()).is_some_and(|(at, _)| at.elapsed() < crate::hud_dock::CLOSE_FADE)
}

/// This detached window's entrance or exit right now.
fn detached_motion(ctx: &egui::Context) -> WindowMotion {
    if windows_still(ctx) {
        return WindowMotion::REST;
    }
    let viewport = ctx.viewport_id();
    let mut motions = lock(&DETACHED_MOTIONS);
    let opened = motions.opened(viewport, ctx.cumulative_pass_nr());
    let opening = opened.elapsed().as_secs_f32() / crate::hud_dock::OPEN_ANIM.as_secs_f32();
    let closing = motions.leaving.get(&viewport).map(|(at, _)| (at.elapsed().as_secs_f32() / crate::hud_dock::CLOSE_FADE.as_secs_f32()).min(1.0));
    WindowMotion::of((opening < 1.0).then_some(opening), closing)
}

/// A view as a block on the board: what a session without windows of its own to give draws
/// (a picture, a test, the headless verifier) where the real HUD opens a window. The panel's
/// plate with its index tab, title, lamp and keys; movable and resizable on the board; its
/// rect is published so the emblem knows whether the centre is covered
/// (`hud_dock::block_rects`).
pub(super) fn hud_block_window(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], body: impl FnOnce(&mut Ui)) -> BlockAction {
    let mut action = BlockAction::default();
    // Closing ("close the feature lab", its [ CLOSE ], the menu) or minimising
    // ("minimise it"): the block plays its exit, then leaves the board that way.
    let fade = crate::hud_dock::closing_progress(id);
    let tray = crate::hud_dock::is_minimized(id);
    if fade.is_some_and(|t| t >= 1.0) || (tray && fade.is_some()) {
        // Off the board from this frame on (the emblem stops making room for it at once).
        crate::hud_dock::off_board(id);
        if !crate::hud_dock::take_minimizing(id, title) && (tray || !crate::hud_dock::park(id, title)) {
            // (Closed from the tray, or for real: gone. Otherwise it waits in the tray, dim.)
            action.close = true;
        }
        return action;
    }
    // Put away in the tray: nothing on the board until it is restored (with its entrance). A
    // closed one waits there for ten minutes, then closes for real.
    if tray {
        action.close = crate::hud_dock::parked_out(id);
        return action;
    }
    // Opening: the entrance. (Offscreen renders draw every block at rest, as before.)
    let still = windows_still(ctx);
    let motion = if still {
        let alpha = 1.0 - fade.unwrap_or(0.0);
        WindowMotion { frame: alpha, content: alpha, ..WindowMotion::REST }
    } else {
        WindowMotion::of(if fade.is_some() { None } else { crate::hud_dock::opening_progress(id) }, fade)
    };
    if fade.is_some() || motion.moving() {
        ctx.request_repaint();
    }
    // The block's screen: on a spanned HUD the monitor it stands on (the one under the pointer
    // while Sir drags it across), so no block straddles a bezel or sits where no screen shows.
    let regions = block_regions(ctx);
    let dragged = crate::hud_dock::grabbed(id).and_then(|_| ctx.input(|i| i.pointer.primary_down().then(|| i.pointer.interact_pos()).flatten()));
    let anchor = dragged.or_else(|| crate::hud_dock::block_rect(id).map(|r| r.center()));
    let mut bounds = anchor.map_or(regions[0], |at| region_at(&regions, at));
    // Full screen ("full screen the Earth"): the block fills the home screen, above everything.
    // On its way there or back (and on any glide) it may cross its monitor's part.
    let (full, gliding) = (crate::hud_dock::is_fullscreen(id), crate::hud_dock::is_gliding(id));
    let home = home_screen(ctx);
    let size = vec2(size[0].min(bounds.width() - 16.0).max(320.0), size[1].min(bounds.height() - 16.0).max(220.0));
    let area = egui::Id::new(("hud_block", id));
    // Where the dock holds the block for now: its first slot, a cell of the tidy grid, the
    // maximised rect. Otherwise it stays wherever Sir dragged it.
    // (A block that reopens comes back where it was, unless something has taken that spot. A
    // block whose place is no longer on the board, because the HUD moved to a smaller or an
    // upright screen, is placed afresh: pushed back inside by egui alone it would land on
    // whatever is at the edge.)
    // The dock thinks in whole blocks (frame included); egui sizes a window by its content.
    let frame_pad = Vec2::splat(2.0 * BLOCK_MARGIN);
    // (Only where a block fits the board at all: in a window smaller than one block there is
    // no place to give.)
    let fits = bounds.width() >= crate::hud_dock::MIN_BLOCK[0] + 20.0 && bounds.height() >= crate::hud_dock::MIN_BLOCK[1] + 20.0;
    // In Sir's hand (or gliding on from his throw) a block belongs to no one screen: it crosses
    // from one monitor's part to the next as his hand moves, and is neither placed afresh nor
    // pushed back inside on the way. (Until 2026-11-13 it jumped to the other monitor the
    // moment the pointer crossed the bezel. Sir: "I should be able to move JAI's own windows
    // between monitors smoothly".)
    let in_hand = dragged.is_some() || lock(&THROWN).contains_key(id);
    let fresh = !in_hand && ctx.memory(|m| m.area_rect(area)).is_none_or(|old| (fits && !bounds.expand(2.0).contains_rect(old)) || (!crate::hud_dock::is_open(id) && crate::hud_dock::collides(id, old)));
    let held = crate::hud_dock::take_forced(id).or_else(|| full.then_some(home)).or_else(|| fresh.then(|| crate::block_places::remembered(id, &regions).unwrap_or_else(|| new_block_rect(ctx, id, &regions, size + frame_pad))));
    // A place the dock gave on another monitor makes that monitor the block's screen.
    if let Some(r) = held {
        bounds = region_at(&regions, r.center());
    }
    let constrain = if full || gliding || in_hand { ctx.screen_rect() } else { bounds };
    if full {
        bounds = home;
    }
    let (at, size) = held.map_or((bounds.min, size), |r| (r.min, (r.size() - frame_pad).max(Vec2::splat(40.0))));
    // The search across the blocks: a block that answers it is lit, the others step back.
    let needle = crate::hud_dock::search();
    let found = !needle.is_empty() && crate::hud_dock::search_matches(&needle, title, &crate::hud_dock::block_content(id).unwrap_or_default());
    let dim = if needle.is_empty() || found { 1.0 } else { SEARCH_DIM };
    // The pointer resting on another block: this one steps back a little (to 85 %).
    let mine = ctx.memory(|m| m.area_rect(area));
    let elsewhere = !still && ctx.pointer_hover_pos().is_some_and(|at| !mine.is_some_and(|r| r.contains(at)) && crate::hud_dock::block_rects().iter().any(|b| b.contains(at)));
    let dim = dim * (1.0 - 0.15 * ctx.animate_bool_with_time(area.with("others_used"), elsewhere, crate::holo_motion::LEAVE));
    let content_alpha = motion.content * dim;
    let (pinned, maximized) = (crate::hud_dock::is_pinned(id), crate::hud_dock::is_maximized(id));
    let edge = if found { crate::hud_board::active_colour() } else { crate::hud_board::border_colour() };
    // The frame is drawn by the block itself (`paint_block_frame`): on the band its motion has
    // unfolded to, its border inside its rect.
    let frame = Frame::none().rounding(Rounding::ZERO).inner_margin(Margin::same(BLOCK_MARGIN));
    let window = egui::Window::new(title)
        .id(area)
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .frame(frame)
        .fade_in(false)
        .fade_out(false)
        .order(if full { egui::Order::Foreground } else { egui::Order::Middle })
        .default_pos(at)
        .default_size(size)
        .constrain_to(constrain);
    let window = match held {
        Some(r) => window.current_pos(r.min).min_size(size).max_size(size),
        None => window.min_size(Vec2::from(crate::hud_dock::MIN_BLOCK) - frame_pad),
    };
    let mut close_asked = false;
    let mut header_maximize = false;
    let mut header_menu = None;
    let mut natural = None;
    let shown = window.show(ctx, |ui| {
        let frame_rect = ui.max_rect().expand(BLOCK_MARGIN);
        // The block under the pointer wears its frame in the accent, and the core lights the
        // edge that faces it.
        let using = ctx.animate_bool_with_time(area.with("using"), !still && ctx.pointer_hover_pos().is_some_and(|at| frame_rect.contains(at)), crate::holo_motion::TOUCH);
        let accent = crate::hud_board::active_colour();
        // (`with_clip_rect` only ever narrows a clip: the shell's painter gets its own.)
        let mut shell = ui.painter().clone();
        // (The window clips what its content draws to the content's own room, a margin inside
        // the frame: painted with that, the frame's border and the outer strip of its surface
        // never showed, and a block was brackets round a dark patch. The shell is painted on the
        // whole of the block's rect.)
        shell.set_clip_rect(frame_rect);
        paint_block_frame(&shell, frame_rect, &motion, true, mix(edge, accent, using).gamma_multiply(dim), if found { 2.0 } else { 1.0 });
        if !motion.moving() {
            if let Some(orb) = crate::hud_dock::orb() {
                crate::holo_block::core_light(&shell, frame_rect, orb, accent, dim);
            }
        }
        // Only what the frame has unfolded to, down to the scan edge, shows (and takes clicks).
        if motion.moving() {
            let clip = ui.clip_rect().intersect(motion_content_clip(frame_rect, &motion));
            ui.set_clip_rect(clip);
        }
        if content_alpha < 1.0 {
            ui.multiply_opacity(content_alpha);
        }
        // The block keeps the size it was given (or dragged to), however short its content.
        ui.set_min_size(ui.available_size());
        // The header every block wears: its title and state light on the left, its keys on the
        // right. A drag that begins on the title moves the block.
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), BLOCK_HEADER), Sense::hover());
        let mid = row.center().y;
        // (Its tooltip names the words that close it: blocks answer to their name.)
        let name = title.split(" // ").next().unwrap_or(title).trim().to_lowercase();
        let close_hint = format!("Close · say: \"close the {name}\"");
        let pin_hint = format!("Pinned, it stays through \"close the windows\" and keeps its place (say \"pin the {name}\")");
        let labels = [("CLOSE", false, close_hint.as_str()), ("PIN", pinned, pin_hint.as_str())];
        // (A narrow block keeps its title whole and lets its keys go, [ PIN ] first: the menu
        // still pins. The keys are quiet until the pointer is over their block.)
        // (Beside the title the row holds the index tab before it and the lamp after it.)
        let title_width = crate::holo_block::title_galley(ui.painter(), title, ts(14.0), accent, f32::INFINITY).size().x;
        let fit = crate::holo_ui::keys_that_fit(ui.painter(), &labels, row.width(), title_width + 62.0);
        let (key, keys_left) = crate::holo_ui::keys_at(ui, area.with("header"), row.right(), mid, &labels[..fit], content_alpha * (0.62 + 0.38 * using));
        match key {
            Some(0) => close_asked = true,
            Some(1) => {
                crate::hud_dock::toggle_pin(id);
            }
            _ => {}
        }
        // Its index tab: every block on the board is numbered in the order it was opened, as
        // on a drawing. A solid block of the accent in the frame's corner, the number in black.
        let theme = crate::holo::theme();
        let number = crate::hud_dock::open_blocks().iter().position(|(open, _)| open == id).map_or(String::new(), |k| format!("{:02}", k + 1));
        let tab = {
            let digits = crate::holo::galley(&shell, crate::holo::Face::Label, &number, 8.5, Color32::BLACK);
            let tab = Rect::from_min_size(frame_rect.min, vec2(digits.size().x + 16.0, 17.0));
            if !number.is_empty() && motion.unfold >= 1.0 {
                shell.rect_filled(tab, Rounding::ZERO, mix(theme.accent(), theme.hot(), 0.3 * using).gamma_multiply(content_alpha));
                shell.galley(pos2(tab.left() + 8.0, tab.center().y - digits.size().y * 0.5), digits, Color32::BLACK);
            }
            tab
        };
        let title_left = row.left().max(tab.right() + 10.0);
        let title_room = (keys_left - title_left - 22.0).max(20.0);
        // A title cut short shows whole under the pointer.
        let cut = title_width > title_room + 0.5;
        // The title decodes itself as the block opens, and back as it leaves.
        let lettered = crate::holo_block::fitted_title(ui.painter(), &crate::holo_motion::decode(title, motion.resolve.min(motion.content), ctx.input(|i| i.time)), ts(14.0), theme.ink().gamma_multiply(content_alpha), title_room);
        let title_rect = Rect::from_min_size(pos2(title_left, mid - lettered.size().y * 0.5), lettered.size());
        ui.painter().galley(title_rect.min, lettered, Color32::PLACEHOLDER);
        if crate::holo_block::paint_lamp(ui.painter(), pos2(title_rect.right() + 10.0, mid), crate::holo_block::lamp(id), ctx.input(|i| i.time), content_alpha) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        // (A click-only sense: a drag that begins here still moves the block.)
        let header = ui
            .interact(Rect::from_min_max(row.min, pos2(keys_left - 4.0, row.bottom())), area.with("title"), Sense::click())
            .on_hover_text(format!("{}Drag to move, drag an edge to resize, double-click for large and back, fling it away to close. Right-click: copy, pin, maximize, export, close. Esc closes it.", if cut { format!("{title}\n\n") } else { String::new() }));
        if header.double_clicked() {
            header_maximize = true;
        }
        // (The title row takes the right click itself: the block's menu hangs on it too, or a
        // right click on the title opened nothing.)
        header.context_menu(|ui| header_menu = crate::block_actions::menu(ui, pinned, maximized));
        if crate::hud_dock::take_esc_close(id) {
            close_asked = true;
        }
        // The header's rule, from edge to edge of the frame, the accent under the tab.
        let r = ui.cursor();
        crate::holo::rule(&shell, pos2(frame_rect.left(), r.top()), pos2(frame_rect.right(), r.top()), crate::holo::a(theme.ink(), 0.15 * content_alpha));
        if !number.is_empty() {
            crate::holo::rule(&shell, pos2(frame_rect.left(), r.top()), pos2(tab.right() + 26.0, r.top()), crate::holo::a(theme.accent(), 0.9 * content_alpha));
        }
        ui.add_space(6.0);
        // The content lives in the block's size and never grows it: egui widens a window to its
        // last content every frame, so a body a few pixels taller than its room made the block
        // creep down the screen until it left it and was placed afresh, over and over.
        let room = ui.available_rect_before_wrap();
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(room));
        content.set_clip_rect(content.clip_rect().intersect(frame_rect));
        body(&mut content);
        // What the body really took: less than its room (a short list, a few lines) or more.
        natural = Some(block_natural_size(frame_rect, room, content.min_rect()));
        ui.advance_cursor_after_rect(room);
    });
    if let Some(inner) = shown {
        let rect = inner.response.rect;
        // The corner brackets (leading the frame out of the middle on the way in), the hairline,
        // the scan edge, the settling glow and the closing point's flash; the block itself is
        // never scaled (its text never lays out again while it comes or goes).
        let painter = ctx.layer_painter(inner.response.layer_id);
        paint_window_motion(&painter, rect, &motion, false);
        if !motion.moving() {
            paint_resize_grip(&painter, rect, edge.gamma_multiply(0.8 * dim));
        }
        if crate::hud_dock::publish_block(id, title, rect) {
            crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Materialize);
        }
        // Where it stands at rest is where it comes back to, after a restart too.
        if !motion.moving() && !full && !maximized && !gliding && !ctx.input(|i| i.pointer.primary_down()) {
            crate::block_places::note(id, rect);
        }
        // Moved with the mouse, a block stays exactly under the pointer: where it stood when
        // the button went down plus the whole way the pointer has come since. (egui's own
        // window move begins a few pixels late and rounds every frame.) A resize or a drag
        // inside the block's content is left alone: only a block that egui is moving is taken.
        let (pressed, down, released, origin, at) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.press_origin(), i.pointer.interact_pos()));
        if pressed {
            if let Some(origin) = origin.filter(|o| rect.contains(*o)) {
                crate::hud_dock::grab(id, rect, origin);
                // Sir took hold of it: "it" means this block now.
                crate::hud_dock::touch(id);
            }
        } else if let Some((home, origin)) = crate::hud_dock::grabbed(id).filter(|_| down || released) {
            let moved = (rect.min - home.min).length() > 0.5 && (rect.size() - home.size()).length() <= 0.5;
            if let Some(at) = at.filter(|_| moved) {
                // It goes where the hand has it, and nowhere else. (Until 2026-11-13 it lined
                // itself up with its neighbours, the emblem and the screen, and drew the lines
                // it had snapped to. Sir: "I don't want that feature that adds help to align
                // the windows".)
                let landed = Rect::from_min_size(home.min + (at - origin), home.size());
                let velocity = ctx.input(|i| i.pointer.velocity());
                crate::hud_dock::hold_once(id, landed);
                // Let go while it moved: it is thrown, and glides on a little; flung away, it
                // closes (and waits in the tray, so a slip of the hand loses nothing).
                if released && !still && velocity.length() >= crate::holo_block::FLING_AWAY && !crate::hud_dock::is_pinned(id) {
                    crate::hud_dock::begin_close(id);
                } else if released && !still && velocity.length() >= crate::holo_block::THROW_FROM {
                    lock(&THROWN).insert(id.to_string(), (landed, velocity));
                } else if released && !still {
                    // Let go across a bezel (or where no screen shows): it glides the rest of
                    // the way onto the screen its middle is on, it does not jump there.
                    let screen = region_at(&regions, landed.center());
                    let inside = moved_inside(landed, screen);
                    if (inside.min - landed.min).length() > 1.0 || (inside.size() - landed.size()).length() > 1.0 {
                        crate::hud_dock::glide_to(id, landed, inside);
                    }
                }
                ctx.request_repaint();
            }
        }
        if !down {
            crate::hud_dock::let_go(id);
            // A thrown block glides on until it rests (or is taken hold of again: `pressed`).
            let mut thrown = lock(&THROWN);
            if let Some((at, velocity)) = thrown.get(id).copied() {
                match crate::holo_block::glide_on(at, velocity, bounds, ctx.input(|i| i.stable_dt)) {
                    Some((next, v)) => {
                        thrown.insert(id.to_string(), (next, v));
                        crate::hud_dock::hold_once(id, next);
                        ctx.request_repaint();
                    }
                    None => {
                        thrown.remove(id);
                    }
                }
            }
        } else if pressed {
            lock(&THROWN).remove(id);
        }
        if header_maximize && crate::hud_dock::toggle_maximize(id, bounds) {
            crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Spotlight);
        }
        // An edge dragged by hand: the block keeps Sir's size. Otherwise it takes the size of
        // what it shows (once per content, gliding), like the blocks of the reference video.
        if down && crate::hud_dock::grabbed(id).is_some_and(|(home, _)| (rect.size() - home.size()).length() > 2.0) {
            crate::hud_dock::sized_by_hand(id);
        } else if let Some(natural) = natural.filter(|_| held.is_none() && !full && !motion.moving()) {
            crate::hud_dock::fit_to_content(id, natural, bounds, still);
        }
        // Right-click: the block menu.
        let mut chosen = header_menu;
        inner.response.context_menu(|ui| chosen = chosen.or(crate::block_actions::menu(ui, pinned, maximized)));
        if let Some(chosen) = chosen {
            use crate::block_actions::{BlockAction as Menu, Format};
            let text = || crate::hud_dock::block_content(id).unwrap_or_else(|| title.to_string());
            let export = |format| match crate::block_actions::export(title, &text(), None, format) {
                Ok(path) => info!("[HUD] Block '{title}' exported to {}", path.display()),
                Err(e) => warn!("[HUD] Could not export '{title}': {e}"),
            };
            match chosen {
                Menu::Copy => crate::block_actions::copy(ctx, &text()),
                Menu::Pin => {
                    crate::hud_dock::toggle_pin(id);
                }
                Menu::Maximize => {
                    if crate::hud_dock::toggle_maximize(id, bounds) {
                        crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Spotlight);
                    }
                }
                Menu::ExportMarkdown => export(Format::Markdown),
                Menu::ExportJson => export(Format::Json),
                Menu::Close => close_asked = true,
            }
            ctx.request_repaint();
        }
    }
    // Closed by hand (the chip, the menu): the block leaves with its cue, after its exit
    // (offscreen, at once).
    if close_asked {
        crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Dismiss);
        if still {
            action.close = true;
        } else {
            crate::hud_dock::begin_close(id);
            ctx.request_repaint();
        }
    }
    action
}

/// Blocks Sir threw: where each is and how fast it still glides (`holo_block::glide_on`).
static THROWN: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (Rect, Vec2)>>> = std::sync::LazyLock::new(Default::default);

/// The egui-data key that marks a context whose windows are drawn at rest (offscreen renders:
/// `ui_snapshot`, the visual verifier): no entrance or exit moves them while they are measured.
pub(crate) const STILL_WINDOWS: &str = "jai_still_windows";

/// Whether this context draws its windows at rest (`STILL_WINDOWS`), or Sir turned the
/// interface's animations off (`REDUCED_MOTION`): then windows come and go at once.
fn windows_still(ctx: &egui::Context) -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed) || ctx.data(|d| d.get_temp::<bool>(egui::Id::new(STILL_WINDOWS))).unwrap_or(false)
}

/// Settings → Animations off: windows, cards and blocks open and close without their motion
/// (set from the interface settings every frame).
static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Whether windows and cards play their motion (false with the animations off).
pub(crate) fn motion_enabled() -> bool {
    !REDUCED_MOTION.load(Ordering::Relaxed)
}

/// Draws this context's windows at rest (`true`) or with their entrance and exit.
pub(crate) fn set_still(ctx: &egui::Context, still: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(STILL_WINDOWS), still));
}

/// One frame of a window's entrance (`opening` 0 … 1 of `hud_dock::OPEN_ANIM`) or exit
/// (`closing` 0 … 1 of `hud_dock::CLOSE_FADE`): one motion language for every block, detached
/// window, pop-up card and board block. A window is drawn, not shown.
///
/// In: a spark at its top left corner throws a line of light along its top edge (0–130 ms,
/// fast first); the frame unfolds downwards from that line (110–330 ms) with the corner ticks
/// riding the travelling edge, white-hot and cooling; the content resolves top to bottom behind
/// a scan line (280–420 ms); one glow settles on the frame (to 440 ms).
/// Out, quicker: the content goes (0–80 ms), the frame folds up into the top line (50–170 ms),
/// the line runs back into its corner (160–235 ms) and the corner sparks once.
///
/// Nothing is scaled: the frame and the content are clipped and faded, so text never lays out
/// again while a window comes or goes, and its neighbours never move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowMotion {
    /// The frame's height as a share of the window's, from its top edge (1: whole).
    pub unfold: f32,
    /// The line along the top edge: its width share from the left corner and its brightness.
    pub line: Option<(f32, f32)>,
    /// How far down the content has resolved (0 top … 1: all of it).
    pub resolve: f32,
    /// The frame's fill and border.
    pub frame: f32,
    /// The content's opacity (title row included).
    pub content: f32,
    /// How far down the lower corner ticks have travelled, as a share of the window's height
    /// (1: the corners).
    pub brackets: f32,
    pub brackets_alpha: f32,
    /// The glow settling on the frame at the end of an entrance (0 … 1).
    pub glow: f32,
    /// The spark at the top left corner: as the line leaves it, and as it returns (0 … 1).
    pub flash: f32,
}

/// The entrance's beats, in ms of `hud_dock::OPEN_ANIM` (440).
const IN_LINE: (f32, f32) = (0.0, 130.0);
const IN_UNFOLD: (f32, f32) = (110.0, 330.0);
const IN_BRACKETS: (f32, f32) = (100.0, 310.0);
const IN_RESOLVE: (f32, f32) = (280.0, 420.0);
const IN_SETTLE: (f32, f32) = (300.0, 440.0);
/// The exit's beats, in ms of `hud_dock::CLOSE_FADE` (240).
const OUT_CONTENT: (f32, f32) = (0.0, 80.0);
const OUT_FOLD: (f32, f32) = (50.0, 170.0);
const OUT_BRACKETS: (f32, f32) = (40.0, 160.0);
const OUT_CONTRACT: (f32, f32) = (160.0, 235.0);
const OUT_FLASH: f32 = 222.0;

/// 0 before `beat`, 1 after it, linear between.
fn beat(ms: f32, (from, to): (f32, f32)) -> f32 {
    ((ms - from) / (to - from)).clamp(0.0, 1.0)
}

use crate::holo_motion::{ease_in_cubic, ease_in_out as ease_in_out_cubic, ease_out_expo};

impl WindowMotion {
    pub const REST: Self = Self { unfold: 1.0, line: None, resolve: 1.0, frame: 1.0, content: 1.0, brackets: 1.0, brackets_alpha: 1.0, glow: 0.0, flash: 0.0 };

    pub fn of(opening: Option<f32>, closing: Option<f32>) -> Self {
        match (opening, closing) {
            (_, Some(t)) => Self::exit(t),
            (Some(t), None) => Self::entrance(t),
            (None, None) => Self::REST,
        }
    }

    fn entrance(t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        if t >= 1.0 {
            return Self::REST;
        }
        let ms = t * crate::hud_dock::OPEN_ANIM.as_secs_f32() * 1000.0;
        let unfold = ease_out_expo(beat(ms, IN_UNFOLD));
        // The line is the frame's top edge once the frame stands; it fades into it.
        let line_alpha = 1.0 - smooth(IN_UNFOLD.0 + 60.0, IN_UNFOLD.1, ms);
        Self {
            unfold,
            line: (line_alpha > 0.0).then(|| (ease_out_expo(beat(ms, IN_LINE)), line_alpha)),
            resolve: ease_in_out_cubic(beat(ms, IN_RESOLVE)),
            frame: smooth(IN_UNFOLD.0 - 10.0, IN_UNFOLD.0 + 50.0, ms),
            content: 1.0,
            brackets: ease_out_expo(beat(ms, IN_BRACKETS)),
            brackets_alpha: smooth(IN_BRACKETS.0, IN_BRACKETS.0 + 50.0, ms),
            glow: smooth(IN_SETTLE.0 - 60.0, IN_SETTLE.0, ms) * (1.0 - smooth(IN_SETTLE.0, IN_SETTLE.1, ms)),
            flash: 1.0 - smooth(0.0, IN_LINE.1 * 0.8, ms),
        }
    }

    fn exit(t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let ms = t * crate::hud_dock::CLOSE_FADE.as_secs_f32() * 1000.0;
        let unfold = 1.0 - ease_in_cubic(beat(ms, OUT_FOLD));
        let width = 1.0 - ease_in_cubic(beat(ms, OUT_CONTRACT));
        // The line shows once the frame has folded into it, until it is back in its corner.
        let line_alpha = smooth(OUT_FOLD.1 - 40.0, OUT_FOLD.1, ms);
        Self {
            unfold,
            line: (line_alpha > 0.0 && width > 0.0).then_some((width, line_alpha)),
            resolve: 1.0,
            frame: 1.0 - smooth(OUT_FOLD.1 - 30.0, OUT_FOLD.1 + 10.0, ms),
            content: 1.0 - smooth(OUT_CONTENT.0, OUT_CONTENT.1, ms),
            brackets: 1.0 - ease_in_cubic(beat(ms, OUT_BRACKETS)),
            brackets_alpha: 1.0 - smooth(OUT_BRACKETS.0 + 40.0, OUT_BRACKETS.1 + 10.0, ms),
            glow: 0.0,
            flash: (1.0 - (ms - OUT_FLASH).abs() / 28.0).max(0.0),
        }
    }

    /// Anything of the entrance or exit still to draw.
    pub fn moving(&self) -> bool {
        *self != Self::REST
    }
}

/// Where a window's frame stands in this frame of its motion: its rect unfolded from its top
/// edge.
pub(crate) fn motion_band(rect: Rect, m: &WindowMotion) -> Rect {
    if m.unfold >= 1.0 {
        return rect;
    }
    Rect::from_min_size(rect.min, vec2(rect.width(), rect.height() * m.unfold.max(0.0)))
}

/// What of a window's content shows in this frame of its motion: its frame, down to the edge
/// the content has resolved to.
pub(crate) fn motion_content_clip(rect: Rect, m: &WindowMotion) -> Rect {
    let band = motion_band(rect, m);
    if m.resolve >= 1.0 {
        return band;
    }
    Rect::from_min_max(band.min, pos2(band.max.x, band.top() + band.height() * m.resolve.max(0.0)))
}

/// A block's plate in this frame of its motion, on the band the frame has unfolded to: glass
/// lit from above (`glass`; not for a window whose ground is its own), a hairline frame with
/// one corner cut, ticks at the other three. The frame runs white-hot while it travels and
/// cools as it lands; `width` 2 (the search lights the block) doubles its line.
fn paint_block_frame(painter: &egui::Painter, rect: Rect, m: &WindowMotion, glass: bool, edge: Color32, width: f32) {
    let band = motion_band(rect, m);
    if band.height() < 0.5 || m.frame <= 0.0 {
        return;
    }
    let t = holo::theme();
    let heat = (1.0 - m.unfold).clamp(0.0, 1.0);
    let edge = mix(edge, t.hot(), heat * 0.8).gamma_multiply(m.frame.max(heat));
    let cut = crate::holo_frame::CUT.min(band.height() / 2.0);
    let shape = holo::plate(band, [0.0, 0.0, cut, 0.0]);
    if glass {
        crate::holo_frame::glass(painter, &shape, m.frame);
    }
    holo::outline(painter, &shape, edge);
    if width > 1.0 {
        holo::outline(painter, &holo::plate(band.shrink(1.0), [0.0, 0.0, cut, 0.0]), edge);
    }
    let bright = holo::a(mix(t.ink(), t.hot(), heat), 0.85 * m.frame);
    let arm = crate::holo_frame::TICK.min(band.width() / 3.0).min(band.height() / 3.0);
    for (at, dx, dy) in [(band.left_top(), 1.0, 1.0), (band.right_top(), -1.0, 1.0), (band.left_bottom(), 1.0, -1.0)] {
        holo::path(painter, &[pos2(at.x + dx * arm, at.y), at, pos2(at.x, at.y + dy * arm)], bright);
    }
    holo::path(painter, &[pos2(band.right(), band.bottom() - cut), pos2(band.right() - cut, band.bottom())], bright);
}

/// The mark in a resizable block's cut corner: one short line along the cut, inside it.
fn paint_resize_grip(painter: &egui::Painter, rect: Rect, colour: Color32) {
    let cut = crate::holo_frame::CUT;
    holo::path(painter, &[pos2(rect.right() - 5.0, rect.bottom() - cut - 1.0), pos2(rect.right() - cut - 1.0, rect.bottom() - 5.0)], colour);
}

/// 0 below `from`, 1 above `to`, a smooth S between.
fn smooth(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// What a window's entrance or exit draws over it (the line, the scan, the settling glow, the
/// corner's spark), and its corner ticks (at rest: where they always are). The scan fades the
/// freshly resolved content up out of the window's glass. In a window of his own (`crisp`) the
/// black around the frame is seen through, so what is drawn there has no light round it, as
/// the corner orb has none: soft light on a black that is seen through stands round it as a
/// dark veil.
pub(super) fn paint_window_motion(painter: &egui::Painter, rect: Rect, m: &WindowMotion, crisp: bool) {
    use crate::holo::{a, lit};
    let t = crate::holo::theme();
    let (accent, hot, fill) = (t.accent(), t.hot(), block_slate());
    let px = crate::holo::px(painter);
    // (The light round a line stays on the frame's glass there.)
    let glass = if crisp { painter.with_clip_rect(motion_band(rect, m).intersect(painter.clip_rect())) } else { painter.clone() };
    let mut glow = crate::holo::Light::new(&glass);
    let mut light = crate::holo::Light::new(painter);
    if m.brackets_alpha > 0.0 && m.brackets > 0.0 {
        let r = Rect::from_min_size(rect.min, vec2(rect.width(), rect.height() * m.brackets)).shrink(0.5);
        // White-hot while they travel, ink once they are home.
        let travel = (1.0 - m.brackets).abs().min(1.0);
        crate::hud_board::brackets(painter, r, mix(crate::hud_board::bracket_colour().gamma_multiply(0.85), hot, travel).gamma_multiply(m.brackets_alpha));
    }
    if let Some((width, strength)) = m.line {
        let from = pos2(rect.left(), rect.top() + px * 0.5);
        let to = pos2(rect.left() + (rect.width() * width).max(px), from.y);
        light.neon(&[from, to], false, px.max(1.2), a(hot, strength), if crisp { 0.0 } else { strength });
        if width < 0.995 {
            if crisp {
                painter.circle_filled(to, 1.6, a(hot, strength));
            } else {
                light.spark(to, 7.0, lit(hot, strength));
            }
        }
    }
    if m.resolve > 0.0 && m.resolve < 1.0 {
        // The scan: the content above it fades up out of the glass, light trails it.
        let band = motion_band(rect, m);
        let inner = band.shrink2(vec2(1.0, 0.0));
        let y = painter.round_to_pixel(band.top() + band.height() * m.resolve);
        let fade = 34.0f32.min(y - band.top());
        if fade > 1.0 {
            let mut mesh = egui::Mesh::default();
            let above = Rect::from_min_max(pos2(inner.left(), y - fade), pos2(inner.right(), y));
            let (clear, glass) = (Color32::TRANSPARENT, fill.gamma_multiply(0.92));
            for (colour_top, colour_bottom) in [(clear, glass), (clear, lit(accent, 0.20))] {
                let base = mesh.vertices.len() as u32;
                mesh.colored_vertex(above.left_top(), colour_top);
                mesh.colored_vertex(above.right_top(), colour_top);
                mesh.colored_vertex(above.right_bottom(), colour_bottom);
                mesh.colored_vertex(above.left_bottom(), colour_bottom);
                mesh.add_triangle(base, base + 1, base + 2);
                mesh.add_triangle(base, base + 2, base + 3);
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        glow.neon(&[pos2(inner.left(), y), pos2(inner.right(), y)], false, px, a(hot, 0.9), 0.8);
    }
    if m.glow > 0.0 {
        let frame = crate::holo::plate(rect, [0.0, 0.0, crate::holo_frame::CUT.min(rect.height() / 2.0), 0.0]);
        let mut closed = frame.clone();
        closed.push(frame[0]);
        glow.halo(&closed, false, 10.0, lit(accent, 0.34 * m.glow));
        crate::holo::outline(painter, &frame, a(accent, 0.8 * m.glow));
    }
    if m.flash > 0.0 {
        let c = rect.left_top();
        if !crisp {
            light.spark(c, 12.0 * m.flash.max(0.4), lit(hot, 0.9 * m.flash));
        }
        painter.circle_filled(c, if crisp { 2.6 } else { 1.8 }, Color32::WHITE.gamma_multiply(0.95 * m.flash));
    }
    glow.paint(&glass);
    light.paint(painter);
}

/// `a` → `b` by `t`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

/// How far a block that does not answer the search steps back.
const SEARCH_DIM: f32 = 0.22;
/// How long `[ UNDO ]` stands after a change.
const UNDO_CHIP_FOR: Duration = Duration::from_secs(6);

/// "Show me around": the HUD's parts in the order the tour names them, a sentence each.
pub const TOUR: [(&str, &str); 7] = [
    ("emblem", "In the middle stands my emblem: that's me. It answers when I hear my name, and you can drag it anywhere."),
    ("header", "Along the top runs the header. Its boxes open my views, the console and the list of changes."),
    ("comms", "Below me, COMMS shows what I heard and what I answered. Click what I heard to put it right."),
    ("board", "My answers open as blocks. Drag them, double-click a title for large, fling one away to close it."),
    ("tray", "Blocks you put away wait in the tray at the bottom, closed ones for ten minutes."),
    ("telemetry", "The bottom strip is your microphone: its level, its peak and how I listen."),
    ("console", "And the text console is there when you would rather type than talk."),
];
/// The tour under way: when it began, and when each part's sentence starts.
static TOUR_CUES: Mutex<Option<(Instant, Vec<(usize, f32)>)>> = Mutex::new(None);
/// How long the light stays on the last part after its sentence began.
const TOUR_LAST_SECS: f32 = 5.0;

/// Starts the tour: its spoken line; the light moves from part to part as he says it.
/// The tour's light round one part: a hairline with two faint rims outside it.
fn tour_light(p: &egui::Painter, r: Rect, colour: Color32) {
    for (grow, a) in [(6.0, 0.06f32), (3.0, 0.14), (0.0, 0.9)] {
        p.rect_stroke(r.expand(grow), Rounding::ZERO, Stroke::new(1.0_f32, colour.gamma_multiply(a)));
    }
}

pub fn start_tour() -> String {
    let script: Vec<(usize, String)> = TOUR.iter().enumerate().map(|(k, (_, line))| (k, line.to_string())).collect();
    let (line, cues) = crate::hud_board::walkthrough_schedule("Here's the tour, Sir.", &script, TOUR.len());
    *lock(&TOUR_CUES) = Some((Instant::now(), cues));
    refresh();
    line
}
/// The margin between a block's frame and its content, on every side.
const BLOCK_MARGIN: f32 = 12.0;
/// The search bar's width (it sits centred under the header strip).
const FIND_BAR_WIDTH: f32 = 520.0;

/// The part of the screen in-app blocks live in (between the header and the telemetry strip).
pub(crate) fn block_bounds(screen: Rect) -> Rect {
    Rect::from_min_max(screen.min + vec2(8.0, 12.0 + HEADER_H + 8.0), screen.max - vec2(8.0, 12.0 + TELEMETRY_H + 8.0))
}

/// The board's area on `screen`: under the header strip, above the telemetry strip and the
/// caption band, right of the system column (`left_reserve`) and left of the text console
/// (`right_reserve`).
pub(crate) fn board_area(screen: Rect, left_reserve: f32, right_reserve: f32) -> Rect {
    let left = screen.left() + 24.0 + left_reserve;
    let right = screen.right() - 24.0 - right_reserve;
    Rect::from_min_max(pos2(left, screen.top() + 12.0 + HEADER_H + 14.0), pos2(right.max(left + 240.0), screen.bottom() - 12.0 - TELEMETRY_H - CAPTION_BAND))
}

/// An in-app block with a plain text body, exactly as JAI's own Markdown windows are framed
/// (the visual verifier fills the board with these). Its text scrolls when it is longer than
/// the block; `content` is what `Copy Content` and the search read. True when it was closed.
pub(crate) fn text_block(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], content: &str) -> bool {
    crate::hud_dock::describe_block(id, content);
    block(ctx, id, title, size, |ui| {
        crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
            ui.add(egui::Label::new(RichText::new(content).size(13.5).color(strip_ink())).wrap());
        });
    })
}

/// A view of a module outside the GUI (`body` draws its content) in a window of its own,
/// drawn inside the HUD's frame, so `body` may borrow what its caller holds. Returns whether
/// it was closed.
pub(crate) fn block(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], body: impl FnOnce(&mut Ui)) -> bool {
    let viewport = ViewportId::from_hash_of(format!("jai_block_{id}"));
    // (Where the HUD has no windows of its own to give, an offscreen session, it is drawn as
    // a block on the board.)
    if !crate::hud_dock::adopt(id) || ctx.embed_viewports() {
        crate::hud_dock::window_gone(id);
        detached_keep(ctx, viewport, true);
        return hud_block_window(ctx, id, title, size, body).close;
    }
    detached_keep(ctx, viewport, true);
    let name = title.split("//").next().unwrap_or(title).trim();
    let mut builder = egui::ViewportBuilder::default().with_title(format!("{name} \u{2014} J.A.I.")).with_app_id("jai").with_inner_size(size).with_min_inner_size([320.0, 180.0]).with_decorations(false);
    if let Some(icon) = window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = for_test_session(builder);
    // In a window of its own it is still one of his blocks: "close the windows", "close it" and
    // its name mean it too (`hud_dock::close_named`, `close_all_blocks`).
    crate::hud_dock::publish_window(id, title);
    let mut closed = crate::hud_dock::closing_progress(id).is_some();
    // (The viewport's callback may be asked for more than once; the body is drawn once.)
    let mut body = Some(body);
    ctx.show_viewport_immediate(viewport, builder, |ctx, class| {
        if ctx.input(|i| i.viewport().close_requested()) {
            closed = true;
            return;
        }
        let Some(body) = body.take() else { return };
        if class == egui::ViewportClass::Embedded {
            egui::Window::new(title).id(egui::Id::new(("jai_embedded_block", id))).show(ctx, |ui| body(ui));
            return;
        }
        egui::CentralPanel::default().frame(Frame::none().fill(block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
            // (Its own key, on top of a close that was asked for by word.)
            closed |= detached_bar(ui, ctx, id, title);
            body(ui);
        });
        paint_window_border(ctx);
    });
    if closed {
        crate::hud_dock::window_gone(id);
        lock(&DETACHED_MOTIONS).forget(viewport);
    }
    closed
}

/// A window of the board's own (the interface settings, the hub, a question to Sir) as one of
/// his views, in a window of its own. `open` goes false when Sir closes it. (Until 2026-11-13
/// these were plain windows inside the board and nowhere else. Sir: "there should be none
/// windows that are exclusive to JAI's UI".)
pub(crate) fn board_window(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], open: &mut bool, body: impl FnOnce(&mut Ui)) {
    if !*open {
        crate::hud_dock::forget(id);
        return;
    }
    if block(ctx, id, title, size, |ui| {
        crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), body);
    }) {
        *open = false;
        crate::hud_dock::forget(id);
    }
}

/// Where a new block opens and how large: a free spot off every open block and the orb, smaller
/// when the board is full, a cell of one tidy grid when nothing else fits
/// (`hud_dock::place_new_block`), inside one monitor when the HUD spans several. "Overlap the
/// windows" brings back the cascade.
/// The size a block needs for what its body took: the frame and the title row as they are,
/// the body's own width and height in place of its room. A body that fills its room (a map, a
/// model, a scroll list) needs what it has; the title row keeps the block wide enough for its
/// name and its two keys.
fn block_natural_size(frame: Rect, room: Rect, used: Rect) -> Vec2 {
    const TITLE_ROW: f32 = 300.0;
    let chrome = frame.size() - room.size();
    vec2(used.width().max(TITLE_ROW - chrome.x).ceil() + chrome.x, used.height().ceil() + chrome.y)
}

fn new_block_rect(ctx: &egui::Context, id: &str, regions: &[Rect], size: Vec2) -> Rect {
    let bounds = regions.iter().copied().reduce(|a, b| a.union(b)).unwrap_or(Rect::NOTHING);
    let slot = (crate::hud_dock::block_rects().len() % 5) as f32;
    let cascade = regions[0].min + vec2(40.0 + 34.0 * slot, 24.0 + 30.0 * slot);
    // (A windowed HUD that merely straddles a bezel keeps its blocks off the bezel too.)
    let parts = if regions.len() > 1 { regions.to_vec() } else { monitor_regions(ctx) };
    let (at, size) = crate::hud_dock::place_new_block(id, bounds, &parts, size, cascade);
    Rect::from_min_size(at, size)
}

/// Each monitor's part of the HUD window (points, window coordinates, the primary monitor's
/// first) while the window reaches onto more than one screen, and whether the HUD spans them (it
/// covers the whole primary monitor). Nothing for a window on one screen.
fn monitor_layout(ctx: &egui::Context) -> (Vec<Rect>, bool) {
    let Some(window) = ctx.input(|i| i.viewport().inner_rect) else { return (Vec::new(), false) };
    let ppp = ctx.pixels_per_point();
    // Every block asks in every frame: worked out once per frame (and window place and scale),
    // not once per block from a fresh copy of the monitor list.
    type Layout = (u64, Rect, f32, Vec<Rect>, bool);
    let key = egui::Id::new(("jai_monitor_layout", ctx.viewport_id()));
    let pass = ctx.cumulative_pass_nr();
    if let Some((_, _, _, regions, spanning)) = ctx.data(|d| d.get_temp::<Layout>(key)).filter(|(at, rect, scale, ..)| *at == pass && *rect == window && *scale == ppp) {
        return (regions, spanning);
    }
    let layout = monitor_layout_now(window, ppp);
    ctx.data_mut(|d| d.insert_temp::<Layout>(key, (pass, window, ppp, layout.0.clone(), layout.1)));
    layout
}

/// `monitor_layout` worked out afresh for a HUD window at `window` (screen points, `ppp`).
fn monitor_layout_now(window: Rect, ppp: f32) -> (Vec<Rect>, bool) {
    let inside = Rect::from_min_size(Pos2::ZERO, window.size());
    let mut monitors = crate::monitors::recent();
    monitors.sort_by_key(|m| !m.primary);
    // (As the desktop's background the taskbar lies over him: each monitor's part is its work
    // area, so his strips, his docked emblem and his clock stand clear of it. Seen 2026-11-14:
    // the emblem in its corner, cut off by the taskbar.)
    let background = crate::wallpaper::in_use();
    let part = |m: &crate::monitors::Monitor| {
        let [l, t, r, b] = if background { m.work } else { m.rect };
        Rect::from_min_max(pos2(l as f32, t as f32) / ppp, pos2(r as f32, b as f32) / ppp).translate(-window.min.to_vec2())
    };
    let spanning = monitors.first().is_some_and(|m| m.primary && inside.expand(2.0).contains_rect(part(m)));
    // (A monitor that merely touches the window's edge shows nothing of it.)
    let regions: Vec<Rect> = monitors.iter().map(|m| part(m).intersect(inside)).filter(|r| r.width() > 1.0 && r.height() > 1.0).collect();
    if regions.len() > 1 || (background && spanning) { (regions, spanning) } else { (Vec::new(), false) }
}

/// Each monitor's part of the HUD window while it reaches onto more than one; empty otherwise.
fn monitor_regions(ctx: &egui::Context) -> Vec<Rect> {
    monitor_layout(ctx).0
}

/// Where the HUD proper lives: the whole window, or, while one board spans every monitor, the
/// primary monitor's part of it. The header, the console, the emblem and the board stay on
/// Sir's first screen; the others are room for his windows. (The box around differently placed
/// monitors has corners no screen shows: a header laid out on the whole window lands there.)
pub(crate) fn home_screen(ctx: &egui::Context) -> Rect {
    match monitor_layout(ctx) {
        (regions, true) => regions[0],
        _ => ctx.screen_rect(),
    }
}

/// Where in-app blocks may stand: the one screen between its header and its telemetry strip,
/// or, on a spanned HUD, each monitor's part (the home screen's first, between its strips).
fn block_regions(ctx: &egui::Context) -> Vec<Rect> {
    match monitor_layout(ctx) {
        (regions, true) => regions.iter().enumerate().map(|(n, r)| if n == 0 { block_bounds(*r) } else { r.shrink(8.0) }).collect(),
        _ => vec![block_bounds(ctx.screen_rect())],
    }
}

/// The region `at` lies in, else the nearest one (`regions` is never empty).
fn region_at(regions: &[Rect], at: Pos2) -> Rect {
    regions.iter().copied().min_by(|a, b| a.distance_sq_to_pos(at).total_cmp(&b.distance_sq_to_pos(at))).unwrap_or(Rect::NOTHING)
}

/// `rect` moved the shortest way into `screen` (and made smaller only where it is larger).
fn moved_inside(rect: Rect, screen: Rect) -> Rect {
    let size = rect.size().min(screen.size());
    let min = pos2(rect.min.x.clamp(screen.min.x, screen.max.x - size.x), rect.min.y.clamp(screen.min.y, screen.max.y - size.y));
    Rect::from_min_size(min, size)
}

/// The desktop clock of `size` with its top middle at `top`: whole on the monitor that point
/// is over (`room_at`), 8 points in from its edges.
fn clock_rect(top: Pos2, size: Vec2, room_at: impl Fn(Pos2) -> Rect) -> Rect {
    moved_inside(Rect::from_min_size(pos2(top.x - size.x / 2.0, top.y), size), room_at(top).shrink(8.0))
}

/// Where Sir holds the desktop clock after a frame's drag of `delta`: from where he held it
/// (from where it stood, at first), not from where it was kept whole on a monitor, or it
/// could never leave that monitor's edge for the next.
fn clock_dragged(held: Option<Pos2>, shown: Rect, delta: Vec2) -> Pos2 {
    held.unwrap_or(pos2(shown.center().x, shown.top())) + delta
}

/// The SCREENS menu's width (it narrows on a screen that is narrower).
const SCREENS_WIDTH: f32 = 400.0;

/// What Sir clicked in the SCREENS menu.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct ScreensPick {
    /// `[ MOVE HERE ]` on this monitor (1-based).
    send: Option<usize>,
    /// One board across every monitor on (true) or off.
    span: Option<bool>,
    close: bool,
}

/// The SCREENS menu: one row per monitor — its name over its size and place, its button beside
/// them, each in its own column, so no text ever stands on another — and under them the switch
/// between one board across every monitor and the board on one. `here`: the monitor the HUD is on.
fn screens_menu(ui: &mut Ui, monitors: &[crate::monitors::Monitor], spanning: bool, here: Option<usize>) -> ScreensPick {
    let mut picked = ScreensPick::default();
    // (`mark`: the name the window drill clicks it by.)
    let key = |ui: &mut Ui, text: &str, hint: &str, mark: &dyn Fn() -> String| {
        let response = crate::holo_ui::key(ui, text).on_hover_text(hint);
        if crate::window_drill::enabled() {
            crate::window_drill::mark(&mark(), response.rect);
        }
        response.clicked()
    };
    let title = format!("SCREENS // {} DISPLAY{}", monitors.len(), if monitors.len() == 1 { "" } else { "S" });
    crate::holo_ui::titled_plate(ui, &title, if spanning { "SPANNING ALL" } else { "ONE SCREEN" }, |ui| {
        for m in monitors {
            // Its name with its key (or the mark that the HUD stands on it) at the row's end,
            // then where it lies on the desk, on a line of its own.
            ui.horizontal(|ui| {
                ui.set_min_height(holo_kit::KEY_H);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if here == Some(m.index) {
                        chip(ui, "HUD IS HERE", holo::theme().accent());
                    } else if key(ui, "MOVE HERE", "My emblem on this monitor", &|| format!("screens:move:{}", m.index)) {
                        picked.send = Some(m.index);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let name = format!("DISPLAY {}{}", m.index, if m.primary { "  //  PRIMARY" } else { "" });
                        ui.add(egui::Label::new(RichText::new(name).font(strip_font()).color(strip_ink())).truncate().selectable(false));
                    });
                });
            });
            let place = format!("{} x {}  AT  {}, {}", m.width(), m.height(), m.rect[0], m.rect[1]);
            ui.add(egui::Label::new(RichText::new(place).font(strip_font()).color(strip_muted())).truncate().selectable(false));
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            if monitors.len() > 1 {
                if spanning {
                    if key(ui, "BACK TO ONE MONITOR", "My background on your first monitor only", &|| "screens:span:off".into()) {
                        picked.span = Some(false);
                    }
                } else if key(ui, "SPAN ALL MONITORS", "My background across every monitor; the header and the console stay on your first", &|| "screens:span:on".into()) {
                    picked.span = Some(true);
                }
            }
            if key(ui, "CLOSE", "Close (Esc)", &|| "screens:close".into()) {
                picked.close = true;
            }
        });
    });
    picked
}

/// A sharp technical block for egui content: dark slate fill, 1 px cyan frame, white L-brackets
/// and the micro header (label left, tag right).
/// What kind of widget this is, as its block's tag ("NOTE", "GAUGE").
fn short_type_upper(spec: &crate::widgets::DynamicWidgetSpec) -> &'static str {
    widget_view::short_type_upper(&spec.kind)
}

/// The scroll area of a column of blocks: as tall as its blocks, up to `room`. (An area hands
/// its content the height it had the frame before, 400 px to begin with, and a scroll area
/// takes no more than it is handed: left to itself the column could only ever shrink, and a
/// long note was cut off half-way with no lower edge. Sir, 2026-11-13, of the briefing's
/// widget: "this window's border is still broken".) Where the blocks are taller than the
/// screen has room for, the cut edge fades out: a block that goes on below is not one that
/// lost its frame.
fn column_scroll(ui: &mut Ui, room: f32, blocks: impl FnOnce(&mut Ui)) {
    let room = room.max(120.0);
    ui.set_max_height(room);
    let out = crate::holo_block::faded(ui, egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]), blocks);
    let (view, above) = (out.inner_rect, out.state.offset.y);
    let below = out.content_size.y - view.height() - above;
    let fade = |edge: Rect, solid_at_bottom: bool| {
        let (top, bottom) = if solid_at_bottom { (Color32::TRANSPARENT, window_void()) } else { (window_void(), Color32::TRANSPARENT) };
        let mut mesh = egui::Mesh::default();
        for (at, colour) in [(edge.left_top(), top), (edge.right_top(), top), (edge.left_bottom(), bottom), (edge.right_bottom(), bottom)] {
            mesh.colored_vertex(at, colour);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        ui.painter().add(egui::Shape::mesh(mesh));
    };
    const FADE: f32 = 34.0;
    if below > 1.0 {
        fade(Rect::from_min_max(pos2(view.left() - 2.0, view.bottom() - FADE), pos2(view.right() + 2.0, view.bottom() + 1.0)), true);
    }
    if above > 1.0 {
        fade(Rect::from_min_max(pos2(view.left() - 2.0, view.top() - 1.0), pos2(view.right() + 2.0, view.top() + FADE)), false);
    }
}

/// Rows between two scan lines, and the grain's tile and strength (alpha of 255).
const SCAN_PITCH: f32 = 3.0;
const GRAIN_TILE: usize = 128;
const GRAIN_ALPHA: u32 = 15;

/// The grain's tile: every pixel a shade lighter or darker by chance (a fixed pattern: the
/// same tile on every start).
fn grain_image() -> egui::ColorImage {
    let mut seed = 0x9E37_79B9u32;
    let pixels = (0..GRAIN_TILE * GRAIN_TILE)
        .map(|_| {
            // xorshift
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let (light, a) = (seed & 1 == 0, ((seed >> 8) % (GRAIN_ALPHA + 1)) as u8);
            if light {
                // (Premultiplied by hand. `Color32::from_white_alpha` takes its alpha as linear
                // light: 15 of 255 came out as 69 of 255, and from 2026-11-11 to 2026-11-19 the
                // grain lay over the black as a grey fog four times its strength.)
                Color32::from_rgba_premultiplied(a, a, a, a)
            } else {
                Color32::from_black_alpha(a.saturating_mul(2))
            }
        })
        .collect();
    egui::ColorImage { size: [GRAIN_TILE, GRAIN_TILE], pixels }
}

/// The display's finish: scan lines and film grain over everything the HUD draws, so it reads as
/// light on glass and not as flat vector art (Sir, 2026-11-11: "more realistic, a bit more
/// rough"). Two meshes on the topmost layer; the grain shifts twelve times a second, but only
/// on frames that are painted anyway (it never asks for one).
pub(crate) fn paint_finish(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("hud_finish")));
    let mut lines = egui::Mesh::default();
    let dark = Color32::from_black_alpha(24);
    let mut y = rect.top();
    while y < rect.bottom() {
        lines.add_colored_rect(Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), 1.0)), dark);
        y += SCAN_PITCH;
    }
    painter.add(Shape::mesh(lines));
    let key = egui::Id::new("hud_grain");
    let grain = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)).unwrap_or_else(|| {
        let options = egui::TextureOptions { magnification: egui::TextureFilter::Nearest, minification: egui::TextureFilter::Nearest, wrap_mode: egui::TextureWrapMode::Repeat, mipmap_mode: None };
        let texture = ctx.load_texture("hud_grain", grain_image(), options);
        ctx.data_mut(|d| d.insert_temp(key, texture.clone()));
        texture
    });
    let tick = (ctx.input(|i| i.time) * 12.0) as u32;
    let shift = vec2((tick.wrapping_mul(0x9E37) % 97) as f32 / 97.0, (tick.wrapping_mul(0x85EB) % 89) as f32 / 89.0);
    let mut mesh = egui::Mesh::with_texture(grain.id());
    mesh.add_rect_with_uv(rect, Rect::from_min_size(pos2(shift.x, shift.y), rect.size() / GRAIN_TILE as f32), Color32::WHITE);
    painter.add(Shape::mesh(mesh));
    // The glass itself: its edges a little darker than its middle, per screen of a HUD that
    // spans several, else over the one it is on. (Nothing is worn: no dust, no scratches.)
    let screens = monitor_regions(ctx);
    for screen in if screens.is_empty() { vec![rect] } else { screens } {
        let inner = screen.shrink2(screen.size() * 0.22);
        let mut falloff = egui::Mesh::default();
        for (at, colour) in [screen.left_top(), screen.right_top(), screen.right_bottom(), screen.left_bottom()].into_iter().map(|at| (at, Color32::from_black_alpha(46))).chain([inner.left_top(), inner.right_top(), inner.right_bottom(), inner.left_bottom()].into_iter().map(|at| (at, Color32::TRANSPARENT))) {
            falloff.colored_vertex(at, colour);
        }
        for k in 0..4u32 {
            let n = (k + 1) % 4;
            falloff.add_triangle(k, n, 4 + n);
            falloff.add_triangle(k, 4 + n, 4 + k);
        }
        painter.add(Shape::mesh(falloff));
    }
}

/// When the view last changed: the canvas behind the views rushes towards the middle and back
/// for `VIEW_ZOOM_SECS` (the views play their own entrances; nothing with text is scaled).
static VIEW_ZOOM: Mutex<Option<Instant>> = Mutex::new(None);
const VIEW_ZOOM_SECS: f32 = 0.25;

/// The canvas's scale about its centre in this frame of a view change (1: at rest).
fn view_zoom(ctx: &egui::Context) -> f32 {
    let Some(t) = lock(&VIEW_ZOOM).map(|at| at.elapsed().as_secs_f32() / VIEW_ZOOM_SECS).filter(|t| *t < 1.0 && motion_enabled()) else { return 1.0 };
    ctx.request_repaint();
    1.0 + 0.2 * (t * std::f32::consts::PI).sin()
}

/// How far the canvas has drifted against the pointer: a hundredth of the pointer's way from the
/// home screen's centre (some 13 px at the edge of a 1440p screen), eased, in whole pixels; it
/// settles back with the pointer away, and stays put with the animations off.
fn canvas_drift(ctx: &egui::Context, home: Rect) -> Vec2 {
    if !motion_enabled() {
        return Vec2::ZERO;
    }
    let want = ctx.pointer_hover_pos().filter(|at| home.contains(*at)).map_or(Vec2::ZERO, |at| (home.center() - at) * 0.01);
    let id = egui::Id::new("jai_canvas_drift");
    vec2(ctx.animate_value_with_time(id.with(0), want.x, 0.6), ctx.animate_value_with_time(id.with(1), want.y, 0.6)).round()
}

/// The obsidian canvas: `#020408` with a radial vignette of the theme's colour in the centre
/// (fading to black at the edges) and the 28 px dot matrix at 6 %.
/// Whether the dot at `offset` from the canvas's own origin (the drift taken out) is one of the
/// "+" marks: whole grid steps, so a mark stays on its dot as the canvas drifts.
fn is_cross(offset: Vec2) -> bool {
    let step = |v: f32| ((v - DOT_SPACING / 2.0) / DOT_SPACING).round() as i64;
    step(offset.x).rem_euclid(CROSS_EVERY) == 0 && step(offset.y).rem_euclid(CROSS_EVERY) == 0
}

/// Where the core stands, how large it is at rest, how far it is docked (0-1) and how loud
/// JAI's voice is (0-1); the HUD says so every frame. It is the origin of the
/// drafting table.
static CANVAS_ORIGIN: Mutex<Option<(Pos2, f32, f32, f32)>> = Mutex::new(None);

/// The canvas everything stands on: the theme's void as a drafting table. A dot grid with a
/// "+" on every fourth dot (fading out round the blocks, so what they show stands out), and the
/// core's construction lines: its two axes across the whole screen and three range circles
/// round it, the first with a tick every 5°. The panels are seen to be placed about him.
pub(crate) fn paint_canvas(ctx: &egui::Context) {
    paint_canvas_in(ctx, &media_rects(ctx));
}

/// Where each monitor's own background media goes in the HUD window: its number and its rect.
/// (A window that does not know its place, an offscreen one, is one monitor.)
fn media_rects(ctx: &egui::Context) -> Vec<(usize, Rect)> {
    let monitors = crate::monitors::recent();
    match ctx.input(|i| i.viewport().inner_rect) {
        Some(window) if !monitors.is_empty() => crate::bg_media::rects_in(window, ctx.pixels_per_point(), &monitors),
        _ => vec![(1, ctx.screen_rect())],
    }
}

/// `paint_canvas` with the media's places given.
pub(crate) fn paint_canvas_in(ctx: &egui::Context, media: &[(usize, Rect)]) {
    use crate::holo::{a, polar};
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let t = crate::holo::theme();
    painter.rect_filled(rect, Rounding::ZERO, t.void());
    crate::bg_media::paint(ctx, &painter, media);
    let home = home_screen(ctx);
    // Depth: the table drifts a little against the pointer, in whole pixels so it stays sharp;
    // the blocks stand still.
    let drift = canvas_drift(ctx, home);
    let zoom = view_zoom(ctx);
    let (origin, reach, dock, voice) = lock(&CANVAS_ORIGIN).unwrap_or((home.center(), crate::holo_core::centre_radius(home), 0.0, 0.0));
    let origin = origin + drift;
    let (ink, w) = (t.ink(), 1.0 / ctx.pixels_per_point());
    crate::holo::rule(&painter, pos2(rect.left(), origin.y), pos2(rect.right(), origin.y), a(ink, 0.07));
    crate::holo::rule(&painter, pos2(origin.x, rect.top()), pos2(origin.x, rect.bottom()), a(ink, 0.07));
    let mut light = crate::holo::Light::new(&painter);
    // (His first circle and its ticks brighten as he speaks.)
    for (k, share) in [(1.55, 0.085 + 0.20 * voice), (2.3, 0.06), (3.2, 0.045)] {
        light.ring(origin, reach * k * zoom, w, a(ink, share));
    }
    let ring = reach * 1.55 * zoom;
    for i in 0..72 {
        let angle = TAU * i as f32 / 72.0;
        let long = i % 6 == 0;
        light.seg(polar(origin, ring, angle), polar(origin, ring + if long { 9.0 } else { 4.0 }, angle), w, a(ink, if long { 0.22 } else { 0.11 } + 0.25 * voice));
    }
    // The windows are tied to him: a leader from his first circle to the corner of each
    // window's header that faces him, and while he speaks a point of light runs out along it.
    // (His windows stand on the screen, above the board; a session without windows draws
    // them as blocks on it.)
    let mut blocks = crate::hud_dock::block_rects();
    blocks.extend(own_window_rects(ctx));
    if dock < 0.5 {
        let run = (ctx.input(|i| i.time) * 0.9).fract() as f32;
        for (from, to) in blocks.iter().filter_map(|block| crate::holo_stage::leader(origin, ring, *block)) {
            crate::holo::leader(&painter, from, to, a(ink, 0.26 + 0.3 * voice));
            if voice > 0.03 {
                // (Along its two legs: the slope to the knee, then level to the window.)
                let rise = (to.y - from.y).abs().min((to.x - from.x).abs());
                let knee = pos2(from.x + (to.x - from.x).signum() * rise, to.y);
                let (first, second) = ((knee - from).length(), (to - knee).length());
                let at = run * (first + second);
                let point = if at <= first { from + (knee - from) * (at / first.max(1.0)) } else { knee + (to - knee) * ((at - first) / second.max(1.0)) };
                light.spark(point, 4.0, crate::holo::lit(t.hot(), voice.min(1.0)));
            }
        }
    }
    light.paint(&painter);
    let mut dots = egui::Mesh::default();
    let (dot, cross) = (a(ink, DOT_ALPHA), a(ink, CROSS_ALPHA));
    let centre = home.center() + drift;
    let mut y = rect.top() + DOT_SPACING / 2.0 + drift.y.rem_euclid(DOT_SPACING) - DOT_SPACING;
    while y < rect.bottom() {
        let mut x = rect.left() + DOT_SPACING / 2.0 + drift.x.rem_euclid(DOT_SPACING) - DOT_SPACING;
        while x < rect.right() {
            let at = centre + (pos2(x, y) - centre) * zoom;
            let at = pos2(at.x.round(), at.y.round());
            let fade = (blocks.iter().map(|b| b.distance_to_pos(at)).fold(f32::INFINITY, f32::min) / 48.0).clamp(0.0, 1.0);
            if fade > 0.0 && is_cross(pos2(x, y) - rect.left_top() - drift) {
                for arm in [vec2(2.0 * CROSS_ARM + 1.0, 1.0), vec2(1.0, 2.0 * CROSS_ARM + 1.0)] {
                    dots.add_colored_rect(Rect::from_center_size(at + vec2(0.5, 0.5), arm), cross.gamma_multiply(fade));
                }
            } else if fade > 0.0 {
                dots.add_colored_rect(Rect::from_min_size(at, vec2(1.0, 1.0)), dot.gamma_multiply(fade));
            }
            x += DOT_SPACING;
        }
        y += DOT_SPACING;
    }
    painter.add(Shape::mesh(dots));
}

impl eframe::App for HudApp {
    /// Transparent: the canvas paints the background (pure orb mode stays see-through).
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    /// `--window-drill`: its clicks arrive as the window's own pointer input.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::window_drill::feed(raw_input);
        // As the desktop's background: the pointer over the bare desktop is his pointer.
        crate::wallpaper::feed(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // A panic anywhere in a frame costs that frame, never the HUD (the next one redraws).
        let started = Instant::now();
        if crate::crash_guard::guard("HUD frame", || self.frame(ctx)).is_none() {
            self.last_frame = Instant::now();
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        frame_stats(started.elapsed(), &frame_parts(Instant::now()));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        lock(&SHARED.ctx).take();
        SHARED.hwnd.store(0, Ordering::Release);
        crate::camera_feed::stop();
        if self.ptt_sent {
            self.command(HudCommand::Ptt { active: false });
        }
    }
}

impl HudApp {
    fn frame(&mut self, ctx: &egui::Context) {
        ROOT_FRAMES.fetch_add(1, Ordering::AcqRel);
        // (This frame is what the board was waiting for; it says below when it wants the next.)
        BOARD_DUE_MS.store(u64::MAX, Ordering::Release);
        #[cfg(windows)]
        keep_drawing();
        mark("start");
        crate::live_shots::tick(ctx);
        // `--window-drill`: its next step, from what the previous frame drew.
        if let Some(drill) = self.drill.as_mut() {
            drill.step(ctx, self.hwnd);
        }
        crate::window_drill::begin_frame();
        let now = Instant::now();
        // Clamped: the first frame after an idle second never makes an animation jump.
        let dt = crate::holo_core::safe_dt(now.duration_since(self.last_frame).as_secs_f32());
        if ctx.input(|i| i.pointer.any_down() || i.pointer.delta().length() > 0.5 || !i.keys_down.is_empty()) {
            self.last_activity = now;
        }

        let iface = self.sync_interface(ctx);
        // Settings → Animations off: windows come and go without their motion.
        REDUCED_MOTION.store(!iface.animations, Ordering::Relaxed);
        // (The console publishes where it stands again if it is drawn this frame.)
        publish_console_rect(None);
        self.sync_background(ctx, &iface);
        self.enforce_fullscreen(ctx, &iface);
        self.note_window(ctx, &iface);
        self.handle_close(ctx);
        self.handle_keys(ctx);
        self.take_dropped_files(ctx);
        self.flush_memory_search();

        for mode in crate::hud_dock::take_view_requests() {
            self.switch_view(mode, false);
        }
        let model = Arc::clone(&self.model);
        let mut m = lock(&model);
        m.toasts.retain(|t| t.until > now);
        if iface.animations {
            self.reactor.step(&m.state, m.level.as_ref(), dt);
        }
        self.step_emblem(&m, if iface.animations { dt } else { 0.0 });

        // The HUD's text scale (JAI's extra windows set their own while they draw).
        TEXT_SCALE.with(|s| s.set(iface.font_scale));
        let screen = ctx.screen_rect();
        crate::interface::publish_window_size((screen.width() * iface.scale, screen.height() * iface.scale));
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        crate::bg_media::publish(&iface, m.state != "idle" || !own_window_rects(ctx).is_empty());
        paint_canvas(ctx);
        // (Offscreen renders stay clean: the verifier reads their colours.)
        if iface.display_finish && !windows_still(ctx) {
            paint_finish(ctx);
        }
        // The Settings drawer slides in from the right (VIEW → SETTINGS).
        let p = self.palette;
        let home = home_screen(ctx);
        let drawer = Frame::none().fill(p.card_solid.gamma_multiply(0.96)).stroke(stroke(1.0, p.line)).inner_margin(Margin::same(14.0));
        if home != screen {
            // (A side panel hangs on the window's own edge: on a board across the monitors
            // that is the far end of the last one, its top in a corner no screen shows.
            // There the drawer stands at the right of the home screen.)
            if self.drawer_open {
                let width = (home.width() * 0.3).clamp(340.0, 440.0);
                egui::Area::new(egui::Id::new("hud_settings_drawer_span")).order(egui::Order::Middle).fixed_pos(pos2(home.right() - width, home.top())).show(ctx, |ui| {
                    drawer.show(ui, |ui| {
                        ui.set_width(width - 28.0);
                        ui.set_height(home.height() - 28.0);
                        self.settings_drawer(ui, &m, &iface);
                    });
                });
            }
        } else {
            let width = screen.width();
            egui::SidePanel::right("hud_settings_drawer")
                .resizable(true)
                .default_width((width * 0.3).clamp(340.0, 440.0))
                .width_range(300.0..=(width * 0.5).max(320.0))
                .show_separator_line(false)
                .frame(drawer)
                .show_animated(ctx, self.drawer_open, |ui| self.settings_drawer(ui, &m, &iface));
        }
        mark("board and emblem");
        self.canvas(ctx, &m, &iface, dt);
        mark("overlays");
        // The media alone, while the board waits to be called (whole mode).
        crate::bg_media::curtain(ctx, &media_rects(ctx));
        // Night mode: the HUD dims (by the clock or because Sir said so).
        crate::hud_feel::set_night_by_clock(iface.night_mode);
        // (A scripted session shows the dimming from its 34th to its 46th second on request.)
        let night = (crate::hud_feel::night_active() || (demo("night") && (34.0..46.0).contains(&hud_age()))) && !windows_still(ctx);
        self.night = crate::holo_motion::approach(self.night, if night { 1.0 } else { 0.0 }, dt, 0.9);
        if self.night > 0.0 {
            let dim = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_night")));
            dim.rect_filled(ctx.screen_rect(), Rounding::ZERO, Color32::from_black_alpha((NIGHT_DIM * 255.0 * self.night) as u8));
            if self.night < 1.0 {
                ctx.request_repaint();
            }
        }
        if iface.edge_light && !windows_still(ctx) {
            crate::hud_notify::paint_edge(ctx, home_screen(ctx), 1.0 - 0.5 * self.night);
        }
        self.presence(ctx, &m, &iface, dt);
        // (The power-up sequence stands alone on its dark: what the start has to say waits.)
        if self.boot.is_none() {
            self.toasts(ctx, &m);
            self.undo_chip(ctx);
        }
        self.popups(ctx, dt);
        self.room_alert_overlay(ctx, &m);
        // (Neither the fullscreen board nor the one across the monitors is resized by hand,
        // nor the desktop's background.)
        if iface.frameless && !iface.fullscreen && !iface.span_all_monitors && !maximized && !self.as_background() {
            Self::resize_grips(ctx);
        }
        // As the desktop's background: the orb that leaves it when Sir speaks to him unseen,
        // and in orb mode (or mini mode) the floating orb (or the strip) in front of everything.
        self.corner_orb_frame(ctx, &m, &iface);
        self.floating_orb(ctx, &m, &iface);
        if m.offline {
            self.offline_overlay(ctx);
        }
        drop(m);
        if std::mem::take(&mut self.boot_done) {
            lock(&self.model).restart_toasts();
        }
        self.after_body(ctx, &iface);
        settle_new_areas(ctx);
        // (Which of his keys the pointer over the desktop is on, for the mouse hook.)
        crate::wallpaper::after_frame(ctx);
        // A scripted session's board to look at (`JAI_DEMO=board`): the verifier's sample
        // report, put up once in its 10th second (nothing of Sir's is in it).
        if demo("board") && hud_age() > 10.0 {
            static SHOWN: std::sync::Once = std::sync::Once::new();
            SHOWN.call_once(|| {
                crate::hud_board::present(crate::visual_ui_verifier::sample_report(5, 0));
            });
        }
        // (`JAI_DEMO=history`: the history, which only a click on its key opens, in its 12th second.)
        if demo("history") && hud_age() > 12.0 {
            static OPENED: std::sync::Once = std::sync::Once::new();
            OPENED.call_once(|| self.history_open = true);
        }
        // A scripted session's stand-in for Sir's mouse on the desktop (`JAI_DEMO=desk-click`):
        // from its 16th second the pointer comes to rest on `[ CHANGES ]`, presses it and lets
        // go, by the way the hook's events take from `wallpaper::took` on. The log says
        // whether the press was his.
        if self.backdrop == crate::wallpaper::Standing::On && demo("desk-click") && self.demo_click < 40 && hud_age() > 16.0 {
            let key = *lock(&DEMO_KEY);
            if let (Some(key), Some(origin)) = (key, ctx.input(|i| i.viewport().inner_rect.map(|r| r.min))) {
                let ppp = ctx.pixels_per_point();
                let (x, y) = (((origin.x + key.center().x) * ppp).round() as i32, ((origin.y + key.center().y) * ppp).round() as i32);
                self.demo_click += 1;
                match self.demo_click {
                    1 | 10 => {
                        crate::wallpaper::demo_mouse(0, x, y);
                    }
                    20 => info!("[DESKTOP] demo: a press on [ CHANGES ] at {x},{y} on the desktop: his = {:?}", crate::wallpaper::demo_mouse(1, x, y)),
                    24 => {
                        crate::wallpaper::demo_mouse(2, x, y);
                    }
                    _ => {}
                }
                ctx.request_repaint();
            }
        }
        self.last_frame = now;
    }

    /// "Show your windows" (or his key for it): the list of his windows of their own, each
    /// with its keys: in front of the others, onto the monitor Sir is using, closed.
    fn windows_list(&mut self, ctx: &egui::Context) {
        if !WINDOWS_LIST.load(Ordering::Acquire) {
            return;
        }
        {
            // (Looked up twice a second, not in every frame.)
            static SEEN: Mutex<Option<(Instant, Vec<desk::OwnWindow>)>> = Mutex::new(None);
            let own = {
                let mut seen = lock(&SEEN);
                if seen.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_millis(500)) {
                    *seen = Some((Instant::now(), crate::own_windows::list()));
                }
                seen.as_ref().map(|(_, own)| own.clone()).unwrap_or_default()
            };
            // (Not called "Windows": while it was, "close the windows" meant this one block.
            // Seen in a silent session, 2026-11-14.)
            const TITLE: &str = "On call // what I have open";
            let mut act: Option<(isize, u8, [i32; 4])> = None;
            let closed = block(ctx, "on_call", TITLE, [470.0, 300.0], |ui| {
                let rows: Vec<_> = own.iter().filter(|w| !w.title.starts_with("On call")).collect();
                if rows.is_empty() {
                    ui.label(RichText::new("No window of mine is open, Sir.").size(13.5).color(strip_muted()));
                }
                crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
                    for w in rows {
                        ui.horizontal(|ui| {
                            let name = w.title.trim_end_matches("J.A.I.").trim_end_matches([' ', '\u{2014}', '-']).to_string();
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let key = |ui: &mut Ui, text: &str, hint: &str| crate::holo_ui::key(ui, text).on_hover_text(hint).clicked();
                                if key(ui, "CLOSE", "Close it") {
                                    act = Some((w.hwnd, 2, w.rect));
                                }
                                if key(ui, "HERE", "Onto the monitor you are using") {
                                    act = Some((w.hwnd, 1, w.rect));
                                }
                                if key(ui, "FRONT", "In front of the other windows") {
                                    act = Some((w.hwnd, 0, w.rect));
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new(RichText::new(name).size(13.5).color(strip_ink())).truncate());
                                });
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
            });
            match act {
                Some((hwnd, 0, _)) => desk::raise(hwnd),
                Some((hwnd, 1, [l, t, r, b])) => {
                    let monitors = crate::monitors::recent();
                    if let Some(on) = crate::wallpaper::monitor_in_use(&monitors) {
                        let (w, h) = ((r - l).min(on.work[2] - on.work[0]), (b - t).min(on.work[3] - on.work[1]));
                        crate::own_windows::glide(hwnd, [l, t, r - l, b - t], [(on.work[0] + on.work[2] - w) / 2, (on.work[1] + on.work[3] - h) / 2, w, h]);
                        desk::raise(hwnd);
                    }
                }
                Some((hwnd, _, _)) => desk::close(hwnd),
                None => {}
            }
            if closed {
                WINDOWS_LIST.store(false, Ordering::Release);
            }
        }
    }

    /// The timers that run: what the strip says of each ("TIMER  04:12"), and the share of its
    /// time that has passed (None for a stopwatch, which has no end).
    fn running_timers(&self) -> Vec<(String, Option<f32>)> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.app
            .tools()
            .widgets
            .widgets()
            .iter()
            .rev()
            .filter_map(|w| {
                let crate::widgets::WidgetKind::CountdownOrStopwatch { running: true, .. } = &w.kind else { return None };
                let (elapsed, remaining) = crate::widgets::timer_state(&w.kind, now_ms)?;
                let secs = remaining.unwrap_or(elapsed) / 1000;
                let share = remaining.map(|left| elapsed as f32 / (elapsed + left).max(1) as f32);
                Some((format!("{}  {:02}:{:02}", timer_word(&w.title, remaining.is_some()), secs / 60, secs % 60), share))
            })
            .collect()
    }

    /// The day beside the emblem while he is the desktop's background: the lesson and the
    /// appointment that come next, the reminders after them, the timers that run and the song
    /// that plays. Left of the emblem; right of it while his widgets' column stands on the left.
    fn day_panel(&self, ctx: &egui::Context, home: Rect, centre: Pos2, radius: f32) {
        let now = chrono::Local::now().naive_local();
        let glance = crate::idle_screen::glance();
        let dash = crate::kit_screen::dash();
        let mut sections: Vec<(&str, Vec<String>)> = Vec::new();
        // (What comes next stands here only when Sir asks, for a moment: "what's next?". "I
        // don't want the next thing on my screen", 2026-11-14.)
        if crate::hud_feel::next_shown() {
            let mut next: Vec<String> = Vec::new();
            if let Some((subject, start, room)) = &glance.lesson {
                next.push(crate::idle_screen::next_line(subject, *start, room, now));
            }
            if let Some((title, start, place)) = glance.next.as_ref().filter(|(_, start, _)| *start > now) {
                next.push(crate::idle_screen::next_line(title, *start, place, now));
            }
            next.extend(dash.next.iter().take(2).cloned());
            if !next.is_empty() {
                sections.push(("NEXT", next));
            }
        }
        let timers: Vec<String> = self.running_timers().into_iter().map(|(line, _)| line).collect();
        if !timers.is_empty() {
            sections.push(("TIMERS", timers));
        }
        // The orders he was told to carry out later ("play music in one minute").
        let planned: Vec<String> = crate::later::planned().into_iter().take(3).map(|(due, order)| format!("{}  {}", due.format("%H:%M"), order)).collect();
        if !planned.is_empty() {
            sections.push(("PLANNED", planned));
        }
        if let Some(song) = dash.playing {
            sections.push(("NOW PLAYING", vec![song]));
        }
        if sections.is_empty() {
            return;
        }
        let width = (home.width() * 0.2).clamp(240.0, 380.0);
        let x = if self.system_column_rect.is_none() { (centre.x - radius * 1.55 - width).max(home.left() + 28.0) } else { (centre.x + radius * 1.55).min(home.right() - 28.0 - width) };
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("hud_day"))).with_clip_rect(home);
        let t = holo::theme();
        let line_font = holo::font(Face::Body, 15.0);
        // (Laid out first: the whole of it stands level with the core's middle.)
        let laid: Vec<(&str, Vec<Arc<egui::Galley>>)> = sections.iter().map(|(title, lines)| (*title, lines.iter().map(|l| p.layout(l.clone(), line_font.clone(), t.ink(), width)).collect())).collect();
        let height: f32 = laid.iter().map(|(_, lines)| 30.0 + lines.iter().map(|g| g.size().y + 5.0).sum::<f32>() + 14.0).sum();
        let mut y = (centre.y - height / 2.0).max(home.top() + HEADER_H + 30.0);
        for (k, (title, lines)) in laid.into_iter().enumerate() {
            // Numbered like the parts of a drawing: its number, its name, a rule.
            holo_kit::section(&p, pos2(x, y), width, &format!("{:02}", k + 1), title, 1.0);
            y += 30.0;
            for galley in lines {
                let h = galley.size().y;
                p.galley(pos2(x, y), galley, t.ink());
                y += h + 5.0;
            }
            y += 14.0;
        }
        // (The minutes on it count down.)
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// How much room the desktop clock takes under the emblem (nothing where it is off, or
    /// where Sir put it elsewhere): what else stands under the emblem stands under the clock.
    /// (`rest`: the emblem's radius at rest, on whichever monitor it stands; the clock's too.)
    fn clock_room(&self, iface: &InterfaceSettings, rest: f32) -> f32 {
        if self.as_background() && iface.desk_clock && iface.desk_clock_pos.is_none() && self.clock_drag.is_none() {
            crate::idle_screen::clock_height(rest, iface.desk_clock_date, crate::idle_screen::glance().weather.is_some()) + 14.0
        } else {
            0.0
        }
    }

    /// The clock on the desktop while he is its background: the time large under the emblem,
    /// the date and the weather under it. Sir can drag it anywhere, onto any monitor; a double
    /// click puts it back under the emblem, with which it then moves again. (Sir, 2026-11-14:
    /// "I want the desktop view to have a clock".)
    fn desk_clock(&mut self, ctx: &egui::Context, iface: &InterfaceSettings, home: Rect, frame: &crate::holo_core::CoreFrame) {
        let now = chrono::Local::now().naive_local();
        let weather = crate::idle_screen::glance().weather;
        // (By the emblem's size at rest, on whichever monitor it stands: its swelling with his
        // voice does not move the clock.)
        let radius = frame.rest;
        let time = crate::idle_screen::clock_text(now, iface.desk_clock_seconds, iface.desk_clock_12h);
        // (Game mode's badge takes the date's line while a game runs.)
        let date = if crate::vram_guard::in_game() { Some("GAME · VRAM 0 MB".to_string()) } else { iface.desk_clock_date.then(|| crate::idle_screen::date_line(now).to_uppercase()) };
        let size = crate::idle_screen::clock_size(ctx, radius, &time, date.is_some(), weather.is_some());
        // Its top middle: where Sir holds it, where he put it, else under the emblem (in the
        // upper part of the screen while the emblem is away in its corner).
        let top = match (self.clock_drag, iface.desk_clock_pos) {
            (Some(at), _) => at,
            (None, Some([x, y])) => home.min + vec2(x * home.width(), y * home.height()),
            (None, None) if frame.dock < 0.5 => crate::hud_lines::top_under(frame.centre, radius),
            (None, None) => pos2(home.center().x, home.top() + home.height() * 0.3),
        };
        // (Kept whole on the monitor it is over.)
        let rect = clock_rect(top, size, |p| self.emblem.room_at(home, p));
        let response = egui::Area::new(egui::Id::new("hud_desk_clock"))
            .order(egui::Order::Background)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(rect, Sense::click_and_drag());
                crate::idle_screen::paint_clock(ui.painter(), rect, radius, &time, date.as_deref(), weather.as_ref());
                response
            })
            .inner;
        if response.dragged() {
            self.clock_drag = Some(clock_dragged(self.clock_drag, rect, response.drag_delta()));
            ctx.request_repaint();
        }
        if response.drag_stopped() && self.clock_drag.take().is_some() {
            // (Where it stands, whole on its monitor.)
            let at = pos2(rect.center().x, rect.top());
            let mut next = iface.clone();
            next.desk_clock_pos = Some([(at.x - home.left()) / home.width(), (at.y - home.top()) / home.height()]);
            info!("[HUD] Sir put the clock at {:.0} %, {:.0} % of his first screen", (at.x - home.left()) / home.width() * 100.0, (at.y - home.top()) / home.height() * 100.0);
            self.set_interface(next);
        }
        if response.double_clicked() && iface.desk_clock_pos.is_some() {
            let mut next = iface.clone();
            next.desk_clock_pos = None;
            self.set_interface(next);
        }
    }

    /// The orb that leaves the background (`wallpaper::Orb`). Spoken to while another program's
    /// window covers the emblem, it flies from the emblem's place to a corner of the monitor
    /// Sir is using, smaller, in front of everything, with what is being said beside it; five
    /// seconds after the last of it, it flies back and is part of the background again. Sir
    /// can drag it anywhere (it comes out there on that monitor from then on); a click opens
    /// the console, a double click brings the board back.
    fn corner_orb_frame(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings) {
        use crate::wallpaper::{OrbPhase, Standing};
        let (centre, radius, rest) = self.emblem_spot;
        // (In orb mode the floating orb stands out in front already: this one stays home.)
        if iface.orb_mode {
            (self.corner_orb, self.orb_out_on, self.orb_landed) = (Default::default(), None, 0);
        }
        if self.backdrop != Standing::On || self.boot.is_some() || radius <= 0.0 || iface.orb_mode {
            crate::wallpaper::set_orb_out(false);
            return;
        }
        let viewport = ViewportId::from_hash_of("jai_corner_orb");
        let ppp = ctx.pixels_per_point();
        let Some(origin) = ctx.input(|i| i.viewport().inner_rect.map(|r| r.min)) else { return };
        let px = |p: Pos2| pos2(((origin.x + p.x) * ppp).round(), ((origin.y + p.y) * ppp).round());
        let home = Rect::from_center_size(px(centre), Vec2::splat((radius * 2.0 * ppp).round()));
        crate::wallpaper::look_at([home.left() as i32, home.top() as i32, home.right() as i32, home.bottom() as i32]);
        let now = Instant::now();
        // Spoken to (his name was heard, he thinks or speaks, or the conversation is open), or
        // at work on something that shows (a badge, an offer waiting for an answer).
        let talking = matches!(m.state.as_str(), "thinking" | "speaking") || (m.state == "listening" && m.window_until.is_some_and(|t| t > now)) || crate::hud_lines::woke_age().is_some_and(|age| age < 2.5);
        // What he is at for longer shows as a ring round the orb: it fills as a timer runs out,
        // and turns while a search runs on (which has no end to count to). ("Progress on the
        // orb", his choice of 2026-11-13: for that long the orb stays out.)
        let timers = self.running_timers();
        // (Up to three timers, a ring and a line each: "PASTA  04:12".)
        let timer_label = (!timers.is_empty()).then(|| timers.iter().take(3).map(|(line, _)| line.as_str()).collect::<Vec<_>>().join("\n"));
        let more_rings: Vec<f32> = timers.iter().skip(1).take(2).filter_map(|(_, share)| *share).collect();
        let progress: Option<Option<f32>> = timers.first().map(|(_, share)| *share).or_else(|| crate::hud_board::researching().then_some(None));
        let working = m.quick.as_ref().is_some_and(|(_, _, until)| *until > now)
            || crate::hud_lines::offered().is_some()
            || progress.is_some()
            // (A question of his, or orders he offers, wait for Sir's word.)
            || crate::ask_first::pending().is_some()
            || crate::did_you_mean::current().is_some();
        let gaming = crate::hud_feel::game_in_front();
        // (A bit smaller than on the background, smaller still beside a game, and as much
        // larger or smaller as Sir made it: the wheel over it, "make the orb smaller". By the
        // emblem at rest: its swell with the music made it glide to a new size without end.)
        let size = (((rest * 2.0 * ppp) * if gaming { 0.3 } else { 0.42 }).clamp(96.0, 168.0) * iface.corner_orb_scale).round() as i32;
        let at_home = self.corner_orb.is_home();
        // (At home with no window of its own: one that has just landed still has one.)
        let was_home = at_home && self.orb_landed == 0;
        let mut out_on = self.orb_out_on;
        // (A scripted session can ask to see it fly although nothing covers its emblem: `JAI_DEMO=orb`.)
        let hidden = crate::wallpaper::orb_called(crate::wallpaper::hidden() || demo("orb"));
        // The monitor it would come out on (a scripted session's: only its own).
        let in_use = if at_home { crate::wallpaper::monitor_in_use(&crate::monitors::recent()) } else { None };
        if was_home && in_use.is_none() {
            return;
        }
        let handled = self.orb_handled.is_some_and(|at| now.duration_since(at) < Duration::from_secs(8));
        let frame = self.corner_orb.step(now, talking || working || handled, hidden, home, || {
            let work = in_use.map_or([0, 0, 1920, 1040], |m| m.work);
            out_on = Some((work, size));
            crate::wallpaper::corner(work, size, crate::wallpaper::spot_on(work))
        });
        self.orb_out_on = out_on;
        crate::wallpaper::set_orb_out(frame.is_some());
        // Home again: its window stands over the emblem for two more frames, until the board
        // has drawn its emblem under it (the board draws it before the orb steps, in `canvas`:
        // let go at once, a frame showed neither).
        let frame = match frame {
            Some(frame) => {
                self.orb_landed = 0;
                frame
            }
            None if (!at_home || self.orb_landed > 0) && self.orb_landed < 2 => {
                self.orb_landed += 1;
                crate::wallpaper::OrbFrame { rect: home, out: 0.0, phase: OrbPhase::Returning }
            }
            None => {
                if std::mem::take(&mut self.orb_landed) > 0 {
                    info!("[DESKTOP] The orb is back in the background");
                    self.orb_menu = None;
                }
                return;
            }
        };
        let last = |who: Speaker| m.messages.iter().rev().find(|l| l.who == who && !l.text.trim().is_empty()).map(|l| l.text.trim().to_string()).unwrap_or_default();
        if was_home {
            if let Some((work, _)) = self.orb_out_on {
                info!("[DESKTOP] The orb leaves the background for the monitor at {},{} (the emblem is covered)", work[0], work[1]);
            }
            // (What was said before it came out is not what it comes out for.)
            self.orb_words = (last(Speaker::Sir), last(Speaker::Jai), now);
        }
        // What is being said, beside it once it rests: Sir's line and his answer, when they are
        // of this exchange.
        let (said, answered) = (last(Speaker::Sir), last(Speaker::Jai));
        let said = if said != self.orb_words.0 { said } else { String::new() };
        let answered = if answered != self.orb_words.1 { answered } else { String::new() };
        let resting = frame.phase == OrbPhase::Out;
        let asked = crate::ask_first::pending().is_some() || crate::did_you_mean::current().is_some();
        // (What is being said stands beside it unless Sir switched that off; a question that
        // waits for his yes or no always does.)
        let words = resting && !gaming && (asked || (iface.orb_words && (!(said.is_empty() && answered.is_empty() && m.state == "idle") || timer_label.is_some())));
        // (His keys, for a few seconds after a right click on it.)
        let menu = resting && !gaming && self.orb_menu.is_some_and(|since| since.elapsed() < Duration::from_secs(8));
        let orb_px = frame.rect.width().round();
        const WORDS_WIDTH: f32 = 380.0;
        // Its window: the orb in the middle, room for the plate on either side (see-through
        // where nothing is painted), so the plate changes sides and comes and goes without the
        // window moving or changing its size under the orb. Beside a game the orb alone.
        let side = if gaming { 0.0 } else { ((12.0 + WORDS_WIDTH) * ppp).round() };
        let window_px = vec2(orb_px + 2.0 * side, if gaming { orb_px } else { orb_px.max(96.0 * ppp) });
        let monitors = crate::monitors::recent();
        let on = monitors.iter().find(|m| m.contains(frame.rect.center().x as i32, frame.rect.center().y as i32));
        // (The plate on the side that has the room: left of an orb in the right half.)
        let words_left = on.is_some_and(|m| frame.rect.center().x as i32 > (m.work[0] + m.work[2]) / 2);
        // Sir moved it by hand: it rests where he let go, and comes out there on that monitor.
        if let Some(actual) = ctx.input(|i| i.raw.viewports.get(&viewport).and_then(|v| v.outer_rect)) {
            let at = pos2((actual.min.x * ppp).round() + side, (actual.min.y * ppp).round());
            if let Some(moved) = self.corner_orb.let_go(at, crate::wallpaper::button_down()) {
                let monitors = crate::monitors::recent();
                if let Some(on) = monitors.iter().find(|m| m.contains(moved.center().x as i32, moved.center().y as i32)) {
                    crate::wallpaper::remember_spot(on.work, crate::wallpaper::spot_of(on.work, orb_px as i32, at));
                    info!("[DESKTOP] Sir put the orb at {:.0},{:.0}: its place on that monitor from now on", at.x, at.y);
                }
            }
        }
        let at = self.corner_orb.resting().map_or(frame.rect.min, |r| r.min) - vec2(side, 0.0);
        let builder = egui::ViewportBuilder::default()
            .with_title(crate::wallpaper::ORB_TITLE)
            .with_app_id("jai")
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_resizable(false)
            .with_window_level(egui::viewport::WindowLevel::AlwaysOnTop)
            .with_position(pos2(at.x / ppp, at.y / ppp))
            .with_inner_size(window_px / ppp);
        // (It comes up hidden, as every window of his.)
        let builder = for_test_session(builder);
        // Its window is looked after from here, not through the viewport's own commands
        // (`win32_desktop::tend_orb`): its black is see-through, beside a game it takes no click
        // (the game keeps its mouse), and it shows itself once it has drawn. Every frame while
        // it is new, then a few times a second.
        if was_home {
            self.orb_tended = (now, now.checked_sub(Duration::from_secs(1)).unwrap_or(now));
            #[cfg(windows)]
            crate::win32_desktop::orb_sets_out();
        }
        let (born, last) = self.orb_tended;
        // (Its flight out begins once its window is on show; it waits a second for that at most.)
        #[cfg(windows)]
        let on_show = crate::win32_desktop::orb_on_show;
        #[cfg(not(windows))]
        let on_show = || wm::own_window(crate::wallpaper::ORB_TITLE).is_some_and(wm::is_visible);
        if now.duration_since(born) < Duration::from_secs(1) && !on_show() {
            self.corner_orb.wait(now);
        }
        if now.duration_since(born) < Duration::from_millis(1500) || now.duration_since(last) > Duration::from_millis(300) {
            self.orb_tended.1 = now;
            // At rest it goes with Sir to the monitor he goes to (his choice of 2026-11-14:
            // "Orb follows you"), and takes the size he gave it, where it stands.
            if let Some(rest) = self.corner_orb.resting() {
                let in_use = crate::wallpaper::monitor_in_use(&monitors).map(|m| m.work);
                let went = in_use.filter(|work| self.orb_monitor.replace(*work).is_some_and(|was| was != *work));
                let to = match (went, on.map(|m| m.work)) {
                    (Some(work), _) => Some(crate::wallpaper::corner(work, size, crate::wallpaper::spot_on(work))),
                    (None, Some(work)) if (rest.width() - size as f32).abs() > 0.5 => Some(crate::wallpaper::corner(work, size, Some(crate::wallpaper::spot_of(work, rest.width() as i32, rest.min)))),
                    _ => None,
                };
                if let Some(to) = to {
                    self.corner_orb.follow(now, to);
                }
            }
            tend_orb_window(ctx, viewport, gaming);
        }
        let content = CornerOrbContent {
            orb_px,
            progress,
            more_rings,
            words,
            words_left,
            menu,
            said,
            answered,
            timer: timer_label,
            state: m.state.clone(),
            live_words: iface.live_words,
            // What waits for his word: a question before a guess is carried out, or the
            // orders closest to one he did not know.
            question: crate::ask_first::pending().map(|(question, _)| question),
            choices: crate::did_you_mean::current().map(|offer| offer.choices.into_iter().map(|c| c.label).collect()).unwrap_or_default(),
            game: crate::vram_guard::in_game(),
        };
        let mut did = OrbClick::Nothing;
        let emblem = &self.emblem;
        ctx.show_viewport_immediate(viewport, builder, |ctx, _class| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| did = corner_orb_ui(ui, emblem, &content));
            crate::live_shots::tick(ctx);
        });
        match did {
            OrbClick::Board => show_the_board(&self.rt),
            OrbClick::Console => {
                self.console_open = true;
                self.console_focus = true;
            }
            OrbClick::Drag if self.corner_orb.grabbed() => ctx.send_viewport_cmd_to(viewport, ViewportCommand::StartDrag),
            OrbClick::Drag => {}
            OrbClick::Yes => self.command(HudCommand::Chat { text: "yes".into() }),
            OrbClick::No => crate::ask_first::drop_it(),
            OrbClick::Choice(k) => crate::did_you_mean::pick(k),
            OrbClick::Menu => {
                self.orb_menu = if menu { None } else { Some(now) };
                self.orb_handled = Some(now);
            }
            OrbClick::Windows => {
                windows_on_call();
            }
            OrbClick::Quiet => crate::listening_lock::apply(crate::listening_lock::LockCommand::Pause(Duration::from_secs(600))),
            OrbClick::Wheel(larger) => {
                self.orb_handled = Some(now);
                self.app.config().update(|c| c.interface.corner_orb_scale = crate::interface::corner_orb_scale(c.interface.corner_orb_scale, if larger { "bigger" } else { "smaller" }));
            }
            OrbClick::Nothing => {}
        }
        // (A key of the menu was taken: the menu goes.)
        if !matches!(did, OrbClick::Nothing | OrbClick::Menu | OrbClick::Drag | OrbClick::Wheel(_)) {
            self.orb_menu = None;
        }
        // (The board draws on while it is out: every frame while it flies; as smoothly as the
        // board itself while he listens, thinks, speaks or a search turns its ring (at twenty
        // frames a second its rings went round in steps); as the board at rest while it only
        // waits or a timer fills its ring; beside a game a few frames are enough.)
        let lively = talking || progress == Some(None);
        let next = Duration::from_millis(if !resting { 8 } else if gaming { 50 } else if lively { 16 } else { 33 });
        ctx.request_repaint_after(next);
        board_due_in(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame is judged by the median of its neighbours: frames the scheduler held up do not
    /// make a fast HUD slow, and a HUD slow itself stays slow.
    /// A slow frame's parts: each from its mark to the next (the last to the frame's end),
    /// a part marked twice added up, the longest first.
    #[test]
    fn a_frames_parts_add_up_and_the_longest_leads() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let parts = parts_of(&[("start", at(0)), ("board", at(2)), ("small views", at(10)), ("earth", at(12)), ("small views", at(600)), ("end", at(604))], at(605));
        assert_eq!(parts[0].0, "earth");
        assert!((parts[0].1 - 588.0).abs() < 0.5, "{parts:?}");
        let small = parts.iter().find(|(n, _)| *n == "small views").unwrap().1;
        assert!((small - 6.0).abs() < 0.5, "two stretches of the same part add up: {small}");
        assert!(parts_of(&[], at(5)).is_empty());
    }

    /// egui's context lock is not re-entrant: a call on the context inside one of the
    /// context's own closures waits for itself forever. Two of them froze the whole HUD on
    /// 2026-11-16 (every window of his own, and the comms caption); Sir had to end him in the
    /// Task Manager. (One line at a time: the closures over several lines were read by hand.)
    #[test]
    fn no_context_call_waits_inside_the_contexts_own_lock() {
        let mut nested = Vec::new();
        for path in std::fs::read_dir("src").unwrap().flatten().map(|e| e.path()) {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for (n, line) in text.lines().enumerate() {
                for name in ["data", "data_mut", "memory", "memory_mut", "input", "input_mut", "viewport", "options", "options_mut", "fonts", "interaction_snapshot"] {
                    let needle = format!(".{name}(|");
                    for (at, _) in line.match_indices(needle.as_str()) {
                        let body = &line[at + needle.len()..];
                        let mut depth = 1;
                        let end = body.char_indices().find(|&(_, c)| {
                            depth += match c { '(' => 1, ')' => -1, _ => 0 };
                            depth == 0
                        });
                        let body = &body[..end.map_or(body.len(), |(i, _)| i)];
                        if body.contains("ctx.") || body.contains("ctx().") {
                            nested.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(nested.is_empty(), "the context is called inside its own lock at {nested:?}");
    }

    /// The Esc check that every window of his own runs on each of its frames.
    #[test]
    fn a_window_of_his_own_finishes_its_frame() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ctx = egui::Context::default();
            let _ = ctx.run(egui::RawInput::default(), esc_closes_window);
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "the frame froze in esc_closes_window");
    }

    // ---- The gallery: what the look-and-feel pieces of 2026-11-12 draw, rendered offscreen as
    // PNGs (target/ui-snapshots/gallery_*.png) to be looked at. Each asserts only that it drew.

    fn gallery(name: &str, size: Vec2, frames: usize, mut ui: impl FnMut(&egui::Context)) {
        let _look = crate::holo_theme::look_for_pictures();
        let mut snap = crate::ui_snapshot::Snapshot::new(size);
        let canvas = snap.render(frames, |ctx| {
            paint_canvas(ctx);
            ui(ctx);
        });
        canvas.save(name);
        assert!(canvas.brightness(Rect::from_min_size(Pos2::ZERO, size)) > 1.0, "{name}: nothing was drawn");
    }

    /// The board to itself for a picture: no block of another test, and so no leader to one.
    fn board_alone() -> std::sync::MutexGuard<'static, ()> {
        let alone = crate::hud_dock::test_lock();
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
        alone
    }

    const GALLERY: Vec2 = Vec2::new(1280.0, 760.0);

    fn emblem_at_rest() -> Core {
        let mut emblem = Core::new(crate::holo_core::Pose::Front);
        emblem.tick(1.0 / 60.0, 0.0, false);
        emblem
    }

    /// Tool blocks: the header every block wears, and what a block shows while it loads, has
    /// nothing to show, or failed.
    #[test]
    fn gallery_blocks_and_their_states() {
        use crate::holo_block::{Lamp, Outline};
        let _g = crate::hud_dock::test_lock();
        let ids = ["gallery_stats", "gallery_picture", "gallery_failed", "gallery_empty", "gallery_list", "gallery_chart"];
        for (id, light) in ids.iter().zip([Lamp::Good, Lamp::Busy, Lamp::Fault, Lamp::Rest, Lamp::Busy, Lamp::Warn]) {
            crate::holo_block::set_lamp(id, light);
        }
        // (Past their opening and the glide to the size of what they show: blocks fit their content.)
        let mut frame = 0;
        gallery("gallery_blocks", GALLERY, 24, |ctx| {
            frame += 1;
            if frame == 8 || frame == 16 {
                std::thread::sleep(Duration::from_millis(450));
            }
            crate::hud_dock::begin_frame();
            let p = Palette::new(&InterfaceSettings::default());
            hud_block_window(ctx, ids[0], "System telemetry", [400.0, 240.0], |ui| {
                for (key, value, share) in [("CPU", "23.4% · 16 threads", 0.23), ("RAM", "18.2 / 31.9 GB", 0.57), ("RTX 4070", "52°C · 12%", 0.12), ("VRAM", "3.1 / 12.0 GB", 0.26)] {
                    row(ui, key, value.to_string());
                    meter(ui, share, 6.0, p.accent, &[]);
                }
            });
            hud_block_window(ctx, ids[1], "Pictures // Eiffel Tower", [400.0, 240.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::holo_block::outline(ui, room.shrink(6.0), Outline::Picture);
            });
            hud_block_window(ctx, ids[2], "3D studio // apple", [400.0, 240.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::holo_block::failed(ui, room, "The model could not be downloaded (no connection).");
            });
            hud_block_window(ctx, ids[3], "Notes", [400.0, 220.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::holo_block::empty(ui, room, "No notes yet. Say: add a note saying call Tom at five.");
            });
            hud_block_window(ctx, ids[4], "Processes", [400.0, 220.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::holo_block::outline(ui, room.shrink(6.0), Outline::List);
            });
            hud_block_window(ctx, ids[5], "Network", [400.0, 220.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::holo_block::outline(ui, room.shrink(6.0), Outline::Chart);
            });
        });
        for id in ids {
            crate::hud_dock::forget(id);
        }
    }

    #[test]
    fn gallery_settings_panel() {
        let _g = crate::hud_dock::test_lock();
        settings_panel::open(true);
        gallery("gallery_settings", GALLERY, 8, |ctx| {
            crate::hud_dock::begin_frame();
            let _ = settings_panel::show(ctx, &InterfaceSettings::default());
        });
        settings_panel::open(false);
        crate::hud_dock::forget(settings_panel::ID);
    }

    /// The cards that come up beside the core: the song, the day's lessons, a result with a
    /// chart, a tool being built.
    #[test]
    fn gallery_popup_cards() {
        let _alone = board_alone();
        use crate::dynamic_popups::{self as popups, PopupPayload};
        let ids = [
            popups::spawn(PopupPayload::NowPlayingMediaCard { title: "Highway to Hell".into(), artist: "AC/DC".into(), playlist: Some("Rock".into()), player_name: "Sonora".into(), progress: 0.4, is_playing: true }, None),
            popups::spawn(PopupPayload::ScheduleAndCalendarCard { title: "Today".into(), entries: vec![("08:00".into(), "Maths".into()), ("09:45".into(), "Physics".into()), ("11:30".into(), "English".into())], highlight_index: Some(1) }, None),
            popups::spawn(PopupPayload::VisualDataCard { title: "Kinematics".into(), markdown_or_code: "v = 12 m/s after 3 s".into(), chart_points: vec![0.0, 4.0, 8.0, 12.0], action_buttons: vec!["Copy".into()] }, None),
            popups::spawn(PopupPayload::ToolForgeLiveCard { tool_name: "playlist_player".into(), steps_summary: vec!["scan the library".into(), "write the tool".into()], status: "verified".into() }, None),
        ];
        gallery("gallery_popups", GALLERY, 40, |ctx| {
            let _ = popups::show(ctx, Palette::new(&InterfaceSettings::default()).card_style(), 1.0 / 30.0);
        });
        for id in ids {
            popups::manager().dismiss(id);
        }
    }

    /// Wheeled to its end: the group of the top rows stays named at the top, and the game list
    /// under "Game mode" (stand-in games).
    #[test]
    fn gallery_settings_scrolled_to_the_games() {
        let _g = crate::hud_dock::test_lock();
        settings_panel::open(true);
        *settings_panel::GAMES.lock().unwrap() = Some(vec![("Stand-in Racer".into(), "StandInRacer".into()), ("Stand-in Quest".into(), "StandInQuest".into())]);
        let _look = crate::holo_theme::look_for_pictures();
        let mut snap = crate::ui_snapshot::Snapshot::new(GALLERY);
        let mut last = None;
        for k in 0..40 {
            let mut input = egui::RawInput { screen_rect: Some(snap.screen()), time: Some(k as f64 / 60.0), predicted_dt: 1.0 / 60.0, ..Default::default() };
            if k == 4 {
                input.events = vec![egui::Event::PointerMoved(pos2(260.0, 400.0)), egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -6000.0), modifiers: egui::Modifiers::NONE }];
            }
            last = Some(snap.frame(input, |ctx| {
                paint_canvas(ctx);
                crate::hud_dock::begin_frame();
                let _ = settings_panel::show(ctx, &InterfaceSettings::default());
            }));
        }
        let out = last.expect("frames");
        snap.rasterise(out.shapes, out.pixels_per_point).save("gallery_settings_games");
        settings_panel::open(false);
        crate::hud_dock::forget(settings_panel::ID);
    }

    /// The background's rows, found by their word, with a path field for every monitor and one
    /// for each of two (the second holding a video).
    #[test]
    fn gallery_settings_background_media() {
        let _g = crate::hud_dock::test_lock();
        let _look = crate::holo_theme::look_for_pictures();
        let screen = |index: usize, x: i32| crate::monitors::Monitor { index, name: String::new(), rect: [x, 0, x + 1920, 1080], work: [x, 0, x + 1920, 1032], primary: index == 1 };
        crate::monitors::pretend(Some(vec![screen(1, 0), screen(2, 1920)]));
        settings_panel::open(true);
        let mut iface = InterfaceSettings::default();
        iface.bg_media.insert("2".into(), "D:\\Videos\\rain on glass.mp4".into());
        let mut snap = crate::ui_snapshot::Snapshot::new(GALLERY);
        let mut last = None;
        for k in 0..30 {
            let input = egui::RawInput { screen_rect: Some(snap.screen()), time: Some(k as f64 / 60.0), predicted_dt: 1.0 / 60.0, ..Default::default() };
            last = Some(snap.frame(input, |ctx| {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("jai_settings_search"), "background".to_string()));
                paint_canvas(ctx);
                crate::hud_dock::begin_frame();
                let _ = settings_panel::show(ctx, &iface);
            }));
        }
        let out = last.expect("frames");
        snap.rasterise(out.shapes, out.pixels_per_point).save("gallery_settings_background");
        settings_panel::open(false);
        crate::hud_dock::forget(settings_panel::ID);
        crate::monitors::pretend(None);
    }

    #[test]
    fn gallery_notifications_and_edge_light() {
        let _alone = board_alone();
        use crate::hud_notify::{Card, Edge};
        gallery("gallery_notifications", GALLERY, 2, |ctx| {
            crate::hud_notify::edge_held(Edge::Notice);
            let area = Rect::from_min_max(pos2(0.0, 56.0), pos2(GALLERY.x - 16.0, GALLERY.y));
            let cards = [
                Card { text: "Timer done: tea.", alert: false, age: 3.2, left: 1.8 },
                Card { text: "The GPU is at 91 °C. I have turned the fans up and closed the benchmark.", alert: true, age: 1.0, left: 7.0 },
                Card { text: "New mail from the school office: the timetable changes on Friday.", alert: false, age: 0.09, left: 4.9 },
            ];
            crate::hud_notify::paint_stack(ctx, area, &cards);
            crate::hud_notify::paint_edge(ctx, ctx.screen_rect(), 1.0);
        });
    }

    /// The emblem with Sir's words under it, the music's spectrum round it and the
    /// microphone's verdict over it; then what he is working on, the song's line and an offer.
    #[test]
    fn gallery_lines_under_the_emblem() {
        let _alone = board_alone();
        let emblem = emblem_at_rest();
        crate::hud_lines::dismiss_offer();
        crate::hud_lines::heard("Jay, open the mixer and turn the music");
        let draw = |ctx: &egui::Context, state: &'static str, lyric: Option<&str>, spectrum: bool| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let screen = ctx.screen_rect();
                let painter = ui.painter_at(screen);
                emblem.paint(&painter, screen);
                let frame = emblem.frame(screen);
                if spectrum {
                    crate::hud_lines::paint_mic_label(&painter, frame.centre, frame.radius, "TOO QUIET", 1.0);
                } else {
                    crate::holo_core::ripple(&painter, frame.centre, frame.radius, 0.2);
                }
                crate::hud_lines::paint(ui, crate::hud_lines::top_under(frame.centre, frame.radius), 640.0, state, true, lyric);
            });
        };
        gallery("gallery_live_words", GALLERY, 3, |ctx| draw(ctx, "listening", Some("And I think to myself, what a wonderful world"), true));
        // (A step shows once the turn has run a moment.)
        crate::hud_lines::step("get_calendar_schedule");
        gallery("gallery_tmp", GALLERY, 1, |ctx| draw(ctx, "thinking", None, false));
        std::thread::sleep(Duration::from_millis(420));
        crate::hud_lines::offer("open Spotify");
        gallery("gallery_steps_and_offer", GALLERY, 2, |ctx| draw(ctx, "thinking", None, false));
        crate::hud_lines::dismiss_offer();
        crate::hud_lines::took(0, true);
        crate::hud_lines::speech_over();
    }

    /// The desktop clock is dragged from one monitor onto the next: each frame's drag began where
    /// it was kept whole on the first, and it stuck at that monitor's edge.
    #[test]
    fn the_clock_is_dragged_onto_the_next_monitor() {
        let (a, b) = (Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 800.0)), Rect::from_min_max(pos2(1000.0, 0.0), pos2(2000.0, 800.0)));
        let room_at = |p: Pos2| if p.x < 1000.0 { a } else { b };
        let size = vec2(200.0, 120.0);
        let (mut held, mut shown) = (None, clock_rect(pos2(850.0, 100.0), size, room_at));
        for _ in 0..6 {
            let at = clock_dragged(held, shown, vec2(60.0, 0.0));
            (held, shown) = (Some(at), clock_rect(at, size, room_at));
        }
        assert!(b.contains_rect(shown), "{shown:?}");
    }

    /// The wheel over the corner orb steps by whole notches: a touchpad's small turns add up to
    /// one (each frame with a turn in it was a whole step, and it ran to its largest at once).
    #[test]
    fn a_timer_of_his_own_name_says_it() {
        assert_eq!(timer_word("pasta timer", true), "PASTA");
        assert_eq!(timer_word("nudel timer", true), "NUDEL");
        assert_eq!(timer_word("5 minute timer", true), "TIMER");
        assert_eq!(timer_word("Timer", true), "TIMER");
        assert_eq!(timer_word("", false), "STOPWATCH");
        assert_eq!(timer_word("Stopwatch", false), "STOPWATCH");
    }

    #[test]
    fn the_orb_wheel_steps_by_whole_notches() {
        let wheel = |unit, y: f32| egui::Event::MouseWheel { unit, delta: vec2(0.0, y), modifiers: egui::Modifiers::NONE };
        assert_eq!(wheel_turn(0.0, &[wheel(egui::MouseWheelUnit::Line, 1.0)]), (0.0, Some(true)));
        assert_eq!(wheel_turn(0.0, &[wheel(egui::MouseWheelUnit::Line, -1.0)]), (0.0, Some(false)));
        let (mut turned, mut steps) = (0.0, 0);
        // Twenty frames of a touchpad's five points: a hundred points, two notches and a half.
        for _ in 0..20 {
            let (left, step) = wheel_turn(turned, &[wheel(egui::MouseWheelUnit::Point, 5.0)]);
            (turned, steps) = (left, steps + step.is_some() as u32);
        }
        assert_eq!((steps, turned), (2, 0.5));
    }

    /// The core close up, as the floating orb shows it (lines alone: its black is see-through).
    #[test]
    fn gallery_emblem_close_up() {
        let _alone = board_alone();
        let emblem = emblem_at_rest();
        gallery("gallery_emblem", vec2(900.0, 900.0), 2, |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let screen = ctx.screen_rect();
                emblem.paint_in_crisp(&ui.painter_at(screen), screen);
            });
        });
    }

    /// The main screen as it stands at rest and while he speaks: the top rail with one thing
    /// open, the state plate over the voice-lit rule, the core (each of the two), the COMMS
    /// plate, and the bottom strip with its meter, chip and key hints.
    #[test]
    fn gallery_main_screen() {
        // (No block of another test's stands on the stage: the canvas would tie leaders to it.)
        let _g = crate::hud_dock::test_lock();
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
        const SCREEN: Vec2 = Vec2::new(1920.0, 1080.0);
        for (name, style, word, voice, level) in [("gallery_main_simple", crate::holo_core::CoreStyle::Simple, "LISTENING", 0.0, -38.0), ("gallery_main_complex", crate::holo_core::CoreStyle::Complex, "SPEAKING", 0.7, -61.0)] {
            let mut emblem = emblem_at_rest();
            emblem.style = style;
            for _ in 0..20 {
                emblem.tick(1.0 / 60.0, voice, false);
            }
            let reply = if voice > 0.0 { "Three things today, Sir: Physics at 08:00 in room 204, the dentist at 14:30, and the parcel arrives before six." } else { "" };
            gallery(name, SCREEN, 2, |ctx| {
                egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let painter = ui.painter_at(screen);
                    emblem.paint(&painter, screen);
                    // The rail, as `header_strip` lays it out and letters it.
                    let lit = HeaderState { core: style == crate::holo_core::CoreStyle::Complex, ..Default::default() };
                    let items = header_items(&lit, "REBOOT", "DEFAULT CAMERA");
                    let row = header_layout(&items, screen.left() + 12.0, screen.right() - 12.0, screen.top() + 10.0, |t| rail_width(&painter, t));
                    let taken: Vec<Rect> = row.placed.iter().map(|(_, r)| *r).collect();
                    paint_seams(&painter, &row);
                    for (id, rect) in row.placed {
                        let item = items.iter().find(|i| i.id == id).expect("laid-out item");
                        if !item.boxed {
                            holo::text(&painter, rect.left_center(), Align2::LEFT_CENTER, Face::Label, &item.text, RAIL_TEXT, holo::theme().faint());
                        } else if id == "source" {
                            rail_picker(&painter, rect, &item.text, 0.0);
                        } else {
                            rail_key(&painter, rect, &item.text, 0.0, item.on, holo::theme().accent());
                        }
                    }
                    header_rule(&painter, screen, voice);
                    if let Some(plate) = pill_rect(&painter, screen, &taken) {
                        paint_pill(&painter, plate.center(), word, 1.0);
                    }
                    if let Some(block) = crate::hud_board::dialogue_block("what is on today", reply, false, "14:02") {
                        let (width, centre_x) = comms_placement(screen, 0.0);
                        let bottom = screen.bottom() - 12.0 - TELEMETRY_H - 6.0;
                        crate::hud_board::paint_dialogue(&painter, Rect::from_min_size(pos2(centre_x - width / 2.0, bottom - (CAPTION_BAND - 8.0)), vec2(width, CAPTION_BAND - 8.0)), &block);
                    }
                    // The bottom strip: the meter and its chip, what he heard, the key hints.
                    let y = screen.bottom() - 10.0 - TELEMETRY_H;
                    let chip = paint_strip_meter(&painter, pos2(screen.left() + 14.0, y), level, level + 6.0, false, false);
                    holo::text(&painter, pos2(chip.right() + 14.0, y + TELEMETRY_H / 2.0), Align2::LEFT_CENTER, Face::Body, "what is on today", 12.5, holo::a(holo::theme().ink(), 0.75));
                    for (label, rect) in key_hints(&painter, key_hints_right(screen, 0.0), y + TELEMETRY_H / 2.0) {
                        paint_key_hint(&painter, rect, label, false);
                    }
                });
            });
        }
    }

    /// The tour lighting the emblem, as "In the middle stands my emblem" is said.
    #[test]
    fn gallery_tour_light() {
        let _alone = board_alone();
        let emblem = emblem_at_rest();
        gallery("gallery_tour", GALLERY, 2, |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let screen = ctx.screen_rect();
                emblem.paint(&ui.painter_at(screen), screen);
                tour_light(&ui.painter_at(screen), Rect::from_center_size(screen.center(), vec2(300.0, 300.0)), crate::hud_board::active_colour());
            });
        });
    }

    #[test]
    fn gallery_idle_screen_and_startup() {
        let _alone = board_alone();
        use crate::startup_sequence::Kind;
        let emblem = emblem_at_rest();
        let base = |ctx: &egui::Context| -> crate::holo_core::CoreFrame {
            let screen = ctx.screen_rect();
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| emblem.paint(&ui.painter_at(screen), screen));
            emblem.frame(screen)
        };
        let _turn = crate::idle_screen::GLANCE_TURN.lock().unwrap_or_else(|e| e.into_inner());
        crate::idle_screen::show_glance(crate::idle_screen::Glance { next: Some(("Physics".into(), chrono::Local::now().naive_local() + chrono::Duration::minutes(25), "204".into())), weather: Some((7.0, "light rain".into())), lesson: None });
        gallery("gallery_idle", GALLERY, 2, |ctx| {
            let f = base(ctx);
            crate::idle_screen::paint(ctx, ctx.screen_rect(), f.centre, f.radius, 1.0);
        });
        // The desktop clock under the emblem (as the background): the time alone with its
        // seconds in twelve hours, and with the date and the weather.
        let noon = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap().and_hms_opt(21, 41, 7).unwrap();
        for (name, time, date, weather) in [
            ("gallery_desk_clock", crate::idle_screen::clock_text(noon, false, false), Some(crate::idle_screen::date_line(noon).to_uppercase()), Some((7.0, "light rain  ·  sunset 16:52".to_string()))),
            ("gallery_desk_clock_plain", crate::idle_screen::clock_text(noon, true, true), None, None),
        ] {
            gallery(name, GALLERY, 2, |ctx| {
                let f = base(ctx);
                let size = crate::idle_screen::clock_size(ctx, f.radius, &time, date.is_some(), weather.is_some());
                let top = crate::hud_lines::top_under(f.centre, f.radius);
                let rect = Rect::from_min_size(pos2(top.x - size.x / 2.0, top.y), size);
                assert!(ctx.screen_rect().contains_rect(rect), "{name}: the clock stands whole under the emblem: {rect:?}");
                crate::idle_screen::paint_clock(&ctx.layer_painter(egui::LayerId::background()), rect, f.radius, &time, date.as_deref(), weather.as_ref());
            });
        }
        // (The engine and the ears are up, the voice and the brain still loading.)
        crate::startup_sequence::up(0);
        crate::startup_sequence::up(1);
        for (name, kind, ms) in [("gallery_startup_long_a", Kind::Long, 2400), ("gallery_startup_long_b", Kind::Long, 3950), ("gallery_startup_short", Kind::Short, 900)] {
            gallery(name, GALLERY, 2, |ctx| {
                let f = base(ctx);
                crate::startup_sequence::paint(ctx, ctx.screen_rect(), f.centre, f.radius, kind, Duration::from_millis(ms));
            });
        }
    }

    #[test]
    fn gallery_mini_strip_and_subtitles() {
        let _alone = board_alone();
        let strip = MiniStrip {
            word: "SPEAKING",
            sir: "Jay, what is on my calendar tomorrow?".into(),
            jai: "Two things, Sir: physics at 8:35 in room 204 and the dentist at half past two.".into(),
            timers: vec!["TIMER  04:32".into()],
            clock: "15:42".into(),
        };
        gallery("gallery_mini_strip", vec2(1600.0, MINI_STRIP_HEIGHT), 2, |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                paint_mini_strip(ui, &strip);
            });
        });
        let lines = [("So the first thing we need is a plan for the weekend.".to_string(), 0.7), ("And then we look at the weather, because nobody wants to stand in the rain.".to_string(), 1.0)];
        gallery("gallery_subtitles", vec2(1000.0, 110.0), 2, |ctx| crate::live_subtitles::paint_bar(ctx, &lines, None));
        // The corner orb with what is being said beside it, with a question of his, and with
        // the orders he offers (over a light ground, as over a bright window of Sir's).
        let emblem = Core::new(crate::holo_core::Pose::Front);
        let orb = |name: &str, c: CornerOrbContent| {
            gallery(name, vec2(952.0, 168.0), 2, |ctx| {
                ctx.layer_painter(egui::LayerId::background()).rect_filled(ctx.screen_rect(), Rounding::ZERO, Color32::from_rgb(0xD8, 0xDC, 0xE0));
                egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                    corner_orb_ui(ui, &emblem, &c);
                });
            });
        };
        let content = |said: &str, answered: &str| CornerOrbContent {
            orb_px: 168.0,
            progress: None,
            more_rings: Vec::new(),
            words: true,
            words_left: false,
            menu: false,
            said: said.into(),
            answered: answered.into(),
            timer: None,
            state: "speaking".into(),
            live_words: false,
            question: None,
            choices: Vec::new(),
            game: false,
        };
        orb("gallery_corner_orb_game", CornerOrbContent { words: false, game: true, ..content("", "") });
        orb("gallery_corner_orb_words", CornerOrbContent { progress: Some(Some(0.35)), more_rings: vec![0.7, 0.1], timer: Some("TIMER  03:12\nPASTA  07:40\nPIZZA  11:02".into()), ..content("Jay, what is on my calendar tomorrow?", "Two things, Sir: physics at 8:35 in room 204 and the dentist at half past two.") });
        orb("gallery_corner_orb_question", CornerOrbContent { question: Some("Did you mean: create a protocol called 'evening', Sir?".into()), ..content("Jay, save that as evening.", "") });
        orb("gallery_corner_orb_choices", CornerOrbContent { choices: vec!["save this desk layout as evening".into(), "show my desk layouts".into(), "restore the desk layout evening".into()], ..content("Jay, keep the desk like this.", "") });
        // In the right half of its monitor the plate stands left of it; a right click on the
        // orb puts its keys there.
        orb("gallery_corner_orb_words_left", CornerOrbContent { words_left: true, ..content("Jay, what time is it?", "A quarter past nine, Sir.") });
        orb("gallery_corner_orb_menu", CornerOrbContent { words: false, words_left: true, menu: true, ..content("", "") });
    }

    /// The kit's blocks as they stand on the board: the changes list and the coach, then the
    /// privacy board, the hearing log and an explanation (pictures for the eye, and the checks
    /// every block is held to).
    #[test]
    fn gallery_kit() {
        let _g = crate::hud_dock::test_lock();
        let _k = crate::kit::test_lock();
        crate::changes::open(true);
        let _ = crate::kit_voice::coach(&serde_json::json!({"action": "start"}));
        crate::kit_voice::coach_heard("They open this card.");
        gallery("gallery_kit_changes_coach", vec2(1920.0, 1080.0), 10, |ctx| {
            crate::hud_dock::begin_frame();
            crate::kit::show(ctx);
        });
        crate::changes::open(false);
        let _ = crate::kit_voice::coach(&serde_json::json!({"action": "stop"}));
        crate::kit_insight::open_for_picture(true);
        let _ = crate::kit_voice::hearing(&serde_json::json!({"action": "show"}));
        gallery("gallery_kit_privacy_hearing", vec2(1920.0, 1080.0), 10, |ctx| {
            crate::hud_dock::begin_frame();
            crate::kit::show(ctx);
        });
        crate::kit_insight::open_for_picture(false);
        crate::hud_dock::forget(crate::kit_voice::HEARING);
    }

    /// The column of system blocks as the HUD lays it out (`system_column`): a long note and a
    /// short one, where everything fits and where it is taller than the screen.
    #[test]
    fn gallery_system_column() {
        let _g = crate::hud_dock::test_lock();
        let lines = [
            "Afternoon briefing: Friday 13 November, 14:49",
            "School (today)",
            "• 08:00 - 09:30: Physics (Room 12)",
            "• 09:45 - 12:00: Lesson [SUBSTITUTION / ROOM CHANGE], note: Assembly Hall / Assembly Hall / Assembly Hall",
            "• 09:45 - 11:15: German, basic course (Room 14) [CANCELLED]",
            "• 11:30 - 12:15: Art, basic course (Room 21) [CANCELLED]",
            "• 12:20 - 13:05: Art, basic course (Room 21)",
            "itslearning",
            "• Unavailable: signed in through the school's single sign-on, but the course data refused the session: it has to be opened in the browser once.",
            "Calendar",
            "• Nothing else today.",
        ];
        for (name, height) in [("gallery_system_column_fits", 900.0), ("gallery_system_column_tall", 420.0)] {
            gallery(name, vec2(700.0, height), 6, |ctx| {
                let screen = ctx.screen_rect();
                let top = screen.top() + 12.0 + HEADER_H + 14.0;
                egui::Area::new(egui::Id::new("hud_system_column")).default_pos(pos2(screen.left() + 24.0, top)).order(egui::Order::Middle).show(ctx, |ui| {
                    ui.set_width(SYSTEM_COLUMN);
                    column_scroll(ui, screen.height() - top - 90.0, |ui| {
                        crate::holo_ui::titled_plate(ui, "WIDGET // DAILY BRIEFING SUMMARY", "NOTE", |ui| {
                            for line in lines {
                                ui.add(egui::Label::new(RichText::new(line).size(13.5).color(strip_ink())).wrap());
                                ui.add_space(4.0);
                            }
                        });
                        crate::holo_ui::titled_plate(ui, "WIDGET // A SHORT ONE", "NOTE", |ui| {
                            ui.label(RichText::new("One line.").size(13.5).color(strip_ink()));
                        });
                    });
                });
            });
        }
    }

    #[test]
    fn gallery_voice_setup() {
        let _g = crate::hud_dock::test_lock();
        crate::voice_setup::begin();
        crate::voice_setup::heard("Jay, what time is it?", None, (-20.0, -50.0));
        crate::voice_setup::heard("Jay open the mix", None, (-20.0, -50.0));
        crate::voice_setup::show_trace(&(0..72).map(|k| (0.5 + 0.5 * (k as f32 * 0.55).sin()) * (0.25 + 0.75 * (k as f32 / 72.0))).collect::<Vec<f32>>());
        gallery("gallery_voice_setup", GALLERY, 8, |ctx| {
            crate::hud_dock::begin_frame();
            crate::voice_setup::show(ctx, 0.0);
        });
        crate::voice_setup::cancel();
        crate::hud_dock::forget(crate::voice_setup::ID);
    }

    #[test]
    fn the_strips_return_when_the_pointer_nears_the_top_or_the_bottom() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1440.0));
        assert!(near_strips(pos2(1200.0, 40.0), screen) && near_strips(pos2(30.0, 1400.0), screen));
        assert!(!near_strips(pos2(1200.0, 700.0), screen));
        assert!(!near_strips(pos2(3000.0, 20.0), screen), "another monitor's top is not this screen's");
        assert!(CALM_REST > 0.1 && CALM_REST < 0.5, "stepped back, still findable");
    }

    #[test]
    fn frame_costs_are_judged_on_the_median() {
        assert_eq!(frame_summary(&[]), None);
        let mut held_up = vec![0.7; 30];
        held_up.extend([25.0; 10]);
        let (median, average, worst) = frame_summary(&held_up).unwrap();
        assert_eq!((median, worst), (0.7, 25.0));
        assert!(average > 5.5 && median < 5.5, "{average} / {median}");
        let (median, ..) = frame_summary(&[6.0; 40]).unwrap();
        assert_eq!(median, 6.0, "slow every frame: slow");
        assert_eq!(frame_summary(&[1.0, 4.0, 2.0, 3.0]).map(|s| s.0), Some(2.5));
        assert_eq!(frame_summary(&[3.0, 1.0, 2.0]).map(|s| s.0), Some(2.0));
    }

    /// He opens as the desktop's background wherever there is a rect for it, with every setting
    /// as a new install has it and in pure orb mode too (until 2026-11-20 a new install, such as a
    /// friend's PC, opened as a fullscreen window: the background was a switch, off at first).
    /// Without one (a scripted session without a monitor): `--monitor 2`, the fullscreen board
    /// covers that monitor from its first frame and a window starts in its middle; without it,
    /// the board is the primary's at (0, 0). A board across every monitor is turned off for such
    /// a session in `main` (its settings stay in memory).
    #[test]
    fn a_test_session_opens_on_the_monitor_it_names() {
        let second = Rect::from_min_size(pos2(2560.0, 0.0), vec2(2560.0, 1440.0));
        let windowed = vec2(1500.0, 940.0);
        for iface in [InterfaceSettings::default(), InterfaceSettings { orb_mode: true, ..InterfaceSettings::default() }, InterfaceSettings { mini_strip: true, ..InterfaceSettings::default() }] {
            let g = startup_geometry(&iface, Some([0, 0, 2560, 1440]), windowed, Some(second));
            assert!(!g.fullscreen && !g.transparent && g.position == Some([0.0, 0.0]), "{g:?}");
        }
        let board = InterfaceSettings { fullscreen: true, orb_mode: false, span_all_monitors: false, ..InterfaceSettings::default() };
        let g = startup_geometry(&board, None, windowed, Some(second));
        assert!(g.fullscreen && !g.transparent);
        assert_eq!((g.size, g.position), (vec2(2560.0, 1440.0), Some([2560.0, 0.0])));
        assert_eq!(startup_geometry(&board, None, windowed, None).position, Some([0.0, 0.0]));
        let window = InterfaceSettings { fullscreen: false, ..board.clone() };
        let g = startup_geometry(&window, None, windowed, Some(second));
        assert!(!g.fullscreen && g.transparent && g.size == windowed);
        assert_eq!(g.position, Some([2560.0 + 1280.0 - 750.0, 720.0 - 470.0]));
        assert_eq!(startup_geometry(&window, None, windowed, None).position, None, "without --monitor, where Windows puts it");
    }

    /// The floating orb is a window of its own over the background he always is (Sir,
    /// 2026-11-20: "the floating orb and so should still exist and be functinal"): at its
    /// corner of the monitor's work area, the strip across its top, where Sir left it after a
    /// drag (the size changing round that centre), and in the bottom-right corner when it
    /// comes out "free" with no place known yet.
    #[test]
    fn the_floating_orb_stands_at_its_corner_of_the_work_area() {
        // A second monitor right of the first, its taskbar 40 px tall at the bottom.
        let work = [2560, 0, 5120, 1400];
        let at = |anchor: &str, centre| orb_place(anchor, work, 200, 24, 40, centre);
        assert_eq!(at("top-left", None), [2584, 24, 200, 200]);
        assert_eq!(at("top-right", None), [4896, 24, 200, 200]);
        assert_eq!(at("bottom-left", None), [2584, 1176, 200, 200]);
        assert_eq!(at("bottom-right", None), [4896, 1176, 200, 200]);
        assert_eq!(at("center", None), [3740, 600, 200, 200]);
        assert_eq!(at("somewhere odd", None), at("bottom-right", None), "an anchor it does not know: its home corner");
        assert_eq!(at(MINI_STRIP, None), [2560, 0, 2560, 40], "the strip spans the top of the work area");
        // Dragged: it stays round where Sir left it, whatever its corner and monitor say.
        assert_eq!(at("free", Some([3000, 500])), [2900, 400, 200, 200]);
        assert_eq!(orb_place("free", work, 300, 24, 40, Some([3000, 500])), [2850, 350, 300, 300], "bigger round the same centre");
        assert_eq!(at("free", None), at("bottom-right", None));
    }

    /// A window's entrance: a hairline draws out of its middle, the frame unfolds from it with
    /// the brackets leading, the content resolves top to bottom only once the frame is whole, one
    /// glow settles, and it ends exactly at rest. Its exit, quicker: the content goes first, the
    /// frame folds into the line, the line contracts to a point that flashes once.
    /// "More realistic, a bit more rough": the finish is faint (it never hides what is under
    /// it), and the canvas glows in the theme's own colour.
    #[test]
    fn the_display_finish_is_faint_and_the_canvas_takes_the_themes_colour() {
        let grain = grain_image();
        assert_eq!(grain.size, [GRAIN_TILE, GRAIN_TILE]);
        let strongest = grain.pixels.iter().map(|p| p.a()).max().unwrap();
        assert!(strongest > 0 && strongest <= 2 * GRAIN_ALPHA as u8, "grain is a whisper: {strongest}");
        // Its light counts, not only its alpha: a speck adds no more light than its alpha says.
        // (Seen in the real window on 2026-11-19: the black canvas stood at a mean of 25 of 255,
        // because `Color32::from_white_alpha(15)` is [69, 69, 69, 15].)
        let brightest = grain.pixels.iter().map(|p| p.r().max(p.g()).max(p.b())).max().unwrap();
        assert!(brightest <= GRAIN_ALPHA as u8, "a speck is a shade lighter, never a fog: {brightest}");
        let (light, dark) = (grain.pixels.iter().filter(|p| p.r() > 0).count(), grain.pixels.iter().filter(|p| p.r() == 0 && p.a() > 0).count());
        assert!(light > GRAIN_TILE * GRAIN_TILE / 4 && dark > GRAIN_TILE * GRAIN_TILE / 4, "lighter and darker specks: {light} / {dark}");
        // A frame with the finish paints, and asks for no frame of its own.
        // (A new context paints twice before it settles, and the grain's texture is new once.)
        let ctx = egui::Context::default();
        let frame = || ctx.run(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 360.0))), ..Default::default() }, |ctx| paint_finish(ctx));
        let _ = (frame(), frame(), frame());
        let out = frame();
        assert!(!out.shapes.is_empty());
        let wait = out.viewport_output.values().map(|v| v.repaint_delay).min().unwrap_or(Duration::MAX);
        assert!(wait > Duration::from_millis(500), "the finish never drives the frame rate: {wait:?}");
    }

    #[test]
    fn windows_open_and_close_with_their_motion() {
        // (Sampled every 1 % of each motion: 4 ms of the entrance, 2 ms of the exit.)
        let entrance = |n: usize| WindowMotion::of(Some(n as f32 / 100.0), None);
        let exit = |n: usize| WindowMotion::of(None, Some(n as f32 / 100.0));
        let ms_in = |n: usize| n as f32 * crate::hud_dock::OPEN_ANIM.as_secs_f32() * 10.0;
        let ms_out = |n: usize| n as f32 * crate::hud_dock::CLOSE_FADE.as_secs_f32() * 10.0;
        assert_eq!(WindowMotion::of(None, None), WindowMotion::REST);
        assert!(!entrance(100).moving(), "an entrance ends exactly at rest");
        let start = entrance(0);
        assert!(start.moving() && start.unfold == 0.0 && start.frame == 0.0 && start.resolve == 0.0 && start.brackets_alpha == 0.0 && start.flash > 0.9, "it begins as a spark in its corner");
        for n in 0..=100 {
            let (m, ms) = (entrance(n), ms_in(n));
            assert!(ms < IN_LINE.1 || m.flash == 0.0, "the corner's spark is gone once the line is out ({ms} ms)");
            assert!(m.brackets >= m.unfold - 1e-6, "the corner ticks ride ahead of the frame ({ms} ms)");
            if m.unfold < 0.98 {
                assert_eq!(m.resolve, 0.0, "nothing resolves before the frame is whole ({ms} ms)");
            }
            if m.frame > 0.0 {
                assert!(m.line.is_none_or(|(width, _)| width > 0.95), "the frame comes out of a line drawn whole ({ms} ms)");
            }
            if n > 0 {
                let before = entrance(n - 1);
                assert!(m.unfold >= before.unfold - 1e-6 && m.resolve >= before.resolve - 1e-6 && m.frame >= before.frame - 1e-6 && m.brackets >= before.brackets - 1e-6, "the entrance only grows ({ms} ms)");
                if ms <= IN_LINE.1 {
                    let (w0, w1) = (before.line.expect("the line").0, m.line.expect("the line").0);
                    assert!(w1 > w0, "the line runs out of its corner ({ms} ms)");
                }
            }
        }
        let glow = (0..=100).map(|n| entrance(n).glow).fold(0.0f32, f32::max);
        assert!(glow > 0.9 && entrance(99).glow < 0.05, "one glow settles and is gone at the end: {glow}");
        assert_eq!(exit(0).content, 1.0);
        for n in 1..=100 {
            let (before, m, ms) = (exit(n - 1), exit(n), ms_out(n));
            assert!(m.unfold <= before.unfold + 1e-6 && m.frame <= before.frame + 1e-6 && m.content <= before.content + 1e-6 && m.brackets <= before.brackets + 1e-6, "the exit only goes ({ms} ms)");
            assert!(m.content <= m.unfold + 1e-6, "the content leaves before its frame ({ms} ms)");
            if let (Some((w0, _)), Some((w1, _))) = (before.line, m.line) {
                assert!(w1 <= w0 + 1e-6, "the line only contracts ({ms} ms)");
            }
            if m.line.is_some() {
                assert!(m.unfold < 0.75, "the line shows once the frame has folded into it ({ms} ms)");
            }
        }
        assert!(exit(34).content < 1e-6 && exit(34).unfold > 0.9, "the content is gone while the frame has barely folded");
        let end = exit(100);
        assert!(end.frame < 1e-6 && end.content < 1e-6 && end.brackets_alpha < 1e-6 && end.unfold < 1e-6 && end.line.is_none());
        let flash = (0..=100).max_by(|a, b| exit(*a).flash.total_cmp(&exit(*b).flash)).unwrap_or(0);
        assert!((200.0..=240.0).contains(&ms_out(flash)) && exit(flash).flash > 0.9, "the corner sparks once at the end: {} ms", ms_out(flash));
        // Closed while it was still opening: the exit takes over.
        assert_eq!(WindowMotion::of(Some(0.4), Some(0.2)), exit(20));
    }

    /// A hairline frame drawn inside its rect: whole pixels at any scale, nothing outside the
    /// rect (a block flush with the screen's edge keeps all four sides).
    #[test]
    fn outlines_stay_inside_their_rect_at_any_scale() {
        for ppp in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 200.0));
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("border_test")));
                crate::holo::outline(&painter, &crate::holo::plate(rect, [0.0; 4]), Color32::WHITE);
            });
            let shapes: Vec<Rect> = out.shapes.iter().map(|c| c.shape.visual_bounding_rect()).filter(|r| r.is_positive()).collect();
            assert_eq!(shapes.len(), 1, "one closed hairline at {ppp}");
            let drawn = shapes[0];
            assert!(rect.expand(1e-3).contains_rect(drawn), "inside the rect at {ppp}: {drawn:?}");
            assert!((drawn.min - rect.min).length() < 1e-3 && (drawn.max - rect.max).length() < 1e-3, "all four edges at {ppp}: {drawn:?}");
        }
    }

    /// "The text input field sometimes refuses my input": the line keeps its focus while the
    /// completions row comes and goes above it, through Tab (which takes a completion and must
    /// not move the focus on) and after a command was sent.
    #[test]
    fn the_arrow_keys_walk_through_what_was_sent() {
        let mut history = ConsoleHistory { loaded: true, ..Default::default() };
        assert_eq!(history.back("draft"), None, "nothing was sent yet");
        for line in ["open the notebook", "volume 30", "open the notebook", "what did I miss"] {
            history.push(line);
        }
        assert_eq!(history.sent, ["volume 30", "open the notebook", "what did I miss"], "a line sent again is the newest, once");
        assert_eq!(history.back("half a li").as_deref(), Some("what did I miss"));
        assert_eq!(history.back("what did I miss").as_deref(), Some("open the notebook"));
        assert_eq!(history.back("x").as_deref(), Some("volume 30"));
        assert_eq!(history.back("x"), None, "the oldest is the end");
        assert_eq!(history.forward().as_deref(), Some("open the notebook"));
        assert_eq!(history.forward().as_deref(), Some("what did I miss"));
        assert_eq!(history.forward().as_deref(), Some("half a li"), "past the newest: the line he was writing");
        assert_eq!(history.forward(), None);
        for n in 0..(CONSOLE_HISTORY_KEPT + 5) {
            history.push(&format!("line {n}"));
        }
        assert_eq!(history.sent.len(), CONSOLE_HISTORY_KEPT);
    }

    #[test]
    fn the_console_line_keeps_its_focus_while_completions_come_and_go() {
        fn frame(ctx: &egui::Context, events: Vec<egui::Event>, line: &mut String, completions: &[String], focus: &mut bool) -> Option<String> {
            let input = egui::RawInput { events, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))), ..Default::default() };
            let mut sent = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // (Something above the foot, as the console's transcript is.)
                    ui.label("transcript");
                    if let Some(text) = console_foot(ui, line, completions, focus) {
                        sent = Some(text);
                    }
                });
            });
            sent
        }
        let key = |key: Key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let typed = |s: &str| egui::Event::Text(s.to_string());
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let focused = |ctx: &egui::Context| ctx.memory(|m| m.has_focus(egui::Id::new(CONSOLE_INPUT_ID)));
        let (mut line, mut focus) = (String::new(), true);
        let none: Vec<String> = Vec::new();
        let some = vec!["open my system stats".to_string(), "open the earth".to_string()];
        assert_eq!(frame(&ctx, vec![], &mut line, &none, &mut focus), None);
        assert!(focused(&ctx), "the console opens with its line focused");
        frame(&ctx, vec![typed("o")], &mut line, &none, &mut focus);
        // The completions row appears above the line with the next key: the key still lands.
        frame(&ctx, vec![typed("p")], &mut line, &some, &mut focus);
        frame(&ctx, vec![typed("e")], &mut line, &some, &mut focus);
        assert_eq!(line, "ope");
        assert!(focused(&ctx), "still focused with the completions up");
        // And goes again: still typing.
        frame(&ctx, vec![typed("x")], &mut line, &none, &mut focus);
        assert_eq!(line, "opex");
        frame(&ctx, vec![key(Key::Backspace)], &mut line, &some, &mut focus);
        // Tab takes the first completion and leaves the focus on the line.
        frame(&ctx, vec![key(Key::Tab)], &mut line, &some, &mut focus);
        frame(&ctx, vec![], &mut line, &some, &mut focus);
        assert_eq!(line, "open my system stats");
        assert!(focused(&ctx), "Tab kept the focus on the line");
        // Enter sends it, and the line is ready for the next command.
        let sent = frame(&ctx, vec![key(Key::Enter)], &mut line, &none, &mut focus);
        assert_eq!(sent.as_deref(), Some("open my system stats"));
        line.clear();
        frame(&ctx, vec![], &mut line, &none, &mut focus);
        assert!(focused(&ctx), "focused again after sending");
        frame(&ctx, vec![typed("h"), typed("i")], &mut line, &none, &mut focus);
        assert_eq!(line, "hi");
    }

    /// 2026-11-14, Sir: "I tried closing a window JAI opened and it turned black". A window
    /// he closed by hand whose module went on asking for it (a simulation, which as the
    /// desktop's background could not go "back onto the board") stood as its exit's last frame
    /// for good: a dark pane with a line of light.
    #[test]
    fn a_window_closed_by_hand_and_still_asked_for_is_no_dead_pane() {
        let viewport = ViewportId::from_hash_of("test_dead_pane");
        let now = Instant::now();
        let mut motions = DetachedMotions::default();
        assert!(!motions.stays(viewport, now), "not leaving at all");
        // Closed by word and asked for again: it stays at once.
        motions.leaving.insert(viewport, (now, false));
        assert!(motions.stays(viewport, now));
        // Closed by hand: its exit plays, its bar tells its module, and its module closes it.
        // Told and still asked for a moment later, it comes back. A window that has not been
        // told yet (one that draws seldom) is never kept for that, however long it takes.
        motions.leaving.insert(viewport, (now, true));
        assert!(!motions.stays(viewport, now + Duration::from_secs(5)), "its module has not been told");
        motions.told.insert(viewport, now);
        assert!(!motions.stays(viewport, now + Duration::from_millis(100)), "its module closes it now");
        assert!(motions.stays(viewport, now + Duration::from_millis(500)));
        // As the simulation's window asked for itself, frame after frame.
        let ctx = egui::Context::default();
        let then = now.checked_sub(Duration::from_secs(3)).expect("three seconds ago");
        {
            let mut motions = lock(&DETACHED_MOTIONS);
            motions.leaving.insert(viewport, (then, true));
            motions.told.insert(viewport, then);
        }
        assert!(detached_keep(&ctx, viewport, true));
        assert!(!lock(&DETACHED_MOTIONS).leaving.contains_key(&viewport), "its exit is over: it is drawn as a window again");
        lock(&DETACHED_MOTIONS).forget(viewport);
    }

    /// 2026-11-14, Sir: "I want to be able to drag the windows JAI creates". Only the letters
    /// of a window's title took a drag. Its bare ground moves it now (the title row, a margin,
    /// an empty part), and a key on that ground still takes its click.
    #[test]
    fn a_drag_on_a_windows_bare_ground_moves_it() {
        let _g = crate::hud_dock::test_lock();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0));
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        set_still(&ctx, true);
        // One frame of a window with his bar, a line of text and a key of its own; whether the
        // window was told to follow the mouse. (How often the key was clicked, and where it is.)
        let (clicks, key_at) = (std::cell::Cell::new(0), std::cell::Cell::new(Pos2::ZERO));
        let frame = |events: Vec<egui::Event>| {
            let out = ctx.run(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    detached_bar(ui, ctx, "changes", "Changes");
                    ui.label("What is new.");
                    let key = crate::holo_ui::key(ui, "A KEY OF ITS OWN");
                    key_at.set(key.rect.center());
                    clicks.set(clicks.get() + key.clicked() as u32);
                });
            });
            out.viewport_output.get(&ViewportId::ROOT).is_some_and(|v| v.commands.contains(&ViewportCommand::StartDrag))
        };
        let press = |at: Pos2, pressed: bool| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        // A drag that begins on `at` and goes thirty points to the right: whether the window
        // was told to follow, with the press or with the move.
        let drag_from = |at: Pos2| {
            frame(vec![egui::Event::PointerMoved(at)]);
            let pressed = frame(vec![press(at, true)]);
            let moved = frame(vec![egui::Event::PointerMoved(at + vec2(30.0, 4.0))]);
            frame(vec![press(at + vec2(30.0, 4.0), false)]);
            pressed || moved
        };
        frame(vec![]);
        assert!(drag_from(pos2(300.0, 300.0)), "the empty part under its content");
        assert!(drag_from(pos2(300.0, 14.0)), "the title row beside its title");
        assert!(drag_from(pos2(30.0, 14.0)), "the title itself");
        // No pointer down: nothing moves.
        assert!(!frame(vec![egui::Event::PointerMoved(pos2(100.0, 100.0))]));
        // A press on a key is the key's: the window stays, and the key takes its click (egui
        // hands the drag of such a press to the ground beneath the key).
        let key = key_at.get();
        frame(vec![egui::Event::PointerMoved(key)]);
        let pressed = frame(vec![press(key, true)]);
        let released = frame(vec![press(key, false)]);
        frame(vec![]);
        assert!(!pressed && !released, "a click on a key moves no window");
        assert_eq!(clicks.get(), 1, "and the key is clicked");
        assert!(!drag_from(key), "nor does a press on it that slips a little");
    }

    /// 2026-11-14, seen live: for as long as a simulation ran in a window of its own, the
    /// board drew nothing (Windows gave every turn to paint to the window above it, which
    /// asked again at once): the emblem stood still, the orb did not come out. A board that
    /// wanted to draw and has waited too long is told to; one that has not asked, never.
    #[test]
    fn a_board_kept_from_drawing_is_told_to_draw() {
        assert!(!board_overdue(1_000, 1_000, false) && !board_overdue(1_000 + BOARD_OVERDUE_MS, 1_000, false), "a little late is not kept waiting");
        assert!(board_overdue(1_001 + BOARD_OVERDUE_MS, 1_000, false));
        assert!(board_overdue(1_001, 1_000, true) && !board_overdue(1_000, 1_000, true), "just kept waiting: told the moment it is due");
        assert!(!board_overdue(u64::MAX - 1, u64::MAX, false) && !board_overdue(u64::MAX - 1, u64::MAX, true), "it has not asked for a frame");
        // It says when it wants its next frame, and the sooner of two wishes counts.
        BOARD_DUE_MS.store(u64::MAX, Ordering::Release);
        board_due_in(Duration::from_millis(500));
        board_due_in(Duration::from_millis(16));
        board_due_in(Duration::from_secs(2));
        let (due, now) = (BOARD_DUE_MS.load(Ordering::Acquire), hud_ms());
        assert!(due <= now + 16 && due + 5_000 > now, "{due} at {now}");
        BOARD_DUE_MS.store(u64::MAX, Ordering::Release);
    }

    /// 2026-11-14, seen live with the build Sir had: "close the feature lab" did nothing while
    /// the Feature Lab stood in a window of its own (as the desktop's background it always
    /// does), and "close all windows" left it standing. The dock knew only the windows of
    /// `block`. A view with his bar in a window of its own closes by word as by its key.
    #[test]
    fn a_view_in_a_window_of_its_own_closes_by_word() {
        let _g = crate::hud_dock::test_lock();
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0));
        // (The board's frame asks for the window, as the view's module does; then the window
        // draws a frame of its own.)
        let frame = || {
            let mut closed = false;
            detached_keep(&ctx, ViewportId::ROOT, true);
            let _ = ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| closed = detached_title_bar(ui, ctx, "features", "Feature Lab // every system"));
            });
            closed
        };
        assert!(!frame() && !frame(), "open: nothing closes it");
        assert_eq!(crate::hud_dock::close_named("feature lab"), vec!["Feature Lab // every system".to_string()], "the dock knows it by its name");
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut closed = false;
        while !closed && Instant::now() < deadline {
            closed = frame();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(closed, "its exit has played, and it closes as by its key");
        lock(&DETACHED_MOTIONS).forget(ViewportId::ROOT);
        crate::hud_dock::window_gone("window:Feature Lab // every system");
    }

    /// 2026-11-14, Sir: "Some animations are stuttery". As the desktop's background every
    /// frame at rest was held to twelve a second, also while his emblem was in plain sight:
    /// its rings went round in steps. Seen, he draws as the board does.
    /// "Show me around": one sentence per part, said in order, each part lit as its sentence
    /// starts.
    #[test]
    fn the_tour_lights_each_part_as_its_sentence_starts() {
        let line = start_tour();
        let (_, cues) = lock(&TOUR_CUES).take().unwrap();
        assert_eq!(cues.iter().map(|(k, _)| *k).collect::<Vec<_>>(), (0..TOUR.len()).collect::<Vec<_>>());
        assert!(cues.windows(2).all(|w| w[0].1 < w[1].1));
        assert!(line.starts_with("Here's the tour, Sir.") && TOUR.iter().all(|(_, said)| line.contains(said)));
    }

    #[test]
    fn seen_on_the_desktop_he_draws_as_smoothly_as_the_board() {
        let rest = rest_fps(false);
        assert_eq!(background_fps(rest, false, false, false), rest, "at rest in plain sight: the board's own rate, not twelve");
        assert_eq!(background_fps(144.0, true, false, false), 144.0);
        // For nobody, a few frames are plenty; beside a game, fewer still.
        assert_eq!(background_fps(rest, false, true, false), 12.0);
        assert_eq!(background_fps(144.0, true, true, false), 12.0);
        assert_eq!(background_fps(144.0, true, true, true), 2.0);
        assert_eq!(background_fps(rest, false, false, true), 2.0, "at rest beside a game");
        assert_eq!(background_fps(60.0, true, false, true), 60.0, "spoken to beside a game, on a monitor the game is not on");
        // The music's spectrum asked for 25 frames a second behind a game that covered it.
        assert_eq!(music_frame(true, true, true), None, "a game covers the emblem: no frames for its spectrum");
        assert_eq!(music_frame(true, true, false), None, "a window covers it");
        assert_eq!(music_frame(true, false, true), Some(Duration::from_millis(40)));
        assert_eq!(music_frame(true, false, false), Some(Duration::from_millis(16)));
        assert_eq!(music_frame(false, false, false), None);
    }

    /// "Move yourself to my second monitor" as the desktop's background: his emblem's spot,
    /// as a share of the first monitor's work area, in the middle of the other's.
    #[test]
    fn the_emblem_goes_to_the_middle_of_another_monitor() {
        let (first, second) = ([0, 0, 2560, 1392], [2560, 0, 5120, 1392]);
        assert_eq!(emblem_spot_on(first, first), None, "its own middle is its home");
        assert_eq!(emblem_spot_on(first, second), Some([1.5, 0.5]));
        // A smaller monitor above the first one.
        let above = [320, -1080, 2240, -40];
        let [x, y] = emblem_spot_on(first, above).unwrap();
        assert!((x - 0.5).abs() < 1e-4 && (y - (-560.0 / 1392.0)).abs() < 1e-4, "{x} {y}");
    }

    /// 2026-11-14, Sir's session: "close all windows" answered "there are no secondary
    /// windows open" beside his text console. His console, the history and the camera in
    /// windows of their own are counted, and closed in the HUD's next frame.
    #[test]
    fn close_all_windows_means_his_console_too() {
        PIECES_UP.store(0, Ordering::Release);
        assert_eq!(close_piece_windows(), 0);
        assert!(!PIECES_CLOSE.load(Ordering::Acquire), "none up: nothing asked");
        PIECES_UP.store(2, Ordering::Release);
        assert_eq!(close_piece_windows(), 2);
        assert!(PIECES_CLOSE.swap(false, Ordering::AcqRel), "the HUD is asked to close them");
        PIECES_UP.store(0, Ordering::Release);
    }

    /// A body that asks for more room than the block has (the 3D studio's footer did, by 10 px)
    /// is cut off at the block's edge: the block keeps its size frame after frame.
    #[test]
    fn a_block_never_grows_to_its_content() {
        let _g = crate::hud_dock::test_lock();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        set_still(&ctx, true);
        let mut sizes = Vec::new();
        for _ in 0..40 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                crate::hud_dock::begin_frame();
                hud_block_window(ctx, "overflow_probe", "Overflow probe", [600.0, 400.0], |ui| {
                    let over = ui.available_size() + vec2(0.0, 10.0);
                    ui.allocate_exact_size(over, Sense::hover());
                });
            });
            sizes.push(crate::hud_dock::block_rect("overflow_probe").expect("drawn").size());
        }
        assert!(sizes.windows(2).all(|w| (w[0] - w[1]).length() < 0.5), "the block changed size: {:?} … {:?}", sizes[1], sizes[39]);
        crate::hud_dock::forget("overflow_probe");
    }

    /// A turning view in a window of its own never takes every frame: when the HUD has not
    /// drawn since its last frame it waits, and once the HUD has drawn it goes on at once.
    #[test]
    fn an_animating_window_takes_turns_with_the_hud() {
        let ctx = egui::Context::default();
        let delays = Arc::new(Mutex::new(Vec::new()));
        let asked = delays.clone();
        ctx.set_request_repaint_callback(move |info| lock(&asked).push(info.delay));
        let window = ViewportId::from_hash_of("turns_probe");
        let frame = || {
            lock(&delays).clear();
            let viewports = std::iter::once((window, egui::ViewportInfo::default())).collect();
            let _ = ctx.run(egui::RawInput { viewport_id: window, viewports, ..Default::default() }, next_animation_frame);
            lock(&delays).iter().min().copied().unwrap_or(Duration::MAX)
        };
        assert_eq!(frame(), Duration::ZERO);
        // (egui repaints once more after an immediate request, whatever was asked.)
        frame();
        assert!(frame() > Duration::from_millis(20), "the window took the HUD's turn");
        ROOT_FRAMES.fetch_add(1, Ordering::AcqRel);
        assert_eq!(frame(), Duration::ZERO, "the HUD drew, yet the window waited");
    }

    /// Sir, 2026-11-19: "All windows should be real windows", and of the blocks on the board:
    /// "I really like how the windows have all a connection to the orb. I want this also to
    /// happen with the windows you are making now." A new window takes its place about the
    /// core as a block did: beside him on his circle, the next on the other side, none on him
    /// or on another. Only where he is not on the monitor in use does it open in its middle.
    #[test]
    fn a_new_window_opens_beside_the_core_and_never_on_it() {
        let work = Rect::from_min_size(pos2(2560.0, 0.0), vec2(2560.0, 1392.0));
        let core = Rect::from_center_size(work.center(), Vec2::splat(420.0));
        let size = vec2(560.0, 400.0);
        let middle = work.center() - size / 2.0;
        let mut taken: Vec<Rect> = Vec::new();
        for k in 0..6 {
            let window = Rect::from_min_size(opening_spot(work, size, Some(core), &taken, middle), size);
            assert!(work.expand(0.5).contains_rect(window), "window {k} hangs off the screen: {window:?}");
            assert!(!window.intersects(core.shrink(40.0)), "window {k} stands on the core: {window:?}");
            assert!(taken.iter().all(|t| !t.intersects(window)), "window {k} stands on another: {window:?}");
            taken.push(window);
        }
        assert!(taken[0].center().x > core.center().x && taken[1].center().x < core.center().x, "the first two flank him: {taken:?}");
        // Out on his second circle, so the leader that ties each to him has a length to show.
        let ring = core.width() * 0.5 * 1.55 / 1.15;
        for window in &taken[..2] {
            let (from, to) = crate::holo_stage::leader(core.center(), ring, *window).expect("a leader to a window beside him");
            assert!((to - from).length() > 120.0, "a leader too short to see: {:.0} to {window:?}", (to - from).length());
        }
        // Large windows, as his are (the settings, the Feature Lab, the telemetry, the console,
        // the mixer on one 1440p screen): none on him, even when no free place is left.
        let mut large: Vec<Rect> = Vec::new();
        for (k, size) in [vec2(620.0, 680.0), vec2(580.0, 340.0), vec2(820.0, 720.0), vec2(620.0, 760.0), vec2(440.0, 640.0)].into_iter().enumerate() {
            let window = Rect::from_min_size(opening_spot(work, size, Some(core), &large, work.center() - size / 2.0), size);
            assert!(work.expand(0.5).contains_rect(window) && !window.intersects(core.shrink(40.0)), "large window {k} hangs off the screen or stands on the core: {window:?}");
            // (Stacked over others where no free place is left: their headers stay in view.)
            assert!(large.iter().all(|under| !window.intersects(Rect::from_min_size(under.min, vec2(under.width(), WINDOW_TITLE_ROW)))), "large window {k} covers a header: {window:?} over {large:?}");
            large.push(window);
        }
        // A panel wider than his flank (the disk map) and a screen with no room at all: the
        // middle, as before (there the core makes room, `stage_window_covers_centre`).
        let wide = vec2(1120.0, 700.0);
        assert_eq!(opening_spot(work, wide, Some(core), &[], work.center() - wide / 2.0), work.center() - wide / 2.0);
        assert_eq!(opening_spot(work, work.size(), Some(core), &[], Pos2::ZERO), Pos2::ZERO);
        // He stands on another monitor, or is not up: the middle of the one in use.
        assert_eq!(opening_spot(work, size, Some(core.translate(vec2(-2560.0, 0.0))), &[], middle), middle);
        assert_eq!(opening_spot(work, size, None, &[], middle), middle);
        // (A window stands there: off it, not a step beside it on it.)
        let standing = Rect::from_min_size(middle + vec2(-34.0, -30.0), size);
        let off = Rect::from_min_size(opening_spot(work, size, None, &[standing], middle), size);
        assert!(!off.intersects(standing) && work.contains_rect(off), "{off:?}");
    }

    /// The board ties each of his windows to the core as it tied its blocks: where they stand
    /// on the screen comes into the board's own points. A window on its way out, on another
    /// screen, minimised or gone (closed without its exit: a widget's CLOSE) has no leader.
    #[test]
    fn the_board_draws_its_leaders_to_his_windows() {
        let board = Rect::from_min_size(pos2(2560.0, 0.0), vec2(2560.0, 1440.0));
        let [beside, leaving, elsewhere, gone, small] = ["a", "b", "c", "d", "e"].map(|k| ViewportId::from_hash_of(("leader_probe", k)));
        let stands = |x: f32| Rect::from_min_size(pos2(x, 400.0), vec2(600.0, 420.0));
        {
            let mut motions = lock(&DETACHED_MOTIONS);
            motions.rects.insert(beside, stands(2560.0 + 1700.0));
            motions.rects.insert(leaving, stands(2560.0 + 100.0));
            motions.leaving.insert(leaving, (Instant::now(), true));
            motions.rects.insert(elsewhere, stands(300.0));
            motions.rects.insert(gone, stands(2560.0 + 900.0));
            motions.rects.insert(small, stands(2560.0 + 1300.0));
        }
        let ctx = egui::Context::default();
        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, board.size())), ..Default::default() };
        input.viewports.entry(ViewportId::ROOT).or_default().inner_rect = Some(board);
        for viewport in [beside, leaving, elsewhere, small] {
            input.viewports.entry(viewport).or_default().minimized = Some(viewport == small);
        }
        let mut rects = Vec::new();
        let _ = ctx.run(input, |ctx| rects = own_window_rects(ctx));
        let forgot_the_gone = !lock(&DETACHED_MOTIONS).rects.contains_key(&gone);
        for viewport in [beside, leaving, elsewhere, gone, small] {
            lock(&DETACHED_MOTIONS).forget(viewport);
        }
        assert!(rects.contains(&stands(1700.0)), "his window, in the board's points: {rects:?}");
        assert!(!rects.contains(&stands(100.0)) && !rects.contains(&stands(300.0 - 2560.0)), "none to a leaving window or to another screen: {rects:?}");
        assert!(!rects.contains(&stands(900.0)) && forgot_the_gone, "none to a window that is gone, and where it stood is forgotten: {rects:?}");
        assert!(!rects.contains(&stands(1300.0)), "none to a minimised window: {rects:?}");
        let core = pos2(1280.0, 720.0);
        let (from, to) = crate::holo_stage::leader(core, 300.0, stands(1700.0)).expect("a leader");
        assert!(((from - core).length() - 300.0).abs() < 0.5 && to.x == 1700.0, "from his circle to the edge that faces him: {from:?} {to:?}");
    }

    /// A window of his own wears an index tab as a panel does: numbered in the order his
    /// windows came up. One on its way out is not counted, nor one without his bar.
    #[test]
    fn his_windows_are_numbered_in_the_order_they_came_up() {
        let mut motions = DetachedMotions::default();
        let ids: Vec<ViewportId> = (0..3).map(|k| ViewportId::from_hash_of(("number_probe", k))).collect();
        let start = Instant::now();
        for (k, id) in ids.iter().enumerate() {
            let at = start + Duration::from_millis(10 * (k as u64 + 1));
            motions.shown.insert(*id, (0, at, at));
            motions.barred.insert(*id);
        }
        assert_eq!(ids.iter().map(|id| motions.number(*id)).collect::<Vec<_>>(), vec![1, 2, 3]);
        motions.shown.insert(ViewportId::from_hash_of("number_probe_console"), (0, start, start));
        assert_eq!(motions.number(ids[2]), 3, "a window without his bar takes no number");
        motions.leaving.insert(ids[0], (Instant::now(), true));
        assert_eq!((motions.number(ids[1]), motions.number(ids[2])), (1, 2), "the first is leaving");
    }

    /// A window of his own as it stands on the screen, for the eye: his bar (the index tab in
    /// its corner, the title, the lamp, its key), its content, its frame.
    #[test]
    fn gallery_a_window_of_his_own() {
        let _g = crate::hud_dock::test_lock();
        let _look = crate::holo_theme::look_for_pictures();
        let size = vec2(720.0, 400.0);
        crate::holo_block::set_lamp("window_probe", crate::holo_block::Lamp::Busy);
        let mut snap = crate::ui_snapshot::Snapshot::new(size);
        let canvas = snap.render(6, |ctx| {
            egui::CentralPanel::default().frame(Frame::none().fill(block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                detached_bar(ui, ctx, "window_probe", "Network radar // who is on the network");
                ui.add_space(10.0);
                crate::holo_ui::head(ui, "DEVICES");
                ui.label(RichText::new("Twelve devices answer. The router is the nearest.").size(13.5).color(strip_ink()));
            });
            paint_window_border(ctx);
        });
        crate::holo_block::set_lamp("window_probe", crate::holo_block::Lamp::Rest);
        lock(&DETACHED_MOTIONS).forget(ViewportId::ROOT);
        canvas.save("gallery_own_window");
        let tab = canvas.brightness(Rect::from_min_size(pos2(1.0, 1.0), vec2(10.0, 12.0)));
        assert!(tab > 60.0, "the index tab stands lit in the window's corner: {tab}");
        assert!(canvas.brightness(Rect::from_min_size(pos2(40.0, 6.0), vec2(300.0, 24.0))) > 2.0, "the title is drawn");
    }

    /// Sir, 2026-11-19: "when the windows are closing the animation is great but there is a
    /// black background". A window of his left a pane of the canvas's near-black below its
    /// frame as the frame folded up, and Windows showed it as drawn. Now that ground is pure
    /// black, which Windows sees through (`win32_desktop::see_through_black`), and nothing else
    /// of the window is: not at rest (its index tab's black digits included), not its frame
    /// as it goes.
    #[test]
    fn a_closing_window_leaves_nothing_behind_its_frame() {
        let _g = crate::hud_dock::test_lock();
        let _look = crate::holo_theme::look_for_pictures();
        let size = vec2(720.0, 400.0);
        let draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().frame(Frame::none().fill(block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                detached_bar(ui, ctx, "exit_probe", "Network radar // who is on the network");
                ui.label(RichText::new("Twelve devices answer. The router is the nearest.").size(13.5).color(strip_ink()));
            });
            paint_window_border(ctx);
        };
        let black = |canvas: &crate::ui_snapshot::Canvas, rows: std::ops::Range<usize>| rows.flat_map(|y| (0..720).map(move |x| (x, y))).filter(|&(x, y)| canvas.pixel(x, y) == [0, 0, 0]).count();
        let mut snap = crate::ui_snapshot::Snapshot::new(size);
        let rest = snap.render(4, draw);
        {
            // 100 ms into the exit [ CLOSE ] started: the frame has folded up a tenth, the
            // content has gone.
            let mut motions = lock(&DETACHED_MOTIONS);
            motions.shown.insert(ViewportId::ROOT, (0, Instant::now() - Duration::from_secs(2), Instant::now()));
            motions.leaving.insert(ViewportId::ROOT, (Instant::now() - Duration::from_millis(100), true));
        }
        let exit = snap.render(1, |ctx| {
            set_still(ctx, false);
            draw(ctx);
        });
        lock(&DETACHED_MOTIONS).forget(ViewportId::ROOT);
        exit.save("window_exit");
        assert_eq!(black(&rest, 0..400), 0, "at rest nothing of it is seen through");
        assert_eq!(black(&exit, 385..400), 720 * 15, "below the folding frame all is seen through");
        assert_eq!(black(&exit, 0..160), 0, "the folding frame itself is not");
    }

    /// "Move it to the middle", "put it on the left half", "full screen it", "minimise it",
    /// "restore it": the HUD carries each out on the block in its next frames.
    #[test]
    fn blocks_go_where_sir_sends_them() {
        use crate::window_commands::{Action, Arrangement, Position};
        let _g = crate::hud_dock::test_lock();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        set_still(&ctx, true);
        let id = "arrange_probe";
        let run = |frames: usize| {
            for _ in 0..frames {
                let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                let _ = ctx.run(input, |ctx| {
                    crate::hud_dock::begin_frame();
                    apply_arrangements(ctx);
                    text_block(ctx, id, "Arrange probe", [500.0, 300.0], "probe");
                });
            }
        };
        let send = |action: Action, position: Option<Position>| {
            crate::window_commands::queue(Arrangement { id: id.into(), action, position, size: None, monitor: None, other: None, deadline: Instant::now() + Duration::from_secs(2) });
        };
        let rect = || crate::hud_dock::block_rect(id).expect("drawn");
        run(3);
        let size = rect().size();
        let bounds = block_bounds(screen);
        send(Action::Move, Some(Position::Centre));
        run(4);
        assert!((rect().center() - bounds.center()).length() < 1.5 && (rect().size() - size).length() < 1.5, "the middle, its own size: {:?}", rect());
        send(Action::Move, Some(Position::Left));
        run(4);
        let left = Rect::from_min_max(bounds.min, pos2(bounds.center().x, bounds.max.y));
        assert!((rect().min - left.min).length() < 1.5 && (rect().max - left.max).length() < 1.5, "the left half: {:?}", rect());
        // Full screen: it fills the home screen (on its way there it glides).
        send(Action::Fullscreen, None);
        run(2);
        std::thread::sleep(crate::hud_dock::GLIDE + Duration::from_millis(30));
        run(3);
        assert!(crate::hud_dock::is_fullscreen(id) && (rect().min - screen.min).length() < 1.5 && (rect().max - screen.max).length() < 1.5, "full screen: {:?}", rect());
        send(Action::ExitFullscreen, None);
        run(2);
        std::thread::sleep(crate::hud_dock::GLIDE + Duration::from_millis(30));
        run(3);
        assert!(!crate::hud_dock::is_fullscreen(id) && (rect().min - left.min).length() < 1.5, "back where it was: {:?}", rect());
        // Minimised: off the board into the tray once its exit has played; restored: back.
        send(Action::Minimize, None);
        run(2);
        std::thread::sleep(crate::hud_dock::CLOSE_FADE + Duration::from_millis(30));
        run(2);
        assert_eq!(crate::hud_dock::whereabouts(id), crate::hud_dock::Whereabouts::Tray);
        send(Action::Restore, None);
        run(3);
        assert_eq!(crate::hud_dock::whereabouts(id), crate::hud_dock::Whereabouts::Board);
        assert!(!crate::window_commands::pending(), "nothing left waiting");
        crate::hud_dock::forget(id);
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
    }

    /// The status pill's tooltip says what its word says: a wait (a web search, the brain
    /// loading, game mode) is never "Standby".
    #[test]
    fn every_pill_word_has_its_own_tooltip() {
        let standby = pill_tip("STANDBY");
        assert!(standby.starts_with("Standby"), "{standby}");
        assert_eq!(pill_tip("THINKING"), "Thinking: working on your request.");
        for word in PILL_WORDS.into_iter().filter(|w| *w != "STANDBY") {
            assert_ne!(pill_tip(word), standby, "{word}");
        }
    }

    /// The reference videos' shell pieces: the status pill's words and colours, the voice rim
    /// (only while Sir speaks, never while JAI does, never docked), the key hints, the pill's
    /// place beside the header row and the screen card's place.
    #[test]
    fn the_reference_shell_pieces() {
        // The line under the header runs between the header row and the blocks' area.
        for size in [vec2(2560.0, 1440.0), vec2(1080.0, 1920.0), vec2(1280.0, 720.0)] {
            let s = Rect::from_min_size(Pos2::ZERO, size);
            let band = header_rule_band(s);
            assert!(band.top() > s.top() + 10.0 + HEADER_H + 2.0 && band.bottom() < block_bounds(s).top(), "{band:?}");
        }
        assert_eq!(pill_word("idle", false, false), "LISTENING");
        assert_eq!(pill_word("listening", false, false), "LISTENING");
        assert_eq!(pill_word("thinking", true, false), "THINKING");
        assert_eq!(pill_word("speaking", false, false), "SPEAKING");
        assert_eq!(pill_word("idle", true, false), "MUTED");
        assert_eq!(pill_word("speaking", false, true), "STANDBY");
        // Colour is information: his lamp is lit while he listens, the plate is solid while
        // he speaks, and a microphone that is off reads in the alert colour.
        let t = holo::theme();
        assert_eq!(pill_colours("LISTENING")[1], t.accent());
        assert_eq!((pill_colours("SPEAKING")[2], pill_colours("SPEAKING")[0]), (t.accent(), Color32::BLACK));
        assert_eq!(pill_colours("MUTED")[0], t.alert());
        assert!(pill_works("LOADING BRAIN") && !pill_works("LISTENING"));
        assert_eq!(rim_target("listening", "idle", 0.9, 0.0), 0.0, "silence");
        assert!(rim_target("listening", "speaking", 0.6, 0.0) >= 0.6);
        assert!(rim_target("idle", "pre_speech", 0.0, 0.0) >= 0.25, "a quiet voice still shows");
        assert_eq!(rim_target("speaking", "speaking", 0.9, 0.0), 0.0, "never while JAI speaks");
        assert_eq!(rim_target("listening", "speaking", 0.9, 1.0), 0.0, "never docked");
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let hints = key_hints(&painter, 1800.0, 1060.0);
            // (No F11: he is the desktop's background, never a fullscreen window.)
            assert_eq!(hints.iter().map(|h| h.0).collect::<Vec<_>>(), ["F1 MUTE", "ESC INTERRUPT"]);
            assert!((hints[1].1.right() - 1800.0).abs() < 0.5);
            assert!(hints.windows(2).all(|w| w[0].1.right() + 10.0 < w[1].1.left()), "apart");
            let free = pill_rect(&painter, screen, &[Rect::from_min_size(pos2(1500.0, 10.0), vec2(400.0, HEADER_H))]).expect("room");
            assert!((free.center().x - 960.0).abs() < 0.5 && free.top() < 12.0, "top centre: {free:?}");
            let crowded = pill_rect(&painter, screen, &[Rect::from_min_size(pos2(700.0, 10.0), vec2(1200.0, HEADER_H))]).expect("room");
            assert!(crowded.right() <= 690.0 && crowded.top() < 12.0, "left of a row that reaches the centre: {crowded:?}");
            assert_eq!(pill_rect(&painter, screen, &[Rect::from_min_size(pos2(40.0, 10.0), vec2(1860.0, HEADER_H))]), None, "no room, no pill");
        });
        let home = crate::holo_core::CoreFrame { centre: pos2(960.0, 500.0), radius: 200.0, rest: 200.0, flat: 1.0, dock: 0.0 };
        let card = screen_card_rect(screen, &home);
        assert!(card.top() > 700.0 && (card.center().x - 960.0).abs() < 0.5, "under the emblem: {card:?}");
        let low = crate::holo_core::CoreFrame { centre: pos2(1900.0, 1000.0), ..home };
        assert!(screen.contains_rect(screen_card_rect(screen, &low)), "always whole on the screen");
        let docked = crate::holo_core::CoreFrame { dock: 1.0, ..home };
        assert!(screen_card_rect(screen, &docked).bottom() <= 1080.0 - 10.0 - TELEMETRY_H, "above the strip");
    }

    /// Offscreen renders draw their windows at rest; the window's own context plays them.
    #[test]
    fn offscreen_renders_hold_their_windows_still() {
        let mut snap = crate::ui_snapshot::Snapshot::new(vec2(400.0, 300.0));
        let mut still = false;
        snap.frame(egui::RawInput::default(), |ctx| still = windows_still(ctx));
        assert!(still);
        let live = egui::Context::default();
        assert!(!windows_still(&live));
        set_still(&live, true);
        assert!(windows_still(&live));
    }

    /// One board across Sir's three monitors: the window is the box around them, which has
    /// corners no monitor shows (the upright one starts 290 px higher and ends 190 px lower).
    /// The HUD proper keeps house on the first monitor; each monitor is a region for blocks.
    #[test]
    fn a_board_across_the_monitors_keeps_house_on_the_first_one() {
        use crate::visual_ui_verifier as visual;
        crate::monitors::pretend(Some(visual::desk()));
        let ctx = egui::Context::default();
        let layout = |size: Vec2, origin: Pos2| {
            let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), ..Default::default() };
            input.viewports.entry(ViewportId::ROOT).or_default().inner_rect = Some(Rect::from_min_size(origin, size));
            let _ = ctx.run(input, |_| {});
            (home_screen(&ctx), block_regions(&ctx), monitor_regions(&ctx))
        };
        let screens = visual::desk_screens();
        let (_, size, origin) = screens[3];
        assert_eq!(size, vec2(6200.0, 1920.0));
        let (home, blocks, parts) = layout(size, origin);
        let first = Rect::from_min_size(pos2(1080.0, 290.0), vec2(2560.0, 1440.0));
        assert_eq!(home, first, "the first monitor's part of the window");
        assert_eq!(parts.len(), 3);
        assert_eq!(blocks, vec![block_bounds(first), parts[1].shrink(8.0), parts[2].shrink(8.0)]);
        // The emblem's home is the middle of the first monitor, not of the box.
        let (centre, _) = crate::holo_core::home_pose(home, 0.0);
        assert_eq!(centre, pos2(1080.0 + 1280.0, 290.0 + 720.0));
        // The top right of the header and the bottom strip are on the first monitor, not in the
        // corners above and below the two landscape monitors.
        assert!(parts.iter().any(|p| p.contains(pos2(home.right() - 40.0, home.top() + 20.0))));
        assert!(!parts.iter().any(|p| p.contains(pos2(size.x - 40.0, 20.0))), "the box's own top right is on no monitor");
        // A block belongs to the monitor it stands on; the nearest one from a dead corner.
        assert_eq!(region_at(&blocks, pos2(4000.0, 900.0)), blocks[1]);
        assert_eq!(region_at(&blocks, pos2(500.0, 900.0)), blocks[2]);
        assert_eq!(region_at(&blocks, pos2(3000.0, 100.0)), blocks[0], "above the first monitor: the first monitor");
        // On one monitor (any of the three) the home screen is the window.
        for (name, size, origin) in screens.iter().take(3) {
            let (home, blocks, parts) = layout(*size, *origin);
            assert_eq!(home, Rect::from_min_size(Pos2::ZERO, *size), "{name}");
            assert!(parts.is_empty(), "{name}");
            assert_eq!(blocks, vec![block_bounds(home)], "{name}");
        }
        // A plain window that straddles the bezel between two monitors is no span.
        let (home, blocks, parts) = layout(vec2(1500.0, 940.0), pos2(1800.0, 200.0));
        assert_eq!(home, Rect::from_min_size(Pos2::ZERO, vec2(1500.0, 940.0)));
        assert_eq!((parts.len(), blocks.len()), (2, 1));
        crate::monitors::pretend(None);
    }

    /// The screenshot of 2026-11-06: each row's `[ MONITOR n ]` button was drawn over its
    /// description, and the span button could not switch spanning off again.
    #[test]
    fn the_screens_menu_keeps_its_texts_apart_and_switches_the_span_both_ways() {
        use crate::visual_ui_verifier::{self as visual, Stage};
        let desk = visual::desk();
        for width in [SCREENS_WIDTH, 300.0] {
            for (spanning, here) in [(false, Some(1usize)), (true, None), (false, Some(3))] {
                let mut stage = Stage::new(vec2(900.0, 600.0));
                let picked = std::cell::Cell::new(ScreensPick::default());
                let mut menu = |ctx: &egui::Context| {
                    egui::Area::new(egui::Id::new("screens_under_test")).fixed_pos(pos2(40.0, 40.0)).show(ctx, |ui| {
                        ui.set_width(width);
                        let now = screens_menu(ui, &desk, spanning, here);
                        if now != ScreensPick::default() {
                            picked.set(now);
                        }
                    });
                };
                let frame = stage.frames(3, &mut menu);
                let case = format!("{width} px wide, spanning {spanning}, HUD on {here:?}");
                assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{case}");
                assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{case}");
                // Every monitor has its two lines; all but the HUD's own can be moved to.
                for n in 1..=3 {
                    assert_eq!(frame.find_text(&format!("DISPLAY {n}")).len(), 1, "{case}");
                }
                assert_eq!(frame.find_text("1080 x 1920  AT  -1080, -290").len(), 1, "{case}");
                assert_eq!(frame.find_exact("MOVE HERE").len(), if spanning { 3 } else { 2 }, "{case}");
                assert_eq!(frame.find_exact("HUD IS HERE").len(), usize::from(!spanning), "{case}");
                // The switch says what it will do, and does it.
                let (label, turns) = if spanning { ("BACK TO ONE MONITOR", false) } else { ("SPAN ALL MONITORS", true) };
                let key = frame.find_exact(label);
                assert_eq!(key.len(), 1, "{case}: {label}");
                stage.click(key[0].center(), egui::PointerButton::Primary, &mut menu);
                assert_eq!(picked.get(), ScreensPick { span: Some(turns), ..Default::default() }, "{case}");
                // `[ MOVE HERE ]` names its own monitor (the rows are in the monitors' order).
                let mut moves = frame.find_exact("MOVE HERE");
                moves.sort_by(|a, b| a.top().total_cmp(&b.top()));
                stage.click(moves[moves.len() - 1].center(), egui::PointerButton::Primary, &mut menu);
                assert_eq!(picked.get().send, Some(if here == Some(3) { 2 } else { 3 }), "{case}");
            }
        }
    }

    /// The old menu, as it was drawn: the check the verifier now runs on every frame finds it.
    #[test]
    fn text_drawn_over_text_is_found() {
        use crate::visual_ui_verifier::{self as visual, Stage};
        let mut stage = Stage::new(vec2(900.0, 600.0));
        let frame = stage.frames(3, &mut |ctx| {
            egui::Area::new(egui::Id::new("old_screens_menu")).fixed_pos(pos2(40.0, 40.0)).show(ctx, |ui| {
                ui.set_width(300.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("DISPLAY 3  MONITOR 3 (1080×1920 AT -1080,-290)").font(strip_font()).color(strip_ink()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(egui::Button::new(RichText::new("MONITOR 3").font(strip_font())).frame(false)));
                });
            });
        });
        let found = visual::text_over_text(&frame);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("MONITOR 3"), "{found:?}");
        // A window over the board's text hides it: that is no finding.
        let frame = stage.frames(3, &mut |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| ui.label("The board's own text, under the window."));
            egui::Window::new("over").title_bar(false).fixed_pos(pos2(0.0, 0.0)).show(ctx, |ui| ui.label("A window's text over the board."));
        });
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
    }

    #[test]
    fn the_hud_draws_as_fast_as_the_display_only_while_sir_is_at_it() {
        // Animations off: a slow tick, whatever happens.
        assert_eq!(target_fps(false, true, true, 180.0), 8.0);
        // At rest: 30, even on a fast display with the pointer on the HUD.
        assert_eq!(target_fps(true, false, true, 180.0), 60.0);
        // Something moves while Sir is elsewhere (in a game): 60, the GPU is his.
        assert_eq!(target_fps(true, true, false, 180.0), 60.0);
        // He is at the HUD: the display's own rate, between 60 and `MAX_FPS`.
        assert_eq!(target_fps(true, true, true, 144.0), 144.0);
        assert_eq!(target_fps(true, true, true, 180.0), 180.0);
        assert_eq!(target_fps(true, true, true, 240.0), MAX_FPS);
        assert_eq!(target_fps(true, true, true, 50.0), 60.0);
        // 180 FPS leaves 5.5 ms a frame: the live self-test's frame budget.
        assert!((1000.0 / MAX_FPS - 5.56).abs() < 0.01);
    }

    /// Repro 2026-11-03 ([ THEME ] "does nothing"): egui closes a `CloseOnClick` popup on any click
    /// outside its anchor, so a popup anchored to a separate 1 px rect under the button closed in
    /// the very frame the button's click opened it. Anchored to the button itself it stays open.
    #[test]
    fn a_header_popup_survives_the_click_that_opens_it() {
        let stays_open = |anchor_on_button: bool| {
            let ctx = egui::Context::default();
            let id = egui::Id::new("theme_popup");
            let at = pos2(60.0, 20.0);
            let frame = |events: Vec<egui::Event>| {
                let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), events, ..Default::default() };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let button = ui.put(Rect::from_center_size(at, vec2(80.0, 16.0)), egui::Button::new("THEME"));
                        if button.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(id));
                        }
                        let anchor = if anchor_on_button {
                            button
                        } else {
                            ui.allocate_rect(Rect::from_min_size(button.rect.left_bottom(), vec2(button.rect.width(), 1.0)), Sense::hover())
                        };
                        egui::popup::popup_below_widget(ui, id, &anchor, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| ui.label("swatches"));
                    });
                });
            };
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            frame(vec![egui::Event::PointerMoved(at)]);
            frame(vec![press(true)]);
            frame(vec![press(false)]);
            frame(Vec::new());
            ctx.memory(|m| m.is_popup_open(id))
        };
        assert!(!stays_open(false), "the old 1 px anchor closes the popup at once");
        assert!(stays_open(true), "anchored to the button, the swatches stay open");
    }

    #[test]
    fn every_module_button_runs_valid_tool_calls() {
        let defs = crate::tools::definitions_with(Vec::new());
        assert_eq!(MODULES.len(), 8);
        for (label, calls) in MODULES {
            assert!(!calls.is_empty(), "{label}");
            for (name, args) in *calls {
                features_view::check_call(&defs, name, args).unwrap_or_else(|e| panic!("{label}: {e}"));
            }
        }
    }

    #[test]
    fn a_tool_block_renders_inside_the_hud_and_a_crashing_frame_is_contained() {
        let _dock = crate::hud_dock::test_lock();
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let screen = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))), ..Default::default() };
        let draw = |ctx: &egui::Context| {
            crate::hud_dock::begin_frame();
            hud_block_window(ctx, "test_block", "Function plotter", [900.0, 560.0], |ui| {
                ui.label("f(x) = x^2");
            })
        };
        for _ in 0..3 {
            let _ = ctx.run(screen.clone(), |ctx| {
                assert_eq!(draw(ctx), BlockAction::default(), "no click, no action");
            });
        }
        let rects = crate::hud_dock::block_rects();
        assert!(!rects.is_empty(), "the block publishes its rect for the emblem");
        assert!(rects.iter().all(|r| r.width() >= 300.0 && r.height() >= 200.0), "{rects:?}");
        // A panic inside a frame costs that frame only: the context keeps drawing afterwards.
        assert!(crate::crash_guard::guard("test frame", || {
            let _ = ctx.run(screen.clone(), |_| panic!("viewport transition went wrong"));
        })
        .is_none());
        let _ = ctx.run(screen.clone(), |ctx| {
            draw(ctx);
        });
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
    }

    #[test]
    fn the_bottom_strip_reads_like_the_reference() {
        assert_eq!(strip_badge(false, false).0, "LIVE");
        assert_eq!(strip_badge(true, false).0, "MUTE");
        assert_eq!(strip_badge(false, true).0, "SLEEP");
        assert_eq!(heard_line("Yes,   please.\nPresent the report.", 60), "Yes, please. Present the report.");
        let long = heard_line("Hey, can you research something for me, a state of VR report please", 24);
        assert!(long.ends_with('…') && long.chars().count() == 24, "{long}");
        assert_eq!(heard_line("anything", 3), "", "no room, no line");
        // The chip is a solid block while he hears or is muted, an outline while he sleeps.
        assert!(strip_badge(false, false).2 && strip_badge(true, false).2 && !strip_badge(false, true).2);
        assert_eq!(strip_badge(true, false).1, holo::theme().alert(), "a muted microphone reads in the alert colour");
        // The COMMS block is off unless switched on.
        assert!(!crate::interface::InterfaceSettings::default().comms_block);
    }

    #[test]
    fn the_header_strip_is_the_reference_strip_flush_top_right() {
        let items = header_items(&HeaderState { camera: true, ..HeaderState::default() }, "REBOOT LLM", "DEFAULT CAMERA");
        let names: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            names,
["THEME", "PHONE QR", "CORE", "MODULES", "FEATURES", "CHANGES", "HISTORY", "VIEW", "CLEAR VIEW", "CONSOLE", "REBOOT LLM", "CAMERA", "CAMERA SOURCE", "DEFAULT CAMERA", "SCREENS", "VOICE: YOURS", "MIC: ON"]
        );
        assert!(items.iter().filter(|i| !i.boxed).map(|i| i.id).eq(["source_label"]), "only the source caption is unboxed");
        let on = |items: &[HeaderItem], id: &str| items.iter().find(|i| i.id == id).unwrap().on;
        assert!(on(&items, "camera") && !on(&items, "console"), "the camera box lights while the camera runs");
        let lit = header_items(&HeaderState { console: true, phone: true, core: true, features: true, ..HeaderState::default() }, "X", "X");
        assert!(["console", "phone", "core", "features"].iter().all(|id| on(&lit, id)), "a box lights while its panel is open");
        let muted = header_items(&HeaderState { mic_muted: true, ..HeaderState::default() }, "X", "X");
        assert!(muted.iter().any(|i| i.id == "mic" && i.text == "MIC: MUTED" && i.on));
        // Whose voice he answers to: lit while others are answered too.
        for (lock_on, guests, known, text, lit) in [(true, false, true, "VOICE: YOURS", false), (true, false, false, "VOICE: LEARNING", false), (true, true, true, "VOICE: GUESTS", true), (false, false, true, "VOICE: ANYONE", true), (false, true, false, "VOICE: ANYONE", true)] {
            let items = header_items(&HeaderState { voice: VoiceLock::of(lock_on, guests, known), ..HeaderState::default() }, "X", "X");
            assert!(items.iter().any(|i| i.id == "voice" && i.text == text && i.on == lit), "{lock_on} {guests} {known}");
        }
        assert_eq!((rest_fps(false), rest_fps(true)), (60.0, 10.0));
        let dictating = header_items(&HeaderState { dictating: true, ..HeaderState::default() }, "X", "X");
        assert!(dictating.iter().any(|i| i.id == "mic" && i.text == "MIC: DICTATING" && i.on));
        let measure = |t: &str| t.chars().count() as f32 * 6.0;
        let row = header_layout(&items, 12.0, 2548.0, 10.0, measure);
        let rects = &row.placed;
        // A wide screen holds the whole strip: left to right, no overlaps, the last box flush
        // against the right margin, no menu.
        assert!(rects.len() == items.len() && row.hidden.is_empty() && row.more.is_none());
        assert_eq!(rects.last().unwrap().1.right(), 2548.0);
        assert!(rects.windows(2).all(|w| w[0].1.right() + 6.0 <= w[1].1.left() + 0.01));
        // The groups stand apart, a hairline halfway between each two.
        assert_eq!(row.seams.len(), HEADER_GROUPS.len());
        for (seam, id) in row.seams.iter().zip(HEADER_GROUPS) {
            let after = rects.iter().find(|(i, _)| i == id).unwrap().1;
            assert!((after.left() - seam - HEADER_GROUP_GAP / 2.0).abs() < 0.01, "{id}: {seam} before {after:?}");
        }
        let source = rects.iter().find(|(id, _)| *id == "source").unwrap().1;
        assert!(source.width() >= SOURCE_PICKER_W, "the picker is a wide field");
        assert!(rects.iter().all(|(_, r)| r.height() == HEADER_H && r.top() == 10.0));
    }

    #[test]
    fn on_an_upright_screen_the_header_keeps_inside_and_the_rest_is_in_the_more_menu() {
        let items = header_items(&HeaderState::default(), "REBOOT LLM", "DEFAULT CAMERA");
        let measure = |t: &str| t.chars().count() as f32 * 6.6;
        for width in [1080.0f32, 800.0, 1440.0, 1920.0, 400.0] {
            let (left, right) = (12.0, width - 12.0);
            let row = header_layout(&items, left, right, 10.0, measure);
            let boxes: Vec<Rect> = row.more.into_iter().chain(row.placed.iter().map(|(_, r)| *r)).collect();
            assert!(boxes.iter().all(|r| r.left() >= left - 0.01 && r.right() <= right + 0.01), "{width}: a box leaves the screen: {boxes:?}");
            assert!(boxes.windows(2).all(|w| w[0].right() + 6.0 <= w[1].left() + 0.01), "{width}: boxes overlap");
            assert!(row.seams.iter().all(|x| boxes.iter().all(|r| !(r.left()..=r.right()).contains(x))), "{width}: a seam cuts a box");
            // Every box is either on the row or in the menu, and the menu is there exactly
            // when something is in it.
            let boxed = items.iter().filter(|i| i.boxed).count();
            let on_row = row.placed.iter().filter(|(id, _)| items.iter().any(|i| i.id == *id && i.boxed)).count();
            assert_eq!(on_row + row.hidden.len(), boxed, "{width}");
            assert_eq!(row.more.is_some(), row.placed.len() < items.len(), "{width}");
            // What stays is the right end of the strip, the microphone last.
            if let Some((last, _)) = row.placed.last() {
                assert_eq!(*last, "mic", "{width}");
            }
        }
        // 1080 points across: the first boxes are in the menu, in the strip's order.
        let row = header_layout(&items, 12.0, 1068.0, 10.0, measure);
        assert!(row.hidden.starts_with(&["theme", "phone"]), "{:?}", row.hidden);
    }

    #[test]
    fn the_comms_block_stays_left_of_the_console_and_clear_of_the_docked_emblem() {
        for (w, h) in [(2560.0, 1440.0), (1920.0, 1080.0), (1080.0, 1920.0)] {
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h));
            for console in [0.0, CONSOLE_WIDTH] {
                let (width, x) = comms_placement(screen, console);
                let (left, right) = (x - width / 2.0, x + width / 2.0);
                assert!(left >= 0.0 && right <= w - console, "{w}×{h} console {console}: {left}..{right}");
                // The docked emblem (centre `DOCK_INSET` left of the free edge, 52.5 px radius).
                let emblem = crate::hud_dock::docked_centre(screen, console).x - crate::holo_core::DOCKED_DIAMETER / 2.0;
                assert!(right < emblem, "{w}×{h} console {console}: block ends at {right}, emblem starts at {emblem}");
                // The key hints of the bottom strip end left of it too, console out or not.
                assert!(key_hints_right(screen, console) + 16.0 <= emblem, "{w}×{h} console {console}: hints end at {}", key_hints_right(screen, console));
            }
        }
        // Wide screens keep the familiar centred 820 px block.
        assert_eq!(comms_placement(Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1440.0)), 0.0), (820.0, 1280.0));
    }

    #[test]
    fn the_emblem_follows_jai_voice_and_never_sirs_microphone() {
        // Repro 2026-11-03: `mic.max(voice)` made the orb pulse while Sir talked.
        let sir = Reactor { mic: 0.8, voice: 0.0, ..Default::default() };
        let jai = Reactor { mic: 0.1, voice: 0.6, ..Default::default() };
        assert_eq!((sir.emblem_voice(), jai.emblem_voice()), (0.0, 0.6));
    }
}
