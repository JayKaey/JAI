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
//! windows are egui deferred viewports (`widget_view`). The board launches in true borderless
//! fullscreen on the primary monitor (F11 toggles maximised windowed mode).

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
use crate::rose_emblem_renderer::{EmblemInputs, RoseEmblem};
use crate::config::ConfigStore;
use crate::ears::AudioLevel;
use crate::hud::{ConfigState, HudCommand, HudEvent};
use crate::interface::{self, HudLayout, InterfaceSettings, WindowCommand, THEMES};
use crate::memory::MemoryEntry;
use crate::modality::VoiceResponseMode;
use crate::skills::JaiSkill;
use crate::telemetry::SystemTelemetry;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Frame, Key, Layout, Margin,
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
    }
}

/// A repaint within one 60 Hz frame: a burst of backend events (streamed tokens, tool logs)
/// becomes one frame instead of one repaint per event.
fn request_repaint_soon() {
    if let Some(ctx) = context() {
        ctx.request_repaint_after_for(Duration::from_millis(16), ViewportId::ROOT);
    }
}

/// Whether JAI can open native windows (false when headless).
pub fn windows_supported() -> bool {
    SHARED.running.load(Ordering::Acquire)
}

/// The monitor the board was sent to and has not gone to yet (`send_board_to_monitor`).
static HUD_MONITOR: Mutex<Option<usize>> = Mutex::new(None);

/// Sends the HUD's board to monitor `index` (1-based) as the fullscreen board there; a board
/// that spans every monitor comes back to that one. False when the HUD is a plain window or
/// the floating orb, which move as any window does.
pub fn send_board_to_monitor(config: &ConfigStore, index: usize) -> bool {
    let iface = config.read(|c| c.interface.clone());
    if iface.orb_mode || !(iface.fullscreen || iface.span_all_monitors) {
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

/// The monitor the fullscreen board was sent to, once it can go there (a board that still
/// spans the monitors, a window and the floating orb leave the request where it is).
fn take_monitor_request(current: &InterfaceSettings) -> Option<usize> {
    if current.fullscreen && !current.span_all_monitors && !current.orb_mode { lock(&HUD_MONITOR).take() } else { None }
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
#[cfg_attr(not(windows), allow(dead_code))]
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

/// "Open the interface settings": the HUD's settings window opens on its next frame.
static SETTINGS_REQUEST: AtomicBool = AtomicBool::new(false);

/// Opens the HUD's interface settings window and brings the HUD up.
pub fn open_interface_settings() {
    SETTINGS_REQUEST.store(true, Ordering::Release);
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
    #[cfg(windows)]
    {
        let board = SHARED.hwnd.load(Ordering::Acquire);
        let own: Vec<_> = crate::win32_desktop::own_windows().into_iter().filter(|w| w.hwnd != board && w.title != "J.A.I. // orb").collect();
        let n = own.len();
        info!("[HUD] His windows on call: {n} brought to the front");
        // (And their list, to pick one from, move it or close it.)
        WINDOWS_LIST.store(true, Ordering::Release);
        request_repaint();
        crate::win32::run_detached(move || {
            // (From the lowest up: they keep the order they had among themselves.)
            for w in own.iter().rev() {
                crate::win32_desktop::raise(w.hwnd);
            }
        });
        n
    }
    #[cfg(not(windows))]
    0
}

/// The list of his windows is open ("show your windows", his key for it).
static WINDOWS_LIST: AtomicBool = AtomicBool::new(false);

/// `JAI_DEMO=desk-click`: where the header's `[ CHANGES ]` key stands (window points), for a
/// scripted session's stand-in for Sir's click on the desktop.
static DEMO_KEY: Mutex<Option<Rect>> = Mutex::new(None);

/// One of his global keys was pressed (`win32_desktop::start`): 0 his windows to the front,
/// 1 the console to type to him.
pub fn on_global_key(key: usize) {
    match key {
        0 => {
            windows_on_call();
        }
        _ => set_text_console(true),
    }
}

/// Text the console's input line takes on its next frame (the Feature Lab's `[ TRY ]`).
static CONSOLE_PREFILL: Mutex<Option<String>> = Mutex::new(None);

/// Opens the text console with `text` in its input line, ready for Sir to finish.
fn prefill_console(text: String) {
    *lock(&CONSOLE_PREFILL) = Some(text);
    set_text_console(true);
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

/// "Open the mixer": a fader for every channel (and closes it again).
pub fn show_mixer(open: bool) {
    if open {
        open_control("mixer");
    } else {
        mixer_view::close();
        request_repaint();
    }
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
static CORE_REQUESTS: Mutex<Vec<crate::core_sphere_3d::CoreCommand>> = Mutex::new(Vec::new());

/// Opens, closes, zooms or spins the core sphere and brings the HUD up.
pub fn core_command(cmd: crate::core_sphere_3d::CoreCommand) {
    lock(&CORE_REQUESTS).push(cmd);
    if !matches!(cmd, crate::core_sphere_3d::CoreCommand::Open(false)) {
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
    #[cfg(windows)]
    {
        let hwnd = SHARED.hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            crate::win32::hide_hud(hwnd);
        }
    }
    #[cfg(not(windows))]
    if let Some(ctx) = context() {
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
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
            let ctx = context().ok_or("The HUD window is not open.")?;
            match other {
                WindowCommand::Minimize => ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(true)),
                WindowCommand::Maximize => ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Maximized(true)),
                WindowCommand::Restore => ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Maximized(false)),
                _ => return Err("Moving the HUD window is only supported on Windows.".into()),
            }
            ctx.request_repaint();
        }
    }
    Ok(())
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
    #[cfg(windows)]
    {
        let hwnd = SHARED.hwnd.load(Ordering::Acquire);
        // The floating orb's square is not where the full HUD should reopen.
        if hwnd == 0 || config.read(|c| c.interface.fullscreen || c.interface.orb_mode) {
            return;
        }
        if let Some((rect, maximized)) = crate::win32::window_placement(hwnd) {
            config.update(|c| {
                c.hud_window = Some(rect);
                c.hud_maximized = maximized;
            });
        }
    }
    #[cfg(not(windows))]
    let _ = config;
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
    #[cfg(windows)]
    let scale = crate::win32::primary_screen().2;
    #[cfg(not(windows))]
    let scale = 1.0;
    Some(Rect::from_min_max(pos2(m.rect[0] as f32 / scale, m.rect[1] as f32 / scale), pos2(m.rect[2] as f32 / scale, m.rect[3] as f32 / scale)))
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
    if !crate::platform::in_test_session() {
        // A window with no place of its own opens on the monitor Sir is using, not always on
        // his first one. (His choice of 2026-11-13: "Windows open where you work".)
        return match builder.position.is_none().then(|| place_where_sir_works(&builder)).flatten() {
            Some(at) => builder.with_position(at),
            None => builder,
        };
    }
    match (builder.position, start_monitor_points()) {
        (None, Some(m)) => {
            let size = builder.inner_size.unwrap_or(vec2(800.0, 600.0)).min(m.size());
            builder.with_position(m.center() - size / 2.0)
        }
        _ => builder,
    }
}

/// Where a window of JAI's own that has no place yet opens (points): in the middle of the
/// monitor Sir is using, a step aside for each window of his that opened just before; beside a
/// game, on a monitor the game is not on ("A game is left alone"). The same place for as long
/// as the window is asked for (by its title), so a window Sir has moved is never pulled back.
fn place_where_sir_works(builder: &egui::ViewportBuilder) -> Option<Pos2> {
    static PLACES: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (Pos2, Instant)>>> = std::sync::LazyLock::new(Default::default);
    let title = builder.title.clone()?;
    let mut places = lock(&PLACES);
    let now = Instant::now();
    // (Not asked for during a second and no such window left: it was closed, and its next
    // opening is placed anew. A window that is still there keeps its place however long a
    // frame of the board took: placed anew it would jump.)
    places.retain(|title, (_, seen)| {
        let fresh = now.duration_since(*seen) < Duration::from_secs(1);
        #[cfg(windows)]
        let fresh = fresh || crate::win32::own_window(title).is_some();
        let _ = title;
        fresh
    });
    if let Some((at, seen)) = places.get_mut(&title) {
        *seen = now;
        return Some(*at);
    }
    let monitors = crate::monitors::recent();
    let mut on = crate::wallpaper::monitor_in_use(&monitors)?;
    if crate::hud_feel::game_in_front() {
        if let Some(other) = monitors.iter().find(|m| m.rect != on.rect && m.primary).or_else(|| monitors.iter().find(|m| m.rect != on.rect)) {
            on = other.clone();
        }
    }
    #[cfg(windows)]
    let scale = crate::win32::primary_screen().2;
    #[cfg(not(windows))]
    let scale = 1.0;
    let scale = (scale * context().map_or(1.0, |ctx| ctx.zoom_factor())).max(0.1);
    let work = Rect::from_min_max(pos2(on.work[0] as f32, on.work[1] as f32) / scale, pos2(on.work[2] as f32, on.work[3] as f32) / scale);
    let size = builder.inner_size.unwrap_or(vec2(800.0, 600.0)).min(work.size());
    let step = (places.len() % 6) as f32 - 1.0;
    let at = work.center() - size / 2.0 + vec2(34.0, 30.0) * step;
    let at = pos2(at.x.clamp(work.left(), (work.right() - size.x).max(work.left())), at.y.clamp(work.top(), (work.bottom() - size.y).max(work.top())));
    places.insert(title, (at, now));
    Some(at)
}

fn open_window(app: &Jai, rt: &Handle, model: &Arc<Mutex<HudModel>>) -> Result<(), String> {
    let (iface, placement, maximized) = app.config().read(|c| (c.interface.clone(), c.hud_window, c.hud_maximized));
    let on_top = if iface.orb_mode { iface.orb_on_top } else { iface.always_on_top };
    let level = if on_top { egui::viewport::WindowLevel::AlwaysOnTop } else { egui::viewport::WindowLevel::Normal };
    let (size, min) = if iface.orb_mode { (vec2(iface.orb_size_px, iface.orb_size_px), vec2(60.0, 60.0)) } else { (layout_size(iface.layout), min_size(iface.layout)) };
    let start_on = start_monitor_points();
    let asked = START_MONITOR.load(Ordering::Acquire);
    if asked > 0 && start_on.is_none() {
        warn!("[HUD] --monitor {asked}: there is no monitor {asked} ({} connected); the HUD opens where it would anyway", crate::monitors::list().len());
    }
    let geometry = startup_geometry(&iface, size, start_on);
    let fullscreen = geometry.fullscreen;
    // He starts as the desktop's background (when there is a rect for it: a scripted session
    // without a monitor of its own has none and starts as the board).
    let background = iface.desktop_background && !iface.orb_mode && background_rect(&iface).is_some();
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
        // A see-through background ("opacity") and the floating orb need a transparent surface,
        // but on Windows a transparent window does not take the whole monitor in fullscreen (it
        // stayed a ~60 % rectangle in the top-left corner): the fullscreen board is opaque.
        .with_transparent(geometry.transparent)
        .with_decorations(!iface.frameless && !iface.orb_mode && !fullscreen && !iface.span_all_monitors && !background)
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
        ..Default::default()
    };
    if fullscreen {
        let [x, y] = geometry.position.unwrap_or([0.0, 0.0]);
        info!("[HUD] Fullscreen board: {:.0}×{:.0} at ({x:.0}, {y:.0}), opaque", geometry.size.x, geometry.size.y);
    }
    if let Some(on) = start_on {
        info!("[HUD] This session opens on monitor {} (--monitor), at ({:.0}, {:.0})", START_MONITOR.load(Ordering::Acquire), on.left(), on.top());
    }
    let restore = placement.filter(|_| !maximized && !fullscreen && !iface.orb_mode && start_on.is_none() && !background);
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
    Some(ICON.get_or_init(crate::app_icon::generate_rose_gyro_app_icon).clone())
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

/// Fullscreen board: borderless, opaque, the primary monitor's full resolution at (0, 0) (pure orb
/// mode never starts fullscreen). Otherwise the layout's window size, transparent. `on`: the
/// monitor a test session opens on (`--monitor`, logical points): the board covers that one, a
/// window starts in its middle.
fn startup_geometry(iface: &InterfaceSettings, windowed: Vec2, on: Option<Rect>) -> StartupGeometry {
    // As the desktop's background: the plain, opaque, borderless window that the desktop is
    // handed before it is first shown (`HudApp::new`).
    if let Some([l, t, r, b]) = (iface.desktop_background && !iface.orb_mode).then(|| background_rect(iface)).flatten() {
        #[cfg(windows)]
        let scale = crate::win32::primary_screen().2.max(0.1);
        #[cfg(not(windows))]
        let scale = 1.0;
        return StartupGeometry { fullscreen: false, transparent: false, size: vec2((r - l) as f32 / scale, (b - t) as f32 / scale), position: Some([l as f32 / scale, t as f32 / scale]) };
    }
    let spanning = iface.span_all_monitors && !iface.orb_mode;
    let fullscreen = iface.fullscreen && !iface.orb_mode && !spanning;
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
    #[cfg(windows)]
    {
        let (w, h, scale) = crate::win32::primary_screen();
        (w > 0 && h > 0).then(|| (w as f32 / scale, h as f32 / scale))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn native_window(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
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
fn stroke(width: f32, colour: Color32) -> Stroke {
    Stroke::new(width, colour)
}

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
    track: Color32,
    text: Color32,
    muted: Color32,
    green: Color32,
    amber: Color32,
    red: Color32,
    violet: Color32,
    sky: Color32,
    light: bool,
    /// Corner radius of panels (`corner_rounding`); bubbles and buttons follow it.
    radius: f32,
    /// The True-OLED minimal design: pure black, `#09090B` cards, hairline borders, no grid.
    oled: bool,
}

impl Palette {
    fn new(s: &InterfaceSettings) -> Self {
        if s.ui_paradigm == crate::oled_minimal_ui::UiDesignParadigm::ModernOledMinimal {
            return Self::oled(s);
        }
        // Every window takes the active HUD theme (board, console, telemetry, detached windows).
        let theme = crate::theme_engine::current();
        let accent = theme.core_primary;
        // Always deep obsidian glass: colour lives only in the accents.
        let bg = interface::dark_glass(s.background_rgb());
        let light = false;
        let o = s.opacity as f32 / 100.0;
        let white = [255, 255, 255];
        // Stark-OS glass: deep obsidian slate with a breath of the accent.
        let slate = [13, 19, 30];
        let card = if light { interface::mix(bg, white, 0.6) } else { interface::mix(interface::mix(bg, slate, 0.6), accent, 0.035) };
        let well = if light { interface::mix(bg, white, 0.85) } else { interface::mix(interface::mix(bg, slate, 0.35), accent, 0.025) };
        let (text, muted) = if light {
            ([11, 23, 38], [74, 99, 120])
        } else {
            // Cool white for what is read, whatever the theme: colour is for his own things.
            ([232, 242, 252], interface::mix([128, 152, 176], accent, 0.06))
        };
        let semantic = |dark: [u8; 3], bright: [u8; 3]| rgb(if light { dark } else { bright });
        Self {
            accent: rgb(accent),
            bg: rgba(bg, o),
            card: rgba(card, 0.55 + 0.4 * o),
            card_solid: rgb(card),
            well: rgba(well, 0.6 + 0.4 * o),
            well_solid: rgb(well),
            line: rgba(theme.hud_frame_border, if light { 0.45 } else { 0.42 }),
            line_strong: rgba(theme.hud_frame_border, 0.8),
            track: if light { Color32::from_black_alpha(22) } else { Color32::from_white_alpha(18) },
            text: rgb(text),
            muted: rgb(muted),
            // A calm green for "all is well", and a warning that never looks like the theme's
            // own colour (amber, or orange under a yellow theme); red is for what failed.
            green: semantic([0, 150, 100], [120, 232, 176]),
            amber: if light { rgb([190, 110, 0]) } else { crate::hud_style::warn_colour() },
            red: semantic([210, 30, 60], [255, 59, 92]),
            violet: rgb(theme.core_secondary),
            sky: rgb(theme.core_secondary),
            light,
            radius: s.corner_rounding,
            oled: false,
        }
    }

    /// Pixels-off black with zinc cards; colour only in the accent.
    fn oled(s: &InterfaceSettings) -> Self {
        let o = s.opacity as f32 / 100.0;
        let p = crate::oled_minimal_ui::OledPalette::new(crate::theme_engine::current().core_primary);
        Self {
            accent: rgb(p.accent),
            bg: rgba(p.background, o),
            card: rgba(p.surface, 0.7 + 0.3 * o),
            card_solid: rgb(p.surface),
            well: rgba(p.well, 0.75 + 0.25 * o),
            well_solid: rgb(p.well),
            line: rgb(p.border(false)),
            line_strong: rgb(p.border(true)),
            track: Color32::from_white_alpha(14),
            text: rgb(p.text),
            muted: rgb(p.muted),
            green: rgb([52, 211, 153]),
            amber: rgb([251, 191, 36]),
            red: rgb([248, 113, 113]),
            violet: rgb([167, 139, 250]),
            sky: rgb([96, 165, 250]),
            light: false,
            radius: crate::oled_minimal_ui::CARD_ROUNDING,
            oled: true,
        }
    }

    /// The pop-up cards' look in either design.
    fn card_style(&self) -> crate::dynamic_popups::CardStyle {
        crate::dynamic_popups::CardStyle { surface: self.card_solid, border: self.line, accent: self.accent, text: self.text, muted: self.muted, rounding: self.r_panel().max(12.0) }
    }

    /// Panels and cards (16 at the default 18).
    fn r_panel(&self) -> f32 {
        self.radius * 16.0 / 18.0
    }

    /// Chat bubbles and text inputs (22 at the default 18).
    fn r_bubble(&self) -> f32 {
        self.radius * 22.0 / 18.0
    }

    /// Buttons and pills (14 at the default 18).
    fn r_button(&self) -> f32 {
        self.radius * 14.0 / 18.0
    }

}

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    rgb(interface::mix([a.r(), a.g(), a.b()], [b.r(), b.g(), b.b()], t))
}

fn visuals(p: &Palette) -> egui::Visuals {
    let mut v = if p.light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.override_text_color = Some(p.text);
    // Panels are transparent: the background (with Sir's opacity) is painted underneath.
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.card_solid;
    v.extreme_bg_color = p.well_solid;
    v.faint_bg_color = p.well_solid;
    v.code_bg_color = p.well_solid;
    v.window_stroke = stroke(1.0, p.line_strong);
    v.window_rounding = Rounding::same(p.r_panel());
    v.menu_rounding = Rounding::same(p.r_button().min(10.0));
    v.window_shadow = egui::epaint::Shadow { offset: vec2(0.0, 8.0), blur: 24.0, spread: 0.0, color: Color32::from_black_alpha(110) };
    v.popup_shadow = egui::epaint::Shadow { offset: vec2(0.0, 4.0), blur: 12.0, spread: 0.0, color: Color32::from_black_alpha(90) };
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = stroke(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    let fill = |t: f32| blend(p.well_solid, p.accent, t);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card_solid;
    w.noninteractive.weak_bg_fill = p.card_solid;
    w.noninteractive.bg_stroke = stroke(1.0, p.line);
    w.noninteractive.fg_stroke = stroke(1.0, p.text);
    w.inactive.bg_fill = fill(0.14);
    w.inactive.weak_bg_fill = fill(0.10);
    w.inactive.bg_stroke = stroke(1.0, p.line);
    w.inactive.fg_stroke = stroke(1.0, p.text);
    w.hovered.bg_fill = fill(0.24);
    w.hovered.weak_bg_fill = fill(0.20);
    w.hovered.bg_stroke = stroke(1.0, p.accent);
    w.hovered.fg_stroke = stroke(1.5, p.text);
    w.active.bg_fill = fill(0.34);
    w.active.weak_bg_fill = fill(0.30);
    w.active.bg_stroke = stroke(1.0, p.accent);
    w.active.fg_stroke = stroke(2.0, p.text);
    w.open = w.hovered;
    for widget in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        widget.rounding = Rounding::same(p.r_button());
    }
    v
}

/// The HUD's own faces, compiled in (Open Font Licence, `assets/fonts`): Rajdhani for titles and
/// labels, Share Tech Mono for numbers and the small technical print. Rajdhani is drawn small on
/// its em, so it is scaled up to stand level with Segoe UI at the same size.
const HUD_FACES: [(&str, &[u8], f32); 3] = [
    ("rajdhani-semibold", include_bytes!("../assets/fonts/Rajdhani-SemiBold.ttf"), 1.16),
    ("rajdhani-medium", include_bytes!("../assets/fonts/Rajdhani-Medium.ttf"), 1.16),
    ("share-tech-mono", include_bytes!("../assets/fonts/ShareTechMono-Regular.ttf"), 1.04),
];

/// One of the HUD's own faces as a file ("display": Rajdhani SemiBold, "mono": Share Tech
/// Mono), for the phone page.
pub(crate) fn hud_face(name: &str) -> Option<&'static [u8]> {
    match name {
        "display" => Some(HUD_FACES[0].1),
        "mono" => Some(HUD_FACES[2].1),
        _ => None,
    }
}

/// Titles and labels in Rajdhani, numbers in Share Tech Mono (both compiled in), long text in
/// Segoe UI (+ Semibold for emphasis, Segoe UI Symbol for icons) with Cascadia Mono and egui's
/// bundled fonts as fallbacks.
pub(crate) fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, bytes, scale) in HUD_FACES {
        fonts.font_data.insert(name.to_string(), FontData::from_static(bytes).tweak(egui::FontTweak { scale, ..Default::default() }));
    }
    let own = |name: &str| vec![name.to_string()];
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut loaded: Vec<(&str, Vec<u8>)> = Vec::new();
    #[cfg(windows)]
    {
        let dir = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
        let read = |file: &str| std::fs::read(dir.join(file)).ok();
        for (name, files) in [
            ("segoe", &["segoeui.ttf"][..]),
            ("segoe-semibold", &["seguisb.ttf", "segoeuib.ttf"][..]),
            ("mono", &["CascadiaMono.ttf", "consola.ttf"][..]),
            ("symbols", &["seguisym.ttf"][..]),
            ("display", &["bahnschrift.ttf"][..]),
        ] {
            if let Some(bytes) = files.iter().find_map(|f| read(f)) {
                loaded.push((name, bytes));
            }
        }
    }
    let has = |name: &str| loaded.iter().any(|(n, _)| *n == name);
    let pick = |names: &[&str]| -> Vec<String> { names.iter().filter(|n| has(n)).map(|n| n.to_string()).collect() };
    let proportional = [pick(&["segoe", "symbols"]), defaults.clone()].concat();
    let bold = [pick(&["segoe-semibold", "segoe", "symbols"]), defaults].concat();
    // What a compiled-in face has no glyph for (box drawing, arrows, symbols) comes from the next.
    let monospace = [own("share-tech-mono"), pick(&["mono", "symbols"]), fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default()].concat();
    // Always registered (the board and the emblem use it).
    let display = [own("rajdhani-semibold"), pick(&["display", "segoe-semibold", "segoe", "symbols"]), bold.clone()].concat();
    // Buttons, menu entries and small labels.
    let label = [own("rajdhani-medium"), proportional.clone()].concat();
    for (name, bytes) in loaded {
        fonts.font_data.insert(name.to_string(), FontData::from_owned(bytes));
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Monospace, monospace);
    fonts.families.insert(bold_family(), bold);
    fonts.families.insert(FontFamily::Name("display".into()), display);
    fonts.families.insert(label_family(), label);
    ctx.set_fonts(fonts);
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

// ------------------------------------------------------------------ the node-graph core ----
// JAI's subsystems sit on a sphere (`orb_3d`), tied together by strings through the interior.
// The strings vibrate with Sir's voice and JAI's own, and signal particles run along them
// while a request flows EARS → STT → BRAIN → a tool.

/// Cascade colours along the orb's real dependency chains.
const STARK_GOLD: Color32 = Color32::from_rgb(255, 184, 0);
fn arc_points(centre: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = ((to - from).abs() / TAU * 120.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = from + (to - from) * i as f32 / steps as f32;
            centre + vec2(a.cos(), a.sin()) * radius
        })
        .collect()
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

#[derive(Clone, Copy)]
enum Tone {
    Normal,
    On,
    Danger,
}

fn button(ui: &mut Ui, p: &Palette, text: &str, tone: Tone) -> egui::Response {
    let label = RichText::new(text).size(ts(12.0));
    match tone {
        Tone::Normal => ui.add(egui::Button::new(label)),
        Tone::On => ui.add(
            egui::Button::new(label.color(p.green)).fill(p.green.gamma_multiply(0.14)).stroke(stroke(1.0, p.green)),
        ),
        Tone::Danger => ui.add(
            egui::Button::new(label.color(blend(p.red, p.text, 0.3)))
                .fill(p.red.gamma_multiply(0.12))
                .stroke(stroke(1.0, p.red.gamma_multiply(0.7))),
        ),
    }
}

fn pill(ui: &mut Ui, p: &Palette, text: impl Into<String>, colour: Color32) -> egui::Response {
    Frame::none()
        .fill(p.accent.gamma_multiply(0.06))
        .stroke(stroke(1.0, p.line))
        .rounding(Rounding::same(p.r_button()))
        .inner_margin(Margin::symmetric(9.0, 3.0))
        .show(ui, |ui| ui.label(RichText::new(text).monospace().size(ts(11.0)).color(colour)))
        .response
}

fn chip(ui: &mut Ui, text: &str, colour: Color32) {
    Frame::none()
        .fill(colour.gamma_multiply(0.16))
        .rounding(Rounding::same(3.0))
        .inner_margin(Margin::symmetric(7.0, 2.0))
        .show(ui, |ui| ui.label(RichText::new(text.replace('_', " ").to_uppercase()).font(bold(10.0)).color(colour)));
}

/// A Stark-OS glass panel: deep obsidian-slate fill (see-through with the background
/// opacity), a 1 px accent border and a soft drop shadow; `card` adds the specular top edge.
fn glass(_ui: &Ui, p: &Palette) -> Frame {
    Frame::none()
        .fill(p.card)
        .stroke(stroke(1.0, p.line))
        .rounding(Rounding::same(p.r_panel()))
        .shadow(egui::epaint::Shadow { offset: vec2(0.0, 6.0), blur: 18.0, spread: 0.0, color: Color32::from_black_alpha(if p.light { 25 } else { 70 }) })
}

/// A titled panel of the HUD.
fn card<R>(ui: &mut Ui, p: &Palette, title: &str, aux: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui) -> R) -> R {
    let shown = glass(ui, p)
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).font(bold(11.5)).color(p.accent).extra_letter_spacing(1.2));
                ui.with_layout(Layout::right_to_left(Align::Center), aux);
            });
            ui.add_space(2.0);
            body(ui)
        });
    let r = shown.response.rect;
    // The specular edge: a bright accent line on top, a faint white sheen just under it.
    let inset = p.r_panel().max(8.0);
    ui.painter()
        .line_segment([pos2(r.left() + inset, r.top()), pos2(r.right() - inset, r.top())], stroke(1.0, p.accent.gamma_multiply(0.45)));
    ui.painter().line_segment(
        [pos2(r.left() + inset * 0.8, r.top() + 1.5), pos2(r.right() - inset * 0.8, r.top() + 1.5)],
        stroke(1.0, if p.light { Color32::from_white_alpha(120) } else { Color32::from_white_alpha(10) }),
    );
    ui.add_space(10.0);
    shown.inner
}

/// A list entry inside a card; `mark` draws a coloured left edge.
fn item(ui: &mut Ui, p: &Palette, mark: Option<Color32>, body: impl FnOnce(&mut Ui)) {
    let shown = Frame::none()
        .fill(p.well)
        .stroke(stroke(1.0, p.line.gamma_multiply(0.7)))
        .rounding(Rounding::same(p.r_button()))
        .inner_margin(Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    if let Some(colour) = mark {
        let r = shown.response.rect;
        let rr = p.r_button();
        ui.painter().rect_filled(
            Rect::from_min_max(r.left_top(), pos2(r.left() + 3.0, r.bottom())),
            Rounding { nw: rr, sw: rr, ne: 0.0, se: 0.0 },
            colour,
        );
    }
    ui.add_space(4.0);
}

fn muted(ui: &mut Ui, p: &Palette, text: impl Into<String>) {
    ui.label(RichText::new(text).size(ts(11.0)).color(p.muted));
}

/// "Key ……… value" row; returns the value's response (for tooltips).
fn row(ui: &mut Ui, p: &Palette, key: &str, value: impl Into<String>) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(ts(11.5)).color(p.muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(value).size(ts(11.5)).color(p.text)).truncate())
        })
        .inner
    })
    .inner
}

/// Horizontal meter (0..1) with vertical markers at fractions.
fn meter(ui: &mut Ui, p: &Palette, fraction: f32, height: f32, fill: Color32, markers: &[(f32, Color32)]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    let rounding = Rounding::same(height / 2.0);
    painter.rect_filled(rect, rounding, p.track);
    let f = fraction.clamp(0.0, 1.0);
    if f > 0.0 {
        let mut filled = rect;
        filled.set_width((rect.width() * f).max(height));
        painter.rect_filled(filled, rounding, fill);
    }
    for (at, colour) in markers {
        let x = rect.left() + rect.width() * at.clamp(0.0, 1.0);
        painter.line_segment([pos2(x, rect.top() - 1.0), pos2(x, rect.bottom() + 1.0)], stroke(2.0, *colour));
    }
}

fn slider(ui: &mut Ui, p: &Palette, label: &str, shown: String, widget: egui::Slider<'_>) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(ts(11.0)).color(p.muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(shown).size(ts(11.0)).color(p.text)));
    });
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(widget.show_value(false))
}

/// Combo box whose first entry ("Auto …") stands for `None`.
fn device_combo(ui: &mut Ui, id: &str, auto: &str, devices: &[String], current: Option<&str>) -> Option<Option<String>> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .truncate()
        .selected_text(current.unwrap_or(auto))
        .show_ui(ui, |ui| {
            if ui.selectable_label(current.is_none(), auto).clicked() {
                picked = Some(None);
            }
            for d in devices {
                if ui.selectable_label(current == Some(d.as_str()), d).clicked() {
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
        if crate::theme_engine::follow(&current.hud_palette) {
            info!("[THEME] {}", current.hud_palette);
        }
        let fading = crate::theme_engine::fading();
        if fading {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        // Every window's colours follow the theme's fade, not only the board.
        let generation = crate::theme_engine::generation();
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
            let in_desktop = current.desktop_background && !current.orb_mode && self.as_background();
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
            let background = |i: &InterfaceSettings| i.desktop_background && !i.orb_mode && background_rect(i).is_some();
            let shape = |i: &InterfaceSettings| (background(i), i.span_all_monitors, i.fullscreen);
            let (to_background, from_background) = (background(&current), background(&prev));
            if shape(&prev) != shape(&current) && !current.orb_mode {
                let target = lock(&HUD_MONITOR).take();
                if to_background {
                    if !from_background {
                        // Behind every other window at once, before it becomes a plain window
                        // where it is to stand: it never lies over Sir's windows on the way.
                        // (Not hidden for that moment: a hidden window draws no frames, and
                        // the frames are what hand it to the desktop. Seen 2026-11-14: hidden,
                        // it waited eight seconds for the next order, and was hidden again by
                        // the helper that wakes a board hidden in the tray.)
                        #[cfg(windows)]
                        crate::win32_desktop::sink(self.hwnd);
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

    /// Pure orb mode on the window itself: borderless, floating, a square of `orb_size_px` at its
    /// corner of the chosen monitor; leaving it restores the full HUD where it was.
    fn sync_orb_window(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        use egui::viewport::WindowLevel;
        let want = iface.orb_mode.then(|| (iface.orb_size_px, iface.orb_anchor.clone(), iface.orb_monitor, iface.orb_on_top));
        if want == self.orb_geometry {
            return;
        }
        let entering = want.is_some() && self.orb_geometry.is_none();
        let leaving = want.is_none() && self.orb_geometry.is_some();
        self.orb_geometry = want.clone();
        match want {
            Some((size, anchor, monitor, on_top)) => {
                if entering {
                    #[cfg(windows)]
                    if self.hwnd != 0 {
                        self.normal_placement = crate::win32::window_placement(self.hwnd).map(|(rect, _)| rect);
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                    ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(60.0, 60.0)));
                    info!("[HUD] Pure orb mode on ({size:.0} px, {anchor}, monitor {monitor})");
                }
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal }));
                #[cfg(windows)]
                if self.hwnd != 0 {
                    let hwnd = self.hwnd;
                    let ppp = ctx.native_pixels_per_point().unwrap_or(1.0);
                    crate::win32::run_detached(move || place_orb(hwnd, size, &anchor, monitor, ppp));
                }
                #[cfg(not(windows))]
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(size, size) / iface.scale));
            }
            None if leaving => {
                ctx.send_viewport_cmd(ViewportCommand::Decorations(!iface.frameless));
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if iface.always_on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal }));
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(min_size(iface.layout)));
                let restored = self.normal_placement.take().filter(|_| self.hwnd != 0);
                match restored {
                    #[cfg(windows)]
                    Some(rect) => {
                        let hwnd = self.hwnd;
                        crate::win32::run_detached(move || crate::win32::put_back_placement(hwnd, rect));
                    }
                    _ => ctx.send_viewport_cmd(ViewportCommand::InnerSize(layout_size(iface.layout) / iface.scale)),
                }
                info!("[HUD] Pure orb mode off");
            }
            None => {}
        }
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

    /// The close button hides the window to the tray (Windows); JAI keeps running.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || SHARED.quitting.load(Ordering::Acquire) {
            return;
        }
        #[cfg(windows)]
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            remember_placement(self.app.config());
            crate::win32::hide_hud(self.hwnd);
            info!("[HUD] Hidden to the tray");
        }
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
                let text = RichText::new(label).size(ts(11.0)).color(if selected { p.accent } else { p.muted });
                if ui.add(egui::Button::new(text).selected(selected)).on_hover_text(hover).clicked() && !selected {
                    self.command(HudCommand::SetResponseMode { mode });
                }
            }
            if let VoiceResponseMode::CustomCondition(c) = &current {
                pill(ui, &p, format!("CUSTOM: {}", c.replace('_', " ")), p.amber)
                    .on_hover_text("A custom rule JAI applies per answer (set by voice or chat)");
            }
            if cfg.voice_muted {
                pill(ui, &p, "voice muted", p.red).on_hover_text("Say \"unmute your voice\" to hear JAI again");
            }
        });
    }

    // --------------------------------------------------------------------- cards ----

    fn audio_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        let phase = m.level.as_ref().map(|l| l.phase).unwrap_or("—");
        let phase_colour = match phase {
            "speaking" | "pre_speech" | "hangover" => p.green,
            "muted" => p.red,
            "calibrating" | "mic_test" => p.amber,
            _ => p.accent,
        };
        card(ui, &p, "MICROPHONE & VAD", |ui| chip(ui, phase, phase_colour), |ui| {
            if let Some(level) = &m.level {
                let value = if level.phase == "muted" {
                    "mic muted".to_string()
                } else if level.rms_db <= -119.0 {
                    "silence (no signal)".to_string()
                } else {
                    format!("{:.1} dBFS", level.rms_db)
                };
                row(ui, &p, "Input level", value);
                let voice = level.rms >= level.start_gate;
                meter(
                    ui,
                    &p,
                    db_fraction(level.rms_db),
                    12.0,
                    if voice { p.green } else { p.accent },
                    &[(db_fraction(to_db(level.noise_floor)), p.violet), (db_fraction(to_db(level.start_gate)), p.amber)],
                );
                row(ui, &p, "Speech probability (Silero)", format!("{:.2}", level.probability));
                let hot = level.probability >= level.start_threshold;
                meter(
                    ui,
                    &p,
                    level.probability,
                    7.0,
                    if hot { p.amber } else { p.accent },
                    &[(level.start_threshold, p.amber), (level.release_threshold, blend(p.sky, p.text, 0.3))],
                );
                ui.horizontal_wrapped(|ui| {
                    for (colour, text) in [
                        (p.violet, format!("noise floor {:.0} dB", to_db(level.noise_floor))),
                        (p.amber, "start gate".to_string()),
                        (blend(p.sky, p.text, 0.3), "release".to_string()),
                    ] {
                        ui.label(RichText::new("[#]").size(ts(10.0)).color(colour));
                        ui.label(RichText::new(text).size(ts(10.0)).color(p.muted));
                    }
                });
            } else {
                muted(ui, &p, "Waiting for the microphone…");
            }
            muted(ui, &p, if m.vad_note.is_empty() { " " } else { m.vad_note.as_str() });
            if let Some(cfg) = &m.cfg {
                let mut start = cfg.vad_start_threshold;
                let r = slider(ui, &p, "VAD sensitivity (start threshold)", format!("{start:.2}"), egui::Slider::new(&mut start, 0.2..=0.9).step_by(0.01))
                    .on_hover_text("Silero probability needed to start an utterance (lower = more sensitive)");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: Some(start), release_threshold: None, silence_ms: None, noise_multiplier: None });
                }
                let mut release = cfg.vad_release_threshold;
                let r = slider(ui, &p, "Release threshold", format!("{release:.2}"), egui::Slider::new(&mut release, 0.1..=0.8).step_by(0.01))
                    .on_hover_text("Below this probability a frame counts as silence");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: None, release_threshold: Some(release), silence_ms: None, noise_multiplier: None });
                }
                let mut silence = cfg.silence_ms as f32;
                let r = slider(ui, &p, "Silence before end-of-speech", format!("{silence:.0} ms"), egui::Slider::new(&mut silence, 150.0..=1500.0).step_by(10.0))
                    .on_hover_text("How long you must pause before JAI treats your sentence as finished");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: None, release_threshold: None, silence_ms: Some(silence as u32), noise_multiplier: None });
                }
                let mut multiplier = cfg.noise_multiplier;
                let r = slider(ui, &p, "Noise gate multiplier", format!("×{multiplier:.1}"), egui::Slider::new(&mut multiplier, 1.2..=5.0).step_by(0.1))
                    .on_hover_text("Speech must be this many times louder than the room noise to start");
                if r.changed() {
                    self.command(HudCommand::SetVad { start_threshold: None, release_threshold: None, silence_ms: None, noise_multiplier: Some(multiplier) });
                }
            }
            ui.add_space(2.0);
            ui.columns(2, |cols| {
                let w = cols[0].available_width();
                let recalibrate = cols[0].add_sized([w, 26.0], egui::Button::new("Recalibrate"));
                if recalibrate.on_hover_text("Re-measure the room noise (stay quiet for ~1 s)").clicked() {
                    self.command(HudCommand::Recalibrate);
                }
                let test = cols[1].add_sized([w, 26.0], egui::Button::new("Test mic"));
                if test.on_hover_text("Record 2.5 s and run Whisper on it").clicked() {
                    self.command(HudCommand::TestMic);
                }
            });
        });
    }

    fn devices_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        let mut refresh = false;
        card(
            ui,
            &p,
            "AUDIO DEVICES",
            |ui| {
                if ui.small_button("RELOAD").on_hover_text("Rescan devices").clicked() {
                    refresh = true;
                }
            },
            |ui| {
                let Some(cfg) = &m.cfg else {
                    muted(ui, &p, "Loading…");
                    return;
                };
                muted(ui, &p, "Microphone");
                if let Some(device) = device_combo(ui, "hud_mic", "Auto (default + failover)", &m.inputs, cfg.requested_mic.as_deref()) {
                    self.command(HudCommand::SetMicDevice { device });
                }
                row(ui, &p, "Active", &cfg.active_mic);
                muted(ui, &p, "Speaker");
                if let Some(device) = device_combo(ui, "hud_speaker", "System default", &m.outputs, cfg.requested_speaker.as_deref()) {
                    self.command(HudCommand::SetSpeakerDevice { device });
                }
                row(ui, &p, "Active", &cfg.active_speaker);
                let mut loudness = cfg.loudness_pct as f32;
                if slider(ui, &p, "JAI voice loudness", format!("{loudness:.0} %"), egui::Slider::new(&mut loudness, 0.0..=200.0).step_by(5.0)).changed() {
                    self.command(HudCommand::SetLoudness { volume_pct: loudness as u8 });
                }
                ui.horizontal(|ui| {
                    let voices = if cfg.voices.is_empty() { std::slice::from_ref(&cfg.voice) } else { cfg.voices.as_slice() };
                    egui::ComboBox::from_id_salt("hud_voice").width(ui.available_width() - 110.0).selected_text(&cfg.voice).show_ui(ui, |ui| {
                        for voice in voices {
                            if ui.selectable_label(*voice == cfg.voice, voice).clicked() {
                                self.command(HudCommand::SetVoice { voice: voice.clone() });
                            }
                        }
                    });
                    if ui.button("Test voice").clicked() {
                        self.command(HudCommand::TestVoice);
                    }
                });
                row(ui, &p, "Speech engines", format!("{} · {}", if cfg.stt_ready { "whisper OK: " } else { "whisper loading…" }, cfg.tts_status))
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
            &p,
            "SYSTEM",
            |ui| {
                ui.add(egui::Label::new(RichText::new(cpu_name).size(ts(10.5)).color(p.muted)).truncate());
            },
            |ui| {
                let Some(t) = &m.telemetry else {
                    muted(ui, &p, "Collecting telemetry…");
                    return;
                };
                let bar = |ui: &mut Ui, pct: f32, hot: bool| meter(ui, &p, pct / 100.0, 6.0, if hot { p.amber } else { p.accent }, &[]);
                row(ui, &p, "CPU", format!("{:.1}% · {} threads", t.cpu_usage_pct, t.cpu_cores));
                bar(ui, t.cpu_usage_pct, t.cpu_usage_pct > 85.0);
                row(ui, &p, "RAM", format!("{:.1} / {:.1} GB", t.ram_used_gb, t.ram_total_gb));
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
                        row(ui, &p, &g.name.replace("NVIDIA GeForce ", ""), if parts.is_empty() { "—".into() } else { parts.join(" · ") });
                        bar(ui, g.utilization_pct.unwrap_or(0.0), g.temperature_c.unwrap_or(0.0) >= 80.0);
                        if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb.filter(|t| *t > 0)) {
                            row(ui, &p, "VRAM", format!("{:.1} / {:.1} GB", used as f32 / 1024.0, total as f32 / 1024.0));
                            let pct = used as f32 / total as f32 * 100.0;
                            bar(ui, pct, pct > 92.0);
                        }
                    }
                    None => {
                        row(ui, &p, "GPU", "no telemetry");
                    }
                }
                row(ui, &p, "Network", format!("↓ {} · ↑ {}", rate(t.net_down_kbps), rate(t.net_up_kbps)));
                for d in &t.disks {
                    row(ui, &p, &format!("Drive {}", d.mount), format!("{:.0} GB free / {:.0} GB", d.free_gb, d.total_gb));
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
            "confirmed" => p.red,
            "pending" => p.red,
            _ => p.muted,
        };
        Frame::none()
            .fill(p.red.gamma_multiply(if pending { 0.10 } else { 0.04 }))
            .stroke(stroke(if pending { 1.6 } else { 1.0 }, colour.gamma_multiply(glow)))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.9).max(180.0));
                ui.label(RichText::new("SYSTEM POWER · DOUBLE-CHECK").size(ts(9.5)).color(p.red).extra_letter_spacing(0.8));
                let what = if card.action == "restart" { "Restart this PC now?" } else { "Shut down this PC now?" };
                ui.label(RichText::new(what).font(bold(13.5)).color(p.red));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| match card.status.as_str() {
                    "pending" => {
                        if button(ui, p, &format!("{}", card.label), Tone::Danger).clicked() {
                            self.command(HudCommand::PowerCard { id: card.id.clone(), confirm: true });
                        }
                        if ui.button("Cancel").clicked() {
                            self.command(HudCommand::PowerCard { id: card.id.clone(), confirm: false });
                        }
                        muted(ui, p, "or say “yes” / “no” within 30 s");
                    }
                    "confirmed" => {
                        ui.label(RichText::new(if card.action == "restart" { "Restarting…" } else { "Shutting down…" }).color(p.red));
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
            "sent" => p.green,
            "cancelled" => p.muted,
            "pending" => p.green.gamma_multiply(0.8),
            _ => p.red,
        };
        Frame::none()
            .fill(p.well)
            .stroke(stroke(1.0, colour.gamma_multiply(0.6)))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::symmetric(12.0, 9.0))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.9).max(160.0));
                ui.label(RichText::new("WHATSAPP · MESSAGE CARD").size(ts(9.5)).color(p.muted).extra_letter_spacing(0.6));
                ui.label(RichText::new(format!("To {}{}", card.recipient, if card.phone.is_empty() { String::new() } else { format!("  ({})", card.phone) })).font(bold(12.5)).color(p.accent));
                ui.add(egui::Label::new(RichText::new(&card.text).size(ts(13.5))).wrap());
                ui.add_space(4.0);
                ui.horizontal(|ui| match card.status.as_str() {
                    "pending" => {
                        if button(ui, p, "Send via WhatsApp", Tone::On).clicked() {
                            self.command(HudCommand::MessageCard { id: card.id.clone(), send: true });
                        }
                        if ui.button("Cancel").clicked() {
                            self.command(HudCommand::MessageCard { id: card.id.clone(), send: false });
                        }
                        muted(ui, p, "or just say “yes”");
                    }
                    "sent" => {
                        ui.label(RichText::new("OK: Sent").color(p.green));
                    }
                    "cancelled" => {
                        ui.label(RichText::new("Cancelled").color(p.muted));
                    }
                    other => {
                        ui.label(RichText::new(format!("WARN: {other}")).color(p.red));
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
        let p = self.palette;
        let pulse = ((ctx.input(|i| i.time) * 6.0).sin().abs() * 0.7 + 0.3) as f32;
        egui::Area::new(egui::Id::new("hud_room_alert"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(0.0, 0.0))
            .interactable(false)
            .show(ctx, |ui| {
                let rect = home_screen(ctx);
                ui.painter().rect_stroke(rect.shrink(3.0), Rounding::same(6.0), stroke(6.0, p.amber.gamma_multiply(pulse)));
                let banner = Rect::from_center_size(pos2(rect.center().x, rect.top() + 70.0), vec2(rect.width().min(560.0), 44.0));
                ui.painter().rect_filled(banner, Rounding::same(8.0), p.card_solid);
                ui.painter().rect_stroke(banner, Rounding::same(8.0), stroke(1.5, p.amber));
                ui.painter().text(banner.center(), Align2::CENTER_CENTER, format!(" {message}"), bold(13.0), p.amber);
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
        card(ui, &p, "LONG-TERM MEMORY", |ui| muted(ui, &p, count), |ui| {
            ui.horizontal(|ui| {
                let w = ui.available_width();
                ui.add(egui::TextEdit::singleline(&mut self.memory_key).hint_text("key (e.g. coffee)").desired_width(w * 0.5));
                egui::ComboBox::from_id_salt("hud_memory_category")
                    .width(ui.available_width())
                    .selected_text(&self.memory_category)
                    .show_ui(ui, |ui| {
                        for c in MEMORY_CATEGORIES {
                            ui.selectable_value(&mut self.memory_category, c.to_string(), c);
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.memory_value).hint_text("Fact to remember…").desired_width(ui.available_width() - 96.0));
                ui.add(egui::DragValue::new(&mut self.memory_importance).range(1..=10).prefix("FAV ")).on_hover_text("Importance 1-10");
                if button(ui, &p, "+", Tone::On).on_hover_text("Remember").clicked() {
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
            let search = ui.add(egui::TextEdit::singleline(&mut self.memory_search).hint_text("Search memories…").desired_width(f32::INFINITY));
            if search.changed() {
                self.memory_search_at = Some(Instant::now());
            }
            ui.add_space(2.0);
            if m.memories.is_empty() {
                muted(ui, &p, if m.memory_query.is_some() { "No matches." } else { "Nothing remembered yet." });
            }
            for mem in m.memories.iter().take(120) {
                item(ui, &p, None, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&mem.key).font(bold(12.0)).color(p.accent));
                        chip(ui, &mem.category, p.green);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("X").on_hover_text("Forget this").clicked() {
                                self.command(HudCommand::DeleteMemory { id_or_key: mem.id.to_string() });
                            }
                            muted(ui, &p, format!("FAV {}", mem.importance));
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(&mem.value).size(ts(12.0))).wrap());
                });
            }
        });
    }

    fn skills_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        card(ui, &p, "SKILLS", |ui| muted(ui, &p, m.skills.len().to_string()), |ui| {
            if m.skills.is_empty() {
                muted(ui, &p, "No skills yet — say “create a skill that …”");
            }
            for skill in &m.skills {
                item(ui, &p, None, |ui| {
                    ui.horizontal(|ui| {
                        let title = if skill.title.is_empty() { &skill.name } else { &skill.title };
                        ui.label(RichText::new(title).font(bold(12.0)).color(p.accent));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("X").on_hover_text("Delete skill").clicked() {
                                self.confirm_delete_skill = Some(skill.name.clone());
                            }
                            if ui.small_button("Run").clicked() {
                                self.command(HudCommand::RunSkill { name: skill.name.clone() });
                            }
                            muted(ui, &p, format!("{}×", skill.usage_count));
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(&skill.description).size(ts(12.0))).wrap());
                    if !skill.trigger_phrases.is_empty() {
                        muted(ui, &p, format!("“{}”", skill.trigger_phrases.join("” · “")));
                    }
                });
            }
        });
    }

    fn tools_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        card(ui, &p, "TOOL LOG", |ui| muted(ui, &p, m.tool_count.to_string()), |ui| {
            if m.tools.is_empty() {
                muted(ui, &p, "No tools used yet.");
            }
            for t in &m.tools {
                item(ui, &p, Some(if t.success { p.green } else { p.red }), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&t.tool).font(bold(11.5)).color(p.accent));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| muted(ui, &p, &t.meta));
                    });
                    ui.add(egui::Label::new(RichText::new(&t.summary).size(ts(12.0))).wrap());
                    egui::CollapsingHeader::new(RichText::new("details").size(ts(10.5)).color(p.muted))
                        .id_salt(("hud_tool", t.id))
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(&t.details).monospace().size(ts(10.5)).color(p.muted)).wrap());
                        });
                });
            }
        });
    }

    fn brain_card(&mut self, ui: &mut Ui, m: &HudModel) {
        let p = self.palette;
        card(ui, &p, "BRAIN", |_| {}, |ui| {
            let Some(cfg) = &m.cfg else {
                muted(ui, &p, "Loading…");
                return;
            };
            muted(ui, &p, "Primary model");
            let mut models = m.models.clone();
            if !models.contains(&cfg.model) {
                models.push(cfg.model.clone());
            }
            egui::ComboBox::from_id_salt("hud_model").width(ui.available_width()).selected_text(&cfg.model).show_ui(ui, |ui| {
                for model in &models {
                    if ui.selectable_label(*model == cfg.model, model).clicked() && *model != cfg.model {
                        self.command(HudCommand::SetModel { model: model.clone() });
                    }
                }
            });
            row(ui, &p, "Fallback", &cfg.fallback_model);
            row(ui, &p, "Engine", &cfg.llm).on_hover_text(&cfg.llm);
            if ui.button("Clear conversation").on_hover_text("Forget the current conversation (long-term memory is kept)").clicked() {
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

    fn dialogs(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        if self.confirm_shutdown {
            let mut open = true;
            board_window(ctx, "confirm_shutdown", "Shut down // are you sure", [380.0, 190.0], &mut open, |ui| {
                    ui.label("Shut down J.A.I. completely?");
                    muted(ui, &p, "Closing the window only hides him in the tray.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if button(ui, &p, "Shut down", Tone::Danger).clicked() {
                            self.confirm_shutdown = false;
                            self.command(HudCommand::Shutdown);
                        }
                        if ui.button("Cancel").clicked() {
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
                        if button(ui, &p, "Delete", Tone::Danger).clicked() {
                            self.confirm_delete_skill = None;
                            self.command(HudCommand::DeleteSkill { name: name.clone() });
                        }
                        if ui.button("Cancel").clicked() {
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

    /// Manual counterpart of `configure_interface`.
    fn settings_window(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        let p = self.palette;
        let mut open = self.settings_open;
        let mut s = iface.clone();
        board_window(ctx, "interface_settings", "Interface // look and layout", [400.0, 640.0], &mut open, |ui| {
                muted(ui, &p, "Or just ask: “JAI, make the interface gold”, “hide the system panel”, “zoom in”, “stay on top”.");
                ui.add_space(4.0);
                egui::Grid::new("hud_interface").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label("Theme");
                    let current = THEMES.iter().find(|t| t.name == s.theme).map(|t| t.display).unwrap_or("Custom");
                    egui::ComboBox::from_id_salt("hud_theme").selected_text(current).show_ui(ui, |ui| {
                        for t in THEMES {
                            if ui.selectable_label(s.theme == t.name, t.display).clicked() {
                                s.theme = t.name.to_string();
                                s.accent = t.accent.to_string();
                                s.background = t.background.to_string();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Accent");
                    let mut colour = s.accent_rgb();
                    if egui::color_picker::color_edit_button_srgb(ui, &mut colour).changed() {
                        s.accent = interface::hex(colour);
                        s.theme = "custom".into();
                    }
                    ui.end_row();
                    // Always dark glass: the background only gets deeper or shallower.
                    ui.label("Background depth");
                    // Shown on the slider's own 5 % grid: a colour between two steps (59 %) would
                    // otherwise be snapped and written back every frame, and the quantised colour
                    // lands off-grid again (66 %) — the window flickered between the two.
                    let shown = (interface::background_darkness(s.background_rgb()) * 20.0).round() * 5.0;
                    let mut depth = shown;
                    let r = ui.add(egui::Slider::new(&mut depth, 0.0..=100.0).step_by(5.0).suffix(" %"));
                    if r.changed() && (depth - shown).abs() >= 2.5 {
                        s.background = interface::hex(interface::with_darkness(s.background_rgb(), depth / 100.0));
                    }
                    ui.end_row();
                    ui.label("Corner rounding");
                    ui.add(egui::Slider::new(&mut s.corner_rounding, 0.0..=32.0).step_by(1.0));
                    ui.end_row();
                    ui.label("Zoom");
                    let mut zoom = self.zoom_draft.unwrap_or(s.scale * 100.0);
                    let r = ui.add(egui::Slider::new(&mut zoom, 50.0..=250.0).step_by(5.0).suffix(" %"));
                    if r.dragged() {
                        self.zoom_draft = Some(zoom);
                    } else if r.changed() || r.drag_stopped() || self.zoom_draft.is_some() {
                        s.scale = zoom / 100.0;
                        self.zoom_draft = None;
                    }
                    ui.end_row();
                    ui.label("Opacity");
                    let mut opacity = s.opacity as f32;
                    if ui.add(egui::Slider::new(&mut opacity, 15.0..=100.0).step_by(5.0).suffix(" %")).changed() {
                        s.opacity = opacity as u8;
                    }
                    ui.end_row();
                    ui.label("Layout");
                    ui.horizontal(|ui| {
                        for (layout, label) in [(HudLayout::Full, "Full"), (HudLayout::Compact, "Compact"), (HudLayout::Orb, "Orb")] {
                            ui.selectable_value(&mut s.layout, layout, label);
                        }
                    });
                    ui.end_row();
                    ui.label("Window");
                    ui.vertical(|ui| {
                        ui.checkbox(&mut s.always_on_top, "Always on top");
                        ui.checkbox(&mut s.fullscreen, "Full screen");
                        ui.checkbox(&mut s.frameless, "Stark title bar (frameless)");
                        ui.checkbox(&mut s.animations, "Animations");
                        ui.checkbox(&mut s.header_telemetry, "Header telemetry");
                        ui.checkbox(&mut s.orb_mode, "Pure orb mode");
                    });
                    ui.end_row();
                    ui.label("Name");
                    let mut title = self.title_draft.clone().unwrap_or_else(|| s.title.clone());
                    let r = ui.add(egui::TextEdit::singleline(&mut title).desired_width(180.0));
                    if r.lost_focus() {
                        s.title = title;
                        self.title_draft = None;
                    } else if r.has_focus() {
                        self.title_draft = Some(title);
                    }
                    ui.end_row();
                });
                ui.separator();
                // The live layout state JAI controls by voice ("zen mode", "make the orb bigger").
                ui.label("Live layout");
                let mut state = s.layout_state((0.0, 0.0));
                let before = state;
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut state.show_audio_devices_panel, "Audio devices");
                    ui.checkbox(&mut state.show_skills_panel, "Skills");
                    ui.checkbox(&mut state.show_quick_pills_bar, "Quick pills");
                    ui.checkbox(&mut state.show_telemetry_sidebar, "Telemetry");
                    ui.checkbox(&mut state.show_orb_panel, "3D orb");
                    ui.checkbox(&mut state.minimal_zen_mode, "Zen mode");
                });
                egui::Grid::new("hud_live_layout").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label("Orb size");
                    let mut orb = state.orb_radius_scale * 100.0;
                    if ui.add(egui::Slider::new(&mut orb, 40.0..=250.0).step_by(5.0).suffix(" %")).changed() {
                        state.orb_radius_scale = orb / 100.0;
                    }
                    ui.end_row();
                    ui.label("Text size");
                    let mut font = state.ui_font_scale * 100.0;
                    if ui.add(egui::Slider::new(&mut font, 75.0..=160.0).step_by(5.0).suffix(" %")).changed() {
                        state.ui_font_scale = font / 100.0;
                    }
                    ui.end_row();
                });
                if state != before {
                    s.apply_layout_state(&state);
                }
                ui.add_space(6.0);
                if ui.button("Reset to defaults").clicked() {
                    s = InterfaceSettings::default();
                    self.zoom_draft = None;
                    self.title_draft = None;
                }
            });
        self.settings_open = open;
        if &s != iface {
            s.sanitize();
            self.set_interface(s);
        }
    }

    fn offline_overlay(&self, ctx: &egui::Context) {
        let p = self.palette;
        egui::Area::new(egui::Id::new("hud_offline"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let rect = ctx.screen_rect();
                ui.painter().rect_filled(rect, Rounding::ZERO, p.card_solid.gamma_multiply(0.94));
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "J.A.I. IS SHUTTING DOWN", bold(16.0), p.accent);
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
        (true, true, true) => p.red,
        (_, true, false) => p.accent,
        (true, _, _) => p.accent,
        _ => p.muted,
    };
    let fill = match (hot, danger, close) {
        (true, true, true) => p.red.gamma_multiply(0.18),
        (_, true, false) => p.accent.gamma_multiply(0.14),
        (true, _, _) => p.accent.gamma_multiply(0.12),
        _ => Color32::TRANSPARENT,
    };
    ui.painter().rect(rect, Rounding::same(p.r_button().min(12.0)), fill, stroke(1.0, if hot { colour.gamma_multiply(0.6) } else { p.line.gamma_multiply(0.5) }));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(ts(13.0)), colour);
    response
}

/// Puts the floating orb window at its corner (`anchor`) of a monitor (1-based; 0 = the one it
/// is on), `size` logical pixels square; "free" keeps its centre where Sir dragged it.
/// Synchronous window calls: run it on the Win32 worker thread.
#[cfg(windows)]
fn place_orb(hwnd: isize, size: f32, anchor: &str, monitor: u8, ppp: f32) {
    let displays = crate::win32::displays();
    let current = crate::win32::window_display_rect(hwnd);
    let target = match monitor {
        0 => displays.iter().find(|d| Some(d.rect) == current).or_else(|| displays.first()),
        n => displays.get(n as usize - 1).or_else(|| displays.first()),
    };
    let Some(target) = target else { return };
    let side = (size * ppp).round() as i32;
    let margin = (24.0 * ppp).round() as i32;
    let [l, t, r, b] = target.work;
    // Mini mode: a strip across the top of the monitor's work area.
    if anchor == MINI_STRIP {
        if let Err(e) = crate::win32::set_window_rect(hwnd, [l, t, r - l, (MINI_STRIP_HEIGHT * ppp).round() as i32]) {
            warn!("[HUD] Could not place the mini strip: {e}");
        }
        return;
    }
    let (x, y) = match anchor {
        "top-left" => (l + margin, t + margin),
        "top-right" => (r - side - margin, t + margin),
        "bottom-left" => (l + margin, b - side - margin),
        "center" => ((l + r - side) / 2, (t + b - side) / 2),
        "free" => match crate::win32::window_state(hwnd) {
            Some(([x, y, w, h], _, _)) => (x + w / 2 - side / 2, y + h / 2 - side / 2),
            None => (r - side - margin, b - side - margin),
        },
        _ => (r - side - margin, b - side - margin),
    };
    if let Err(e) = crate::win32::set_window_rect(hwnd, [x, y, side, side]) {
        warn!("[HUD] Could not place the orb: {e}");
    }
}

/// A saved instant replay: `[ Open Video Clip ]` `[ Open Folder ]`.
fn clip_card(ui: &mut Ui, p: &Palette, clip: &ClipCardView) {
    let name = std::path::Path::new(&clip.video).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| clip.video.clone());
    Frame::none()
        .fill(p.well)
        .stroke(stroke(1.0, STARK_GOLD.gamma_multiply(0.55)))
        .rounding(Rounding::same(p.r_bubble()))
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .show(ui, |ui| {
            ui.set_max_width((ui.available_width() * 0.9).max(180.0));
            ui.label(RichText::new(format!("INSTANT REPLAY · LAST {} S", clip.seconds)).size(ts(9.5)).color(p.muted).extra_letter_spacing(0.6));
            ui.add(egui::Label::new(RichText::new(&name).font(bold(12.5)).color(STARK_GOLD)).truncate()).on_hover_text(&clip.video);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if button(ui, p, "Open Video Clip", Tone::On).clicked() {
                    open_path(&clip.video, false);
                }
                if ui.add(egui::Button::new(RichText::new("Open Folder").size(ts(12.0)))).clicked() {
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

fn paint_background(ctx: &egui::Context, p: &Palette) {
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    painter.rect_filled(rect, Rounding::ZERO, p.bg);
    if p.oled {
        // True black: every background pixel stays off.
        return;
    }
    let grid = stroke(1.0, p.accent.gamma_multiply(if p.light { 0.07 } else { 0.035 }));
    let mut x = rect.left() + 32.0;
    while x < rect.right() {
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid);
        x += 32.0;
    }
    let mut y = rect.top() + 32.0;
    while y < rect.bottom() {
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], grid);
        y += 32.0;
    }
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
                if ui.add(egui::Button::new(RichText::new(label).size(ts(11.5)).color(if selected { p.accent } else { p.muted })).selected(selected)).clicked() {
                    self.drawer_tab = tab;
                }
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt("hud_drawer").auto_shrink([false; 2]).show(ui, |ui| match self.drawer_tab {
            DrawerTab::Voice => {
                card(ui, &p, "REPLIES", |_| {}, |ui| self.response_mode_selector(ui, m));
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
        let p = self.palette;
        card(ui, &p, "LATENCY & LANGUAGE", |_| {}, |ui| {
            let ms = |v: Option<u64>| v.map(|v| format!("{v} ms")).unwrap_or_else(|| "—".into());
            let [stt, token, audio] = m.latency;
            row(ui, &p, "Speech-to-text", ms(stt)).on_hover_text("Last turn, from the end of your speech");
            row(ui, &p, "First token", ms(token));
            row(ui, &p, "First audio", ms(audio));
            if let Some(cfg) = &m.cfg {
                row(ui, &p, "Language model", &cfg.model);
                let mode = if cfg.voice_muted { "muted".to_string() } else { cfg.response_mode.display() };
                row(ui, &p, "Replies", mode);
                row(ui, &p, "Follow-up window", format!("{} s after each answer", cfg.window_secs));
            }
            row(ui, &p, "Languages", "German, English, Denglisch → British English");
        });
    }

}

// ------------------------------------------------------------------- the HUD board canvas ----
// No chat log and no input bar: an obsidian canvas with the ring emblem, the board of blocks,
// a micro header strip (top right) and a micro audio/telemetry strip (bottom left). Ctrl+L opens
// a small floating command prompt; everything else is voice.

const OBSIDIAN: Color32 = Color32::from_rgb(0x02, 0x04, 0x08);
const STRIP_INK: Color32 = Color32::from_rgb(0xD4, 0xEC, 0xF5);
const STRIP_MUTED: Color32 = Color32::from_rgb(0x7F, 0xA6, 0xB6);
/// A muted microphone and the privacy lock's status.
const MUTED_CRIMSON: Color32 = Color32::from_rgb(0xFF, 0x3A, 0x6A);
/// Header strip and telemetry strip heights.
const HEADER_H: f32 = 16.0;
const TELEMETRY_H: f32 = 14.0;
/// The dot matrix: 1 px dots every 28 px at 6 %.
const DOT_SPACING: f32 = 28.0;
const DOT_ALPHA: f32 = 0.06;
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
    FontId::monospace(10.0)
}

/// The status pill's cross-fade.
const PILL_FADE: f32 = 0.033;

/// Where the screen card stands: under the emblem at home, above the bottom strip while the
/// emblem is docked; always whole on the screen.
fn screen_card_rect(screen: Rect, frame: &crate::rose_emblem_renderer::EmblemFrame) -> Rect {
    let size = vec2(340.0, 72.0);
    let top = if frame.dock < 0.5 { frame.centre.y + frame.radius * 1.32 } else { screen.bottom() - 10.0 - TELEMETRY_H - 14.0 - size.y };
    let x = if frame.dock < 0.5 { frame.centre.x } else { screen.center().x };
    let rect = Rect::from_min_size(pos2(x - size.x / 2.0, top), size);
    let inside = screen.shrink(12.0);
    rect.translate(vec2((inside.left() - rect.left()).max(0.0) + (inside.right() - rect.right()).min(0.0), (inside.top() - rect.top()).max(0.0) + (inside.bottom() - TELEMETRY_H - 14.0 - rect.bottom()).min(0.0)))
}

/// `text` as it fits `width` in `font`: whole, or cut after a character and ended with "…".
fn fit_width(painter: &egui::Painter, text: &str, font: &FontId, width: f32) -> String {
    let fits = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x <= width;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |n: usize| format!("{}…", chars[..n].iter().collect::<String>().trim_end());
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// The frost over the whole HUD while JAI looks at the screen (`screen_context::frost`).
fn paint_frost(ctx: &egui::Context, screen: Rect) {
    let frost = crate::screen_context::frost();
    if frost <= 0.0 {
        return;
    }
    ctx.request_repaint();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("jai_screen_frost")));
    painter.rect_filled(screen, Rounding::ZERO, Color32::from_rgba_unmultiplied(205, 228, 245, (frost * 70.0) as u8));
}

/// Where the key hints end on the right: clear of the docked emblem's corner, which moves left
/// of the text console while that is out (`console` = how far it has slid in).
fn key_hints_right(screen: Rect, console: f32) -> f32 {
    screen.right() - console.max(0.0) - crate::hud_dock::DOCK_INSET - crate::rose_emblem_renderer::DOCKED_DIAMETER - 16.0
}

/// The key hints of the bottom strip, right-aligned to `right` on the line `mid`: each label
/// and its rect, left to right.
fn key_hints(painter: &egui::Painter, right: f32, mid: f32) -> Vec<(&'static str, Rect)> {
    let mut placed = Vec::new();
    let mut x = right;
    for label in ["ESC INTERRUPT", "F11 FULLSCREEN", "F1 MUTE"] {
        let size = painter.layout_no_wrap(label.to_string(), strip_font(), STRIP_MUTED).size();
        let rect = Rect::from_min_size(pos2(x - size.x, mid - size.y / 2.0), size);
        x = rect.left() - 18.0;
        placed.push((label, rect));
    }
    placed.reverse();
    placed
}

/// The status pill's word for the HUD's state.
pub(crate) fn pill_word(state: &str, muted: bool, asleep: bool) -> &'static str {
    match state {
        _ if asleep => "STANDBY",
        "thinking" => "THINKING",
        "speaking" => "SPEAKING",
        _ if muted => "MUTED",
        _ => "LISTENING",
    }
}

/// Every word the status pill shows (`pill_word`).
pub(crate) const PILL_WORDS: [&str; 5] = ["LISTENING", "THINKING", "SPEAKING", "MUTED", "STANDBY"];

/// The pill's colours for its word, as in the reference videos: text, dot, fill, outline. They
/// are the same in every theme (status colours, like the status red).
pub(crate) fn pill_colours(word: &str) -> [Color32; 4] {
    let c = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    match word {
        "LISTENING" => [c(0x7EF0A0), c(0x3EE07A), c(0x0F2A1A), c(0x2F8A55)],
        "THINKING" => [c(0xFFA530), c(0xFFA530), c(0x2A1A06), c(0xA06A10)],
        "SPEAKING" => [c(0xFF5A3A), c(0xFF5A3A), c(0x2A0A08), c(0xB03020)],
        _ => [c(0x8A96A3), c(0x5A6673), c(0x10151B), c(0x3A4552)],
    }
}

/// Where the status pill stands (room for its longest word): centred in the top row, or just
/// left of the header row's boxes where they reach the centre; None where the row leaves no
/// room at all.
fn pill_rect(painter: &egui::Painter, screen: Rect, header: &[Rect]) -> Option<Rect> {
    let width = painter.layout_no_wrap("LISTENING".to_string(), strip_font(), STRIP_INK).size().x + 30.0;
    let size = vec2(width, HEADER_H + 2.0);
    let centred = Rect::from_center_size(pos2(screen.center().x, screen.top() + 10.0 + HEADER_H / 2.0), size);
    if !header.iter().any(|r| r.expand(8.0).intersects(centred)) {
        return Some(centred);
    }
    let row_left = header.iter().map(|r| r.left()).fold(f32::INFINITY, f32::min);
    let beside = Rect::from_min_size(pos2(row_left - 14.0 - width, centred.top()), size);
    (beside.left() >= screen.left() + 12.0).then_some(beside)
}

/// The thin line under the header strip (as in the reference videos): the theme's colour along
/// the whole width, brightening towards a lit middle, with a faint glow under it. It stays
/// above the blocks' area (`block_bounds`).
fn header_rule(painter: &egui::Painter, screen: Rect) {
    let band = header_rule_band(screen);
    let (left, right, y) = (band.left(), band.right(), band.top());
    if right - left < 80.0 {
        return;
    }
    let colour = crate::hud_board::active_colour();
    let mid = (left + right) / 2.0;
    let half = ((right - left) * 0.12).max(40.0);
    // Along the line: where the brightness changes, and how bright it is there.
    let stops = [(left, 0.10), (mid - half, 0.24), (mid, 0.9), (mid + half, 0.24), (right, 0.10)];
    let mut mesh = egui::Mesh::default();
    for (top, bottom, strength) in [(y, y + 1.0, 1.0f32), (y + 1.0, y + 3.0, 0.22)] {
        for pair in stops.windows(2) {
            let ((x0, a0), (x1, a1)) = (pair[0], pair[1]);
            let base = mesh.vertices.len() as u32;
            for (x, a) in [(x0, a0), (x1, a1)] {
                let tint = colour.gamma_multiply(a * strength);
                mesh.colored_vertex(pos2(x, top), tint);
                mesh.colored_vertex(pos2(x, bottom), tint);
            }
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Where the line under the header runs, its glow included.
fn header_rule_band(screen: Rect) -> Rect {
    let y = screen.top() + 12.0 + HEADER_H + 3.0;
    Rect::from_min_max(pos2(screen.left() + 12.0, y), pos2(screen.right() - 12.0, y + 3.0))
}

/// The pill for `word` centred on `centre` at `alpha`: a capsule with a dot and the word.
fn paint_pill(painter: &egui::Painter, centre: Pos2, word: &str, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let [ink, dot, fill, edge] = pill_colours(word);
    let galley = painter.layout_no_wrap(word.to_string(), strip_font(), ink.gamma_multiply(alpha));
    let rect = Rect::from_center_size(centre, vec2(galley.size().x + 30.0, HEADER_H + 2.0));
    let round = Rounding::same(rect.height() / 2.0);
    painter.rect_filled(rect, round, fill.gamma_multiply(alpha));
    painter.rect_stroke(rect, round, stroke(1.0, edge.gamma_multiply(alpha)));
    painter.circle_filled(pos2(rect.left() + 11.0, centre.y), 3.0, dot.gamma_multiply(alpha));
    painter.galley(pos2(rect.left() + 19.0, centre.y - galley.size().y / 2.0), galley, ink.gamma_multiply(alpha));
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

/// The voice rim's ticks round an emblem of `radius` at `centre`, `strength` 0-1, at `clock`
/// seconds: (inner end, outer end, alpha), lengths rippling round the ring.
fn rim_ticks(centre: Pos2, radius: f32, strength: f32, clock: f32) -> Vec<(Pos2, Pos2, f32)> {
    const COUNT: usize = 120;
    let inner = radius * 1.08;
    (0..COUNT)
        .map(|i| {
            let a = i as f32 / COUNT as f32 * TAU;
            let ripple = ((a * 3.0 + clock * 5.1).sin() * (a * 7.0 - clock * 3.3).cos()).abs() * 0.75 + ((a * 13.0 + clock * 9.0).sin() * 0.5 + 0.5) * 0.25;
            let length = 2.0 + radius * 0.2 * strength * ripple;
            let dir = vec2(a.cos(), a.sin());
            (centre + dir * inner, centre + dir * (inner + length), strength * (0.3 + 0.7 * ripple))
        })
        .collect()
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
    settings_open: bool,
    /// Zoom being dragged in the Interface window (applied on release, so the slider holds still).
    zoom_draft: Option<f32>,
    title_draft: Option<String>,
    /// Widgets and JAI's extra windows send their clicks through this.
    widget_host: widget_view::WidgetHost,
    /// The Integrations & Productivity Hub window.
    hub: integrations::Hub,
    workout: workout_view::WorkoutWindow,
    /// The Settings drawer (microphone, devices, system, memory, look) and its open tab.
    drawer_open: bool,
    drawer_tab: DrawerTab,
    /// Pure orb mode as applied to the window: (size px, anchor, monitor, always on top).
    orb_geometry: Option<(f32, String, u8, bool)>,
    /// Where the full HUD was before pure orb mode (restored when it ends).
    normal_placement: Option<[i32; 4]>,
    /// The gyroscopic ring emblem.
    emblem: RoseEmblem,
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
    core: crate::core_sphere_3d::CoreSphereView,
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
    /// `theme_engine::generation()` the palette was built at.
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
    emblem_spot: (Pos2, f32),
    /// The console's own window is to take the keyboard as soon as it is up (Sir called it).
    console_wants_keys: bool,
    /// `JAI_DEMO=desk-click`: how far the stand-in for a click on the desktop has got (frames).
    demo_click: u8,
    /// When the corner orb's window came up, and when it was last looked after (`tend_orb`).
    orb_tended: (Instant, Instant),
}

/// What the corner orb's window shows in a frame.
struct CornerOrbContent {
    /// The orb's diameter (physical pixels).
    orb_px: f32,
    /// What he is at for longer: Some(share done) for a timer, Some(None) for a search.
    progress: Option<Option<f32>>,
    /// The plate beside the orb is up.
    words: bool,
    said: String,
    answered: String,
    timer: Option<String>,
    state: String,
    live_words: bool,
    /// A question of his that waits for yes or no.
    question: Option<String>,
    /// The orders he offers for one he did not know.
    choices: Vec<String>,
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
}

/// The corner orb's window: the emblem in its disc with the ring of what he is at, and beside
/// it the plate with what is being said, his question or the orders he offers.
fn corner_orb_ui(ui: &mut Ui, emblem: &RoseEmblem, c: &CornerOrbContent) -> OrbClick {
    let mut did = OrbClick::Nothing;
    let all = ui.max_rect();
    let orb = Rect::from_min_size(all.min, Vec2::splat(all.height().min(c.orb_px / ui.ctx().pixels_per_point())));
    let response = ui.allocate_rect(orb, Sense::click_and_drag());
    if response.drag_started_by(egui::PointerButton::Primary) {
        did = OrbClick::Drag;
    } else if response.double_clicked() {
        did = OrbClick::Board;
    } else if response.clicked() {
        did = OrbClick::Console;
    }
    // Nothing behind it (Sir, 2026-11-14: "the orb in the corner shouldn't have a background
    // colour"): the window's black is see-through (`win32_desktop::key_out_black`), and the
    // emblem is painted without the light round its lines, which on black would be a dark veil.
    emblem.paint_in_crisp(&ui.painter().with_clip_rect(orb), orb);
    if let Some(share) = c.progress {
        let (centre, r) = (orb.center(), orb.width() * 0.5 - 3.0);
        let (from, sweep) = match share {
            Some(done) => (-TAU / 4.0, TAU * done.clamp(0.0, 1.0)),
            None => ((ui.input(|i| i.time) as f32 * 2.2) % TAU, TAU * 0.22),
        };
        let arc: Vec<Pos2> = (0..=48).map(|k| from + sweep * k as f32 / 48.0).map(|a| centre + vec2(a.cos(), a.sin()) * r).collect();
        ui.painter().add(Shape::line(arc, stroke(2.5, crate::hud_board::active_colour())));
    }
    response.on_hover_text("Drag to move · click for the console · double-click for the board");
    if !c.words {
        return did;
    }
    let plate = Rect::from_min_max(pos2(orb.right() + 12.0, all.top()), all.max);
    let p = ui.painter().with_clip_rect(plate);
    p.rect_filled(plate, Rounding::ZERO, rgba([3, 7, 12], 0.9));
    inner_border(&p, plate, 1.0, crate::hud_board::border_colour());
    let inner = plate.shrink2(vec2(12.0, 9.0));
    let mut y = inner.top();
    if let Some(label) = &c.timer {
        p.text(inner.right_bottom(), Align2::RIGHT_BOTTOM, label, strip_font(), crate::hud_board::active_colour());
    }
    if !c.said.is_empty() {
        let line = crate::hud_lines::fit(&p, &c.said, &strip_font(), inner.width());
        y = p.text(pos2(inner.left(), y), Align2::LEFT_TOP, line, strip_font(), STRIP_MUTED).bottom() + 5.0;
    }
    let key = |ui: &mut Ui, at: Pos2, text: &str, lit: bool| -> (bool, f32) {
        let ink = if lit { crate::hud_board::active_colour() } else { STRIP_INK };
        let galley = ui.painter().layout_no_wrap(text.to_string(), strip_font(), ink);
        let rect = Rect::from_min_size(at, galley.size() + vec2(14.0, 8.0));
        let response = ui.interact(rect, ui.id().with(("corner_orb_key", text.to_string())), Sense::click());
        ui.painter().rect_filled(rect, Rounding::ZERO, rgba([3, 7, 12], 0.9));
        ui.painter().rect_stroke(rect, Rounding::ZERO, stroke(1.0, if response.hovered() || lit { crate::hud_board::active_colour() } else { crate::hud_board::border_colour() }));
        ui.painter().galley(rect.min + vec2(7.0, 4.0), galley, ink);
        (response.clicked(), rect.right())
    };
    if let Some(question) = &c.question {
        // His question before a guess is carried out, and its two keys.
        let galley = p.layout(crate::crash_guard::clip(question, 160).to_string(), FontId::proportional(13.5), STRIP_INK, inner.width());
        let h = galley.size().y;
        p.galley(pos2(inner.left(), y), galley, STRIP_INK);
        let row = pos2(inner.left(), (y + h + 7.0).min(inner.bottom() - 24.0));
        let (yes, right) = key(ui, row, "[ YES ]", true);
        let (no, _) = key(ui, pos2(right + 8.0, row.y), "[ NO ]", false);
        if yes {
            did = OrbClick::Yes;
        } else if no {
            did = OrbClick::No;
        }
    } else if !c.choices.is_empty() {
        // The orders closest to the one he did not know: a click (or "the first one") takes one.
        p.text(pos2(inner.left(), y), Align2::LEFT_TOP, "DID YOU MEAN", FontId::monospace(10.0), crate::hud_board::active_colour());
        let mut at = pos2(inner.left(), y + 16.0);
        for (k, label) in c.choices.iter().enumerate().take(3) {
            let text = crate::hud_lines::fit(&p, label, &strip_font(), inner.width() - 20.0);
            let (clicked, _) = key(ui, at, &text, false);
            if clicked {
                did = OrbClick::Choice(k);
            }
            at.y += 27.0;
        }
    } else if !c.answered.is_empty() {
        let galley = p.layout(crate::crash_guard::clip(&c.answered, 220).to_string(), FontId::proportional(13.5), STRIP_INK, inner.width());
        p.with_clip_rect(Rect::from_min_max(pos2(inner.left(), y), inner.max)).galley(pos2(inner.left(), y), galley, STRIP_INK);
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

/// The music's spectrum round the emblem: each band a pair of ticks, the low ones at the bottom,
/// the high ones at the top, the same on both sides.
fn paint_spectrum_ring(painter: &egui::Painter, frame: &crate::rose_emblem_renderer::EmblemFrame, bands: &[f32; crate::pc_audio::BANDS]) {
    const PER_SIDE: usize = 2 * crate::pc_audio::BANDS;
    let accent = crate::hud_board::active_colour();
    // (Clear of the housing's rim and of the four marks on its diagonals.)
    let inner = frame.radius * 1.15;
    let reach = 30.0 * (frame.radius / 175.0).clamp(0.5, 1.6);
    for k in 0..PER_SIDE {
        let at = (k as f32 + 0.5) / PER_SIDE as f32;
        // Between two bands: the mean of them.
        let band = at * (crate::pc_audio::BANDS - 1) as f32;
        let (low, mix) = (band.floor() as usize, band.fract());
        let value = bands[low] * (1.0 - mix) + bands[(low + 1).min(crate::pc_audio::BANDS - 1)] * mix;
        // (Every tick stands, however quiet its band: a whole ring that breathes, not a scatter
        // of dashes where a band happens to sound.)
        let colour = mix_colour(accent, Color32::WHITE, ((value - 0.8) * 4.0).clamp(0.0, 0.8)).gamma_multiply(0.2 + 0.8 * value);
        for side in [1.0f32, -1.0] {
            // From straight down (the bass) up to straight up (the highs).
            let angle = std::f32::consts::FRAC_PI_2 - side * at * std::f32::consts::PI;
            let dir = vec2(angle.cos(), angle.sin());
            painter.line_segment([frame.centre + dir * inner, frame.centre + dir * (inner + 3.0 + reach * value)], stroke(1.6, colour));
        }
    }
}

/// `a` → `b` by `t` (opaque colours).
fn mix_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    mix(a, b, t)
}

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
/// small emblem that turns, his name and what he is doing; in the middle Sir's last words and
/// his last answer; on the right the running timers, the time and the key back to the board.
/// Returns whether that key was clicked.
fn paint_mini_strip(ui: &mut Ui, s: &MiniStrip) -> bool {
    let accent = crate::hud_board::active_colour();
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    let mid = rect.center().y;
    crate::hud_style::surface(&p, rect, Color32::from_rgb(5, 9, 15), 1.0);
    p.rect_filled(Rect::from_min_size(pos2(rect.left(), rect.bottom() - 1.0), vec2(rect.width(), 1.0)), Rounding::ZERO, accent.gamma_multiply(0.85));
    // The emblem in small: two arcs that turn against each other round a lamp in the colour of
    // what he is doing.
    let lamp = pill_colours(s.word)[0];
    let centre = pos2(rect.left() + 20.0, mid);
    let turn = ui.input(|i| i.time) as f32 * if matches!(s.word, "THINKING" | "SPEAKING") { 2.4 } else { 0.5 };
    p.add(Shape::line(arc_points(centre, 10.5, turn, turn + 4.2), stroke(1.4, accent)));
    p.add(Shape::line(arc_points(centre, 7.0, -turn * 1.3, -turn * 1.3 + 3.4), stroke(1.0, Color32::WHITE.gamma_multiply(0.8))));
    p.circle_filled(centre, 2.6, lamp);
    p.circle_filled(centre, 5.0, lamp.gamma_multiply(0.2));
    let name = crate::hud_style::title_galley(&p, "J.A.I.", 15.0, accent, 120.0);
    let name_right = centre.x + 18.0 + name.size().x;
    p.galley(pos2(centre.x + 18.0, mid - name.size().y * 0.5), name, Color32::PLACEHOLDER);
    let mut x = p.text(pos2(name_right + 12.0, mid + 0.5), Align2::LEFT_CENTER, s.word, FontId::monospace(10.5), lamp).right() + 26.0;
    // On the right: the key back to the board, the time, the running timers.
    let (key, keys_left) = crate::hud_style::keys(ui, egui::Id::new("mini_strip"), rect.right() - 10.0, mid, &[("[ BOARD ]", false, "Back to the full board (or say: mini mode off)")], 1.0);
    let mut right = p.text(pos2(keys_left - 14.0, mid + 0.5), Align2::RIGHT_CENTER, &s.clock, FontId::monospace(13.0), STRIP_INK).left() - 18.0;
    for timer in &s.timers {
        right = p.text(pos2(right, mid + 0.5), Align2::RIGHT_CENTER, timer, FontId::monospace(11.0), accent).left() - 18.0;
    }
    // In between: his last words and JAI's last answer, each on one line and cut to its room.
    let room = (right - x).max(0.0);
    let font = FontId::proportional(12.5);
    if room > 180.0 {
        let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !s.sir.trim().is_empty() {
            let tag = p.text(pos2(x, mid + 0.5), Align2::LEFT_CENTER, "YOU", FontId::monospace(9.5), STRIP_MUTED).right() + 8.0;
            let shown = fit_width(&p, &one_line(&s.sir), &font, (room * 0.36 - 40.0).max(40.0));
            x = p.text(pos2(tag, mid), Align2::LEFT_CENTER, shown, font.clone(), STRIP_INK).right() + 24.0;
        }
        if !s.jai.trim().is_empty() && right - x > 90.0 {
            let tag = p.text(pos2(x, mid + 0.5), Align2::LEFT_CENTER, "J.A.I.", FontId::monospace(9.5), accent.gamma_multiply(0.85)).right() + 8.0;
            let shown = fit_width(&p, &one_line(&s.jai), &font, (right - tag).max(40.0));
            p.text(pos2(tag, mid), Align2::LEFT_CENTER, shown, font, Color32::WHITE);
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
fn emblem_inputs(m: &HudModel, console_open: bool, core_open: bool, centre_covered: bool, idle: bool) -> EmblemInputs {
    use crate::hud_board::ScreenState;
    let screen = crate::hud_board::screen_state();
    EmblemInputs {
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
        install_fonts(&cc.egui_ctx);
        configure_style(&cc.egui_ctx, app.config().read(|c| c.interface.font_scale));
        crate::theme_engine::attach_database(&app.config().read(|c| c.memory_db_path.clone()));
        *lock(&SHARED.ctx) = Some(cc.egui_ctx.clone());
        #[cfg(windows)]
        let hwnd = native_window(cc);
        #[cfg(windows)]
        if hwnd != 0 {
            crate::win32::install_hud_quit_hook(hwnd);
            if let Some(rect) = restore {
                crate::win32::restore_placement(hwnd, rect);
            }
            SHARED.hwnd.store(hwnd, Ordering::Release);
            for command in lock(&SHARED.pending).drain(..) {
                crate::win32::run_detached(move || apply_window_command(hwnd, command));
            }
            // He starts as the desktop's background: the desktop is handed the window before
            // it is first shown, so the board never lies over Sir's windows on its way there.
            let iface = app.config().read(|c| c.interface.clone());
            if let Some(rect) = (iface.desktop_background && !iface.orb_mode).then(|| background_rect(&iface)).flatten() {
                crate::wallpaper::arrive(hwnd, rect, cc.egui_ctx.native_pixels_per_point().unwrap_or(1.0));
            }
        }
        #[cfg(not(windows))]
        let hwnd = {
            let _ = restore;
            0
        };
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
            normal_placement: None,
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
            settings_open: false,
            zoom_draft: None,
            title_draft: None,
            emblem: RoseEmblem::new(crate::rose_emblem_renderer::resolve_state(inputs)),
            board: crate::hud_board::BoardView::default(),
            core: crate::core_sphere_3d::CoreSphereView::default(),
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
            backdrop: crate::wallpaper::Standing::Off,
            corner_orb: Default::default(),
            orb_out_on: None,
            orb_words: (String::new(), String::new(), now),
            emblem_spot: (Pos2::ZERO, 0.0),
            console_wants_keys: false,
            demo_click: 0,
            orb_tended: (Instant::now(), Instant::now()),
        }
    }

    /// Whether the board is (or is becoming) the desktop's background: then it is no window,
    /// and none of a window's ways (fullscreen, a title bar, a move) is its own.
    fn as_background(&self) -> bool {
        matches!(self.backdrop, crate::wallpaper::Standing::Arriving | crate::wallpaper::Standing::On)
    }

    /// He as the desktop's background (`wallpaper`): the board's window is handed to the
    /// desktop once it stands, plain, where it is to be, and taken back when the switch goes
    /// off. A refusal puts the switch back and says why. While he is the background every view
    /// of his is a window of its own (`hud_dock::all_own`).
    fn sync_background(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        use crate::wallpaper::Standing;
        let rect = (iface.desktop_background && !iface.orb_mode && self.hwnd != 0 && !ctx.embed_viewports()).then(|| background_rect(iface)).flatten();
        let now = crate::wallpaper::tick(self.hwnd, rect, ctx.native_pixels_per_point().unwrap_or(1.0), crate::hud_feel::game_in_front());
        if now == self.backdrop {
            return;
        }
        if let Standing::Refused(why) = &now {
            lock(&self.model).toast(format!("The desktop would not take me as its background ({why})."), true);
            self.app.config().update(|c| c.interface.desktop_background = false);
        }
        self.backdrop = now;
        crate::hud_dock::set_all_own(self.as_background());
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
            let accent = crate::hud_board::active_colour();
            painter.rect_filled(screen, Rounding::ZERO, rgba([3, 7, 12], 0.55));
            let zone = Rect::from_center_size(screen.center(), vec2(420.0, 120.0));
            painter.rect_stroke(zone, Rounding::ZERO, stroke(1.5, accent));
            crate::hud_board::brackets(&painter, zone.expand(6.0), accent);
            painter.text(zone.center(), Align2::CENTER_CENTER, "+ DROP THE FILE HERE", FontId::monospace(16.0), accent);
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

    /// True fullscreen ↔ maximised windowed mode (F11 and the strip's `F11 FULLSCREEN`).
    fn toggle_fullscreen(&mut self) {
        let mut iface = self.app.config().read(|c| c.interface.clone());
        iface.fullscreen = !iface.fullscreen;
        iface.orb_mode = false;
        info!("[HUD] F11: {}", if iface.fullscreen { "fullscreen" } else { "windowed (maximised)" });
        self.set_interface(iface);
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
        // F11: true fullscreen ↔ maximised windowed mode.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F11)) {
            self.toggle_fullscreen();
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
        let before = self.emblem.state();
        self.emblem.follow(inputs);
        if self.emblem.state() != before {
            info!("[HUD] Emblem: {:?} → {:?}", before, self.emblem.state());
        }
        // Its mood: what he is doing (and one red flash for an order that failed).
        use crate::rose_emblem_renderer::Mood;
        self.emblem.set_mood(match m.state.as_str() {
            "listening" => Mood::Listening,
            "thinking" => Mood::Thinking,
            "speaking" => Mood::Speaking,
            _ => Mood::Rest,
        });
        if crate::hud_lines::take_fault() {
            self.emblem.flash_fault();
        }
        self.emblem.step(dt, self.reactor.emblem_voice().max(welcome_level() * 0.6), inputs.busy());
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
            } else if iface.orb_mode {
                "the floating orb"
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
        if self.as_background() || (iface.desktop_background && !iface.orb_mode && background_rect(iface).is_some()) {
            return;
        }
        // "Use all monitors": the window covers the box around every monitor.
        #[cfg(windows)]
        if iface.span_all_monitors && !iface.orb_mode && self.hwnd != 0 && (self.frames == 2 || self.fullscreen_check.elapsed() > Duration::from_millis(1500)) {
            self.fullscreen_check = Instant::now();
            let hwnd = self.hwnd;
            if let Some(bounds) = crate::monitors::virtual_bounds(&crate::monitors::list()) {
                let state = crate::win32::window_state(hwnd);
                if state.as_ref().is_none_or(|(r, _, _)| r.iter().zip(bounds).any(|(a, b)| (a - b).abs() > 2)) {
                    let maximised = state.is_some_and(|(_, max, _)| max);
                    crate::win32::run_detached(move || {
                        if maximised {
                            crate::win32::show_window(hwnd, crate::win32::WindowCmd::Restore);
                        }
                        if let Err(e) = crate::win32::set_window_rect(hwnd, bounds) {
                            warn!("[HUD] Could not span the monitors: {e}");
                        }
                    });
                }
            }
        }
        if !iface.fullscreen || iface.orb_mode {
            return;
        }
        // (A test session's board stays the borderless window that covers its monitor.)
        let quiet = crate::platform::quiet_session();
        if self.frames == 1 && !quiet {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
        }
        #[cfg(windows)]
        if self.hwnd != 0 && self.frames > 3 && self.fullscreen_check.elapsed() > Duration::from_millis(1500) {
            self.fullscreen_check = Instant::now();
            let hwnd = self.hwnd;
            let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
            if !minimized && crate::win32::is_visible(hwnd) && !crate::win32::covers_monitor(hwnd) {
                self.fullscreen_fixes += 1;
                if self.fullscreen_fixes <= 3 || self.fullscreen_fixes.is_multiple_of(20) {
                    warn!("[HUD] The fullscreen board does not cover its monitor; re-applying ({}×)", self.fullscreen_fixes);
                }
                if !quiet {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                }
                crate::win32::run_detached(move || {
                    if let Err(e) = crate::win32::cover_monitor(hwnd) {
                        warn!("[HUD] Could not stretch the board over its monitor: {e}");
                    }
                });
            } else if !minimized && crate::win32::window_state(hwnd).is_some_and(|(_, maximised, _)| maximised) {
                // It covers the monitor but is still maximised underneath (a maximised window
                // that went fullscreen, F11): a plain window of the same rect, nothing moves.
                crate::win32::run_detached(move || match crate::win32::unmaximise_in_place(hwnd) {
                    Ok(true) => info!("[HUD] The fullscreen board is no longer maximised underneath"),
                    Ok(false) => {}
                    Err(e) => warn!("[HUD] Could not let go of the board's maximised state: {e}"),
                });
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
        let now_ms = chrono::Utc::now().timestamp_millis();
        let timers = self
            .app
            .tools()
            .widgets
            .widgets()
            .iter()
            .rev()
            .filter_map(|w| {
                let crate::widgets::WidgetKind::CountdownOrStopwatch { running: true, .. } = &w.kind else { return None };
                let (elapsed, remaining) = crate::widgets::timer_state(&w.kind, now_ms)?;
                let secs = remaining.unwrap_or(elapsed) / 1000;
                Some(format!("{}  {:02}:{:02}", if remaining.is_some() { "TIMER" } else { "STOPWATCH" }, secs / 60, secs % 60))
            })
            .collect();
        let last = |who: Speaker| m.messages.iter().rev().find(|l| l.who == who).map(|l| l.text.clone()).unwrap_or_default();
        let strip = MiniStrip { word: pill_word(&m.state, muted, asleep), sir: last(Speaker::Sir), jai: last(Speaker::Jai), timers, clock: chrono::Local::now().format("%H:%M").to_string() };
        let mut leave = false;
        egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| leave = paint_mini_strip(ui, &strip));
        if leave {
            self.app.config().update(|c| c.interface.mini_strip = false);
        }
        // (The time, the timers and the little emblem move.)
        ctx.request_repaint_after(Duration::from_millis(if m.state == "idle" { 250 } else { 50 }));
    }

    fn pure_orb(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings) {
        if iface.orb_anchor == MINI_STRIP {
            self.mini_strip(ctx, m);
            return;
        }
        let p = self.palette;
        egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
            let rect = ui.max_rect();
            let response = ui.allocate_rect(rect, Sense::click_and_drag());
            if response.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                if iface.orb_anchor != "free" {
                    // It stays where Sir leaves it (a resize keeps its centre).
                    let mut next = iface.clone();
                    next.orb_anchor = "free".into();
                    self.orb_geometry = Some((next.orb_size_px, next.orb_anchor.clone(), next.orb_monitor, next.orb_on_top));
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
            } else if response.clicked() && crate::vram_guard::is_dormant() {
                // The floating emblem is a wake button during the zero-VRAM standby.
                crate::vram_guard::wake("orb");
            }
            ui.set_opacity(iface.orb_opacity);
            self.emblem.paint_in(&ui.painter().with_clip_rect(rect), rect);
            // A silent quick command's badge, briefly, under the emblem.
            if let Some((label, ok, until)) = &m.quick {
                if *until > Instant::now() {
                    let at = pos2(rect.center().x, rect.bottom() - 14.0);
                    let galley = ui.painter().layout_no_wrap(label.clone(), strip_font(), if *ok { STARK_GOLD } else { p.red });
                    let pill_rect = Rect::from_center_size(at, galley.size() + vec2(14.0, 6.0));
                    ui.painter().rect_filled(pill_rect, Rounding::ZERO, rgba([4, 10, 20], 0.85));
                    ui.painter().galley(pill_rect.min + vec2(7.0, 3.0), galley, Color32::WHITE);
                }
            }
            response.on_hover_text("Drag to move · wheel to resize · double-click for the full HUD");
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
                use crate::core_sphere_3d::CoreCommand;
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
            self.centre_covered = iface.orb_aside || crate::hud_dock::centre_covered(screen, &board_rects) || crate::hud_dock::stage_covers_centre(screen);
            // The board fades out while the core sphere fades in over it.
            let core_t = ctx.animate_bool_with_time(egui::Id::new("hud_core_view"), self.core_open, crate::hud_style::motion::SLOW.as_secs_f32());
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
                let voice = self.reactor.voice;
                ui.scope(|ui| {
                    ui.set_opacity(core_t);
                    self.core.show(ui, area, voice);
                });
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
            // The emblem (drawn over the board; its corner cell is kept free), wherever Sir put it.
            self.emblem.glide(screen, dt);
            // (While the orb is out in a corner the background has no emblem: it has left it.)
            if self.corner_orb.is_home() {
                self.emblem.paint(&ui.painter_at(screen), screen);
            }
            let frame = self.emblem.frame(screen);
            self.emblem_spot = (frame.centre, frame.radius);
            // (Said once, a moment after the start: where the emblem stands, in the log.)
            if self.frames == 150 {
                info!("[HUD] Emblem at {:.0},{:.0} (radius {:.0}) on a screen of {:.0}×{:.0} at {:.0},{:.0}", frame.centre.x, frame.centre.y, frame.radius, screen.width(), screen.height(), screen.left(), screen.top());
            }
            // New windows open around the orb and the board's blocks, never on them.
            let mut taken = board_rects;
            taken.extend(self.system_column_rect.filter(|_| system));
            crate::hud_dock::publish_obstacles(Some(Rect::from_center_size(frame.centre, Vec2::splat(frame.radius * 2.3))), taken);
            let docked = crate::hud_dock::docked_centre(screen, self.console_reserve());
            crate::hud_dock::publish_docked_spot(Some(Rect::from_center_size(docked, Vec2::splat(crate::rose_emblem_renderer::DOCKED_DIAMETER + 8.0))));
            let hit = Rect::from_center_size(frame.centre, vec2(frame.radius * 1.6, frame.radius * 1.6));
            let emblem = ui.interact(hit, ui.id().with("hud_emblem"), Sense::click_and_drag());
            // Drag the emblem anywhere on the board; it stays there (and windows open around it).
            // (Near the middle of the screen it takes the middle: its home. No lines are drawn
            // for it any more: Sir wants no alignment help on his screen, 2026-11-13.)
            let middle = screen.center();
            let near_middle = |p: Pos2| (p - middle).length() <= EMBLEM_SNAP * screen.width().min(screen.height());
            if let Some(p) = emblem.interact_pointer_pos().filter(|_| emblem.dragged_by(egui::PointerButton::Primary)) {
                let p = if near_middle(p) { middle } else { p };
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
                if crate::hud_style::ripple(&ui.painter_at(screen), frame.centre, frame.radius, age, crate::hud_board::active_colour()) {
                    ctx.request_repaint();
                }
            }
            if iface.voice_rim {
                self.voice_rim(&ui.painter_at(screen), m, &frame, dt);
            }
            // The voice lock turned an order away: the emblem says why nothing happens.
            if let Some(strength) = crate::hud_lines::turned_away_strength().filter(|_| frame.dock < 0.5) {
                crate::hud_lines::paint_lock_label(&ui.painter_at(screen), frame.centre, frame.radius, strength);
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            // The lines under the emblem: his words while he speaks, what JAI is working on,
            // the song's line, and the offer of an order heard without his name.
            let lines_top = if frame.dock < 0.5 { crate::hud_lines::top_under(frame.centre, frame.radius) } else { pos2(screen.center().x, screen.bottom() - TELEMETRY_H - 110.0) };
            // The spectrum as a ring of ticks round the emblem (when Sir's own voice is not there).
            if music && frame.dock < 0.5 && self.voice_rim < 0.05 {
                paint_spectrum_ring(&ui.painter_at(screen), &frame, &crate::pc_audio::bands());
                ctx.request_repaint_after(Duration::from_millis(if crate::hud_feel::game_in_front() { 40 } else { 16 }));
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
                header_rule(&ui.painter_at(screen), screen);
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
            self.day_panel(ctx, home, frame.centre, frame.radius);
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
            self.idle = crate::hud_style::motion::approach(self.idle, target, dt, if idle { Duration::from_millis(1200) } else { crate::hud_style::motion::SLOW });
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
            self.calm = crate::hud_style::motion::approach(self.calm, target, dt, crate::hud_style::motion::NORMAL);
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
        let row = header_layout(&items, screen.left() + 12.0, screen.right() - 12.0, y, |t| painter.layout_no_wrap(t.to_string(), strip_font(), STRIP_INK).size().x);
        self.header_taken = row.placed.iter().map(|(_, r)| *r).chain(row.more).collect();
        // On a screen too narrow for the whole strip: `[ MORE ]` holds the boxes that are not
        // on the row (lit while one of them is).
        if let Some(rect) = row.more {
            let response = ui.interact(rect, ui.id().with(("hud_strip", "more")), Sense::click());
            let lit = row.hidden.iter().any(|id| items.iter().any(|i| i.id == *id && i.on));
            let colour = crate::hud_board::active_colour();
            painter.rect_filled(rect, Rounding::ZERO, if lit { colour.gamma_multiply(0.14) } else { rgba([3, 7, 12], 0.9) });
            painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, if response.hovered() || lit { colour } else { STRIP_INK.gamma_multiply(0.7) }));
            painter.text(rect.center(), Align2::CENTER_CENTER, "MORE", strip_font(), if lit { colour } else { STRIP_INK });
            if response.clicked() {
                ui.memory_mut(|mem| mem.toggle_popup(more_id));
            }
            egui::popup::popup_below_widget(ui, more_id, &response, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(190.0);
                for id in row.hidden.iter().copied() {
                    let Some(item) = items.iter().find(|i| i.id == id) else { continue };
                    let ink = if item.on { crate::hud_board::active_colour() } else { STRIP_INK };
                    if ui.add(egui::Button::new(RichText::new(item.text.as_str()).font(strip_font()).color(ink)).frame(false)).clicked() {
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
            // The menus of the boxes it holds hang off it.
            for id in row.hidden.iter().copied().filter(|id| matches!(*id, "theme" | "modules" | "view" | "source")) {
                anchors.push((id, response.clone()));
            }
        }
        for (id, rect) in row.placed {
            let item = items.iter().find(|i| i.id == id).expect("laid-out item");
            if !item.boxed {
                painter.text(rect.left_center(), Align2::LEFT_CENTER, item.text.as_str(), strip_font(), STRIP_MUTED);
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
            let hot = response.hovered() || item.on;
            // A muted microphone reads crimson whatever the theme.
            let lit_colour = if id == "mic" { MUTED_CRIMSON } else { crate::hud_board::active_colour() };
            painter.rect_filled(rect, Rounding::ZERO, if item.on { lit_colour.gamma_multiply(0.14) } else { rgba([3, 7, 12], 0.9) });
            painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, if hot { lit_colour } else { STRIP_INK.gamma_multiply(0.7) }));
            let ink = if item.on { lit_colour } else { STRIP_INK };
            if id == "source" {
                // A picker: the device name left, the v marker right.
                painter.text(rect.left_center() + vec2(7.0, 0.0), Align2::LEFT_CENTER, item.text.as_str(), strip_font(), ink);
                painter.text(rect.right_center() - vec2(7.0, 0.0), Align2::RIGHT_CENTER, "v", strip_font(), ink);
            } else {
                painter.text(rect.center(), Align2::CENTER_CENTER, item.text.as_str(), strip_font(), ink);
            }
            if response.clicked() {
                clicked = Some(id);
            }
            // `[ THEME ]`: a click cycles the presets, a right-click shows all eight as swatches.
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
            Some("theme") => self.apply_palette(ctx, iface, crate::theme_engine::next_preset(&iface.hud_palette)),
            Some("phone") => pairing_view::toggle(),
            Some("core") => core_command(crate::core_sphere_3d::CoreCommand::Open(!self.core_open)),
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
            painter.text(pos2(screen.right() - 12.0, y + 30.0), Align2::RIGHT_TOP, status, strip_font(), MUTED_CRIMSON);
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        // `[ THEME ]` right-click: the eight presets as swatches (primary, contrast, border).
        if let Some(button) = anchor("theme") {
            let mut picked: Option<&'static str> = None;
            let mut load: Option<String> = None;
            let mut create = false;
            egui::popup::popup_below_widget(ui, theme_id, &button, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(230.0);
                for (name, p) in crate::theme_engine::PRESETS.iter() {
                    let active = iface.hud_palette == *name;
                    let (row, response) = ui.allocate_exact_size(vec2(226.0, 20.0), Sense::click());
                    let painter = ui.painter_at(row);
                    if response.hovered() || active {
                        painter.rect_filled(row, Rounding::ZERO, p.border().gamma_multiply(0.16));
                    }
                    for (k, c) in [p.primary(), p.secondary(), p.border()].into_iter().enumerate() {
                        painter.rect_filled(Rect::from_min_size(row.left_center() + vec2(4.0 + k as f32 * 13.0, -5.0), vec2(10.0, 10.0)), Rounding::ZERO, c);
                    }
                    let label = crate::theme_engine::label(name);
                    painter.text(row.left_center() + vec2(48.0, 0.0), Align2::LEFT_CENTER, if active { format!("{label}") } else { label }, strip_font(), STRIP_INK);
                    if response.clicked() {
                        picked = Some(name);
                    }
                }
                // Sir's own themes, then the studio.
                for theme in crate::theme_engine::custom_themes() {
                    let p = theme.palette();
                    let (row, response) = ui.allocate_exact_size(vec2(226.0, 20.0), Sense::click());
                    let painter = ui.painter_at(row);
                    let active = iface.hud_palette == theme.key();
                    if response.hovered() || active {
                        painter.rect_filled(row, Rounding::ZERO, p.border().gamma_multiply(0.16));
                    }
                    for (k, c) in [p.primary(), p.secondary(), p.border()].into_iter().enumerate() {
                        painter.rect_filled(Rect::from_min_size(row.left_center() + vec2(4.0 + k as f32 * 13.0, -5.0), vec2(10.0, 10.0)), Rounding::ZERO, c);
                    }
                    let label = theme.name.to_uppercase();
                    painter.text(row.left_center() + vec2(48.0, 0.0), Align2::LEFT_CENTER, if active { format!("{label}") } else { label }, strip_font(), STRIP_INK);
                    if response.clicked() {
                        load = Some(theme.name.clone());
                    }
                }
                ui.separator();
                if ui.add(egui::Button::new(RichText::new("[ + CREATE THEME ]").font(strip_font()).color(crate::hud_board::active_colour())).frame(false)).clicked() {
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
        // `[ MODULES ]`: one click to each Stark module (the tool runs directly, no language model).
        if let Some(button) = anchor("modules") {
            let mut run: Option<&[(&'static str, &'static str)]> = None;
            egui::popup::popup_below_widget(ui, modules_id, &button, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(330.0);
                for (label, calls) in MODULES {
                    if ui.add(egui::Button::new(RichText::new(*label).font(strip_font()).color(STRIP_INK)).frame(false)).clicked() {
                        run = Some(calls);
                    }
                }
                if ui.add(egui::Button::new(RichText::new("DISCORD // AUDIO ROUTING — VOICE INTO THE CALL").font(strip_font()).color(STRIP_INK)).frame(false)).clicked() {
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
                if devices.is_empty() {
                    ui.label(RichText::new("NO CAPTURE DEVICES FOUND").font(strip_font()).color(STRIP_MUTED));
                }
                for (i, name) in devices.iter().enumerate() {
                    let label = if i == 0 { format!("DEFAULT — {}", name.to_uppercase()) } else { name.to_uppercase() };
                    if ui.selectable_label(self.camera_device == i, RichText::new(label).font(strip_font())).clicked() {
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
        // `[ VIEW ]`: the navigation, the window and the settings.
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
            ui.set_min_width(250.0);
            let item = |ui: &mut Ui, text: &str| ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(STRIP_INK)).frame(false)).clicked();
            ui.label(RichText::new("NAVIGATE").font(strip_font()).color(STRIP_MUTED));
            for (mode, on) in nav {
                if item(ui, &format!("{} {}", if on { "[x]" } else { "[ ]" }, mode.label())) {
                    chosen = Some(mode);
                }
            }
            if item(ui, "  DOCK ALL WINDOWS INSIDE") {
                crate::hud_dock::dock_all();
            }
            ui.separator();
            if item(ui, if iface.comms_block { "COMMS BLOCK: ON" } else { "COMMS BLOCK: OFF" }) {
                let mut next = iface.clone();
                next.comms_block = !iface.comms_block;
                self.set_interface(next);
            }
            if item(ui, if iface.status_pill { "STATUS PILL: ON" } else { "STATUS PILL: OFF" }) {
                let mut next = iface.clone();
                next.status_pill = !iface.status_pill;
                self.set_interface(next);
            }
            if item(ui, if iface.voice_rim { "VOICE RIM: ON" } else { "VOICE RIM: OFF" }) {
                let mut next = iface.clone();
                next.voice_rim = !iface.voice_rim;
                self.set_interface(next);
            }
            if item(ui, if iface.sound_cues { "SOUND CUES: ON" } else { "SOUND CUES: OFF" }) {
                let mut next = iface.clone();
                next.sound_cues = !iface.sound_cues;
                self.set_interface(next);
            }
            if item(ui, "  TIDY THE WINDOWS") {
                crate::hud_dock::retile_all(block_bounds(screen));
            }
            if item(ui, "  FIND IN BLOCKS (CTRL+F)") {
                self.find_open = true;
                self.find_focus = true;
            }
            if item(ui, if wake_word { "MODE: WAKE WORD → HANDS-FREE" } else { "MODE: HANDS-FREE → WAKE WORD" }) {
                self.command(HudCommand::SetWakeWordRequired { required: !wake_word });
            }
            if item(ui, if dormant { "VRAM: STANDBY — WAKE" } else { "VRAM: CLEAR THE GPU NOW" }) {
                if dormant {
                    crate::vram_guard::wake("HUD view menu");
                } else {
                    crate::vram_guard::flush_with_reason("cleared from the HUD");
                }
            }
            ui.separator();
            if item(ui, if iface.fullscreen { "EXIT FULLSCREEN" } else { "FULLSCREEN" }) {
                let mut next = iface.clone();
                next.fullscreen = !iface.fullscreen;
                self.set_interface(next);
            }
            if item(ui, "PURE EMBLEM MODE") {
                let mut next = iface.clone();
                next.orb_mode = true;
                self.set_interface(next);
            }
            if item(ui, if iface.always_on_top { "ALWAYS ON TOP: ON" } else { "ALWAYS ON TOP: OFF" }) {
                let mut next = iface.clone();
                next.always_on_top = !iface.always_on_top;
                self.set_interface(next);
            }
            // The design of the settings, hub and extra windows: Minimal OLED or Sci-Fi.
            use crate::oled_minimal_ui::UiDesignParadigm;
            let other = if iface.ui_paradigm == UiDesignParadigm::ModernOledMinimal { UiDesignParadigm::ClassicSciFiHud } else { UiDesignParadigm::ModernOledMinimal };
            if item(ui, &format!("WINDOW DESIGN: {}", other.label().to_uppercase())) {
                let mut next = iface.clone();
                next.ui_paradigm = other;
                self.set_interface(next);
            }
            let ghosts = crate::workspace_canvas::ghost_count();
            if ghosts > 0 && item(ui, &format!("UN-GHOST ALL ({ghosts})")) {
                crate::workspace_canvas::unghost_all();
            }
            ui.separator();
            if item(ui, "ALL SETTINGS — SEARCH") {
                settings_panel::open(!settings_panel::is_open());
            }
            if item(ui, "SETTINGS — VOICE, DEVICES, MEMORY") {
                self.drawer_open = !self.drawer_open;
            }
            if item(ui, "INTERFACE — THEME, ZOOM") {
                self.settings_open = true;
            }
            if item(ui, "INTEGRATIONS HUB") {
                self.hub.open = !self.hub.open;
            }
            ui.separator();
            if item(ui, "— MINIMISE") {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            if item(ui, "HIDE TO TRAY") {
                hide();
            }
            if item(ui, "SHUT DOWN J.A.I.") {
                self.confirm_shutdown = true;
            }
        });
        if let Some(mode) = chosen {
            self.switch_view(mode, true);
        }
    }

    /// Switches the HUD to the preset `name`: board, blocks, emblem and core cross-fade from the
    /// next frame on (`theme_engine::follow`), the settings windows take its accent, and a toast
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
        if let Some(p) = crate::theme_engine::preset(name) {
            next.accent = format!("#{:02x}{:02x}{:02x}", p.core_primary[0], p.core_primary[1], p.core_primary[2]);
        }
        self.set_interface(next);
        self.app.notice(format!("THEME // {} ACTIVE", crate::theme_engine::label(name)));
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

    /// Bottom left, as on the reference board: the red-to-white double-line level meter,
    /// `LVL −66.5 dB  PK −60.7 dB`, the outlined `[LIVE]` / `[MUTE]` badge (click: microphone
    /// on / off; its tooltip carries the VRAM guard, the listening mode and the state) and the
    /// last thing Sir said, inline. A silent command's badge or a signal shows after it.
    fn telemetry_strip(&mut self, ui: &mut Ui, m: &HudModel, screen: Rect, dt: f32) {
        let painter = ui.painter_at(screen);
        let y = screen.bottom() - 10.0 - TELEMETRY_H;
        let mid = y + TELEMETRY_H / 2.0;
        let mut x = screen.left() + 14.0;
        let level = m.level.as_ref();
        let muted = level.is_none_or(|l| l.phase == "muted") || m.cfg.as_ref().is_some_and(|c| !c.mic_enabled);
        let lvl = level.map(|l| l.rms_db).unwrap_or(-90.0).max(-90.0);
        self.peak_db = if lvl > self.peak_db { lvl } else { (self.peak_db - 9.0 * dt).max(lvl) };
        // The meter: two hairlines, lit from the left in crimson running to white at the level.
        let fill = if muted { 0.0 } else { db_fraction(lvl) };
        for (k, dy) in [-1.5f32, 1.5].into_iter().enumerate() {
            let (from, to) = (pos2(x, mid + dy), pos2(x + METER_W, mid + dy));
            painter.line_segment([from, to], stroke(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18)));
            let mut mesh = egui::Mesh::default();
            meter_ribbon(&mut mesh, from, METER_W * fill, if k == 0 { 1.6 } else { 1.2 });
            painter.add(Shape::mesh(mesh));
        }
        let peak_x = x + METER_W * db_fraction(self.peak_db);
        painter.line_segment([pos2(peak_x, mid - 4.0), pos2(peak_x, mid + 4.0)], stroke(1.0, Color32::WHITE.gamma_multiply(0.8)));
        x += METER_W + 12.0;
        let text = |painter: &egui::Painter, x: &mut f32, s: String, colour: Color32| {
            let r = painter.text(pos2(*x, mid), Align2::LEFT_CENTER, s, strip_font(), colour);
            *x = r.right() + 14.0;
        };
        text(&painter, &mut x, format!("LVL {lvl:.1} dB"), STRIP_INK);
        text(&painter, &mut x, format!("PK {:.1} dB", self.peak_db), STRIP_INK);
        // `[LIVE]` / `[MUTE]` / `[SLEEP]`, outlined in white.
        let sleeping = m.cfg.as_ref().is_some_and(|c| c.sleeping);
        let (badge, ink) = strip_badge(muted, sleeping);
        let galley = painter.layout_no_wrap(badge.to_string(), strip_font(), ink);
        let rect = Rect::from_min_size(pos2(x, y), vec2(galley.size().x + 12.0, TELEMETRY_H));
        painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, Color32::WHITE.gamma_multiply(0.85)));
        painter.galley(rect.center() - galley.size() / 2.0, galley, ink);
        let (vram, _, _) = crate::vram_guard::badge();
        let vram = vram;
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
        x = rect.right() + 14.0;
        // `[CALL · LISTENING]` / `[CALL · TALKING]` while JAI is in Sir's Discord call.
        if let Some(call) = crate::discord_bridge::status_label() {
            let ink = crate::hud_board::active_colour();
            let galley = painter.layout_no_wrap(call.to_string(), strip_font(), ink);
            let rect = Rect::from_min_size(pos2(x, y), vec2(galley.size().x + 12.0, TELEMETRY_H));
            painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, ink));
            painter.galley(rect.center() - galley.size() / 2.0, galley, ink);
            ui.interact(rect, ui.id().with("hud_call"), Sense::hover()).on_hover_text("JAI hears the others in your Discord call (not your microphone). Say \"stop listening to the call\" to end it.");
            x = rect.right() + 14.0;
        }
        // `F1 MUTE · F11 FULLSCREEN · ESC INTERRUPT` on the right, as on the reference board
        // (each also a button), left of the docked emblem's corner; only where it fits.
        let hints = key_hints(&painter, key_hints_right(screen, self.console_reserve()), mid);
        let hints_left = hints.first().map_or(f32::INFINITY, |(_, r)| r.left());
        let room_right = if hints_left > x + 160.0 { hints_left - 24.0 } else { screen.right() - crate::hud_dock::DOCK_INSET * 2.0 - 40.0 };
        // What he said last, inline (fading out with the caption timer).
        if let Some(line) = m.messages.iter().rev().find(|l| l.who == Speaker::Sir && !l.faded) {
            let room = (room_right - x).max(0.0);
            let heard = heard_line(&line.text, (room / 6.2) as usize);
            if !heard.is_empty() {
                text(&painter, &mut x, heard, STRIP_INK.gamma_multiply(0.75));
            }
        }
        if let Some((path, alert, until)) = &m.signal {
            if *until > Instant::now() {
                text(&painter, &mut x, format!("SIGNAL: {path}"), if *alert { self.palette.red } else { crate::hud_board::active_colour() });
            }
        }
        if let Some((label, ok, until)) = &m.quick {
            if *until > Instant::now() {
                text(&painter, &mut x, label.to_uppercase(), if *ok { STARK_GOLD } else { self.palette.red });
            }
        }
        // The key hints, where the line left them room.
        if x + 10.0 > hints_left {
            return;
        }
        for (n, (label, rect)) in hints.iter().enumerate() {
            let response = ui.interact(rect.expand(3.0), ui.id().with(("hud_key_hint", n)), Sense::click());
            let ink = if response.hovered() { crate::hud_board::active_colour() } else { STRIP_MUTED };
            painter.text(rect.left_center(), Align2::LEFT_CENTER, *label, strip_font(), ink);
            if n > 0 {
                painter.text(pos2(rect.left() - 9.0, mid), Align2::CENTER_CENTER, "·", strip_font(), STRIP_MUTED);
            }
            let response = response.on_hover_text(["Microphone on / off (F1, Ctrl+M)", "Fullscreen ↔ maximised window (F11)", "Stop JAI speaking (Esc)"][n]);
            if response.clicked() {
                match n {
                    0 => self.toggle_mic(),
                    1 => self.toggle_fullscreen(),
                    _ => self.command(HudCommand::Interrupt),
                }
            }
        }
    }

    /// The status pill, top centre as on the reference board: `● LISTENING` (green), `THINKING`
    /// (amber), `SPEAKING` (red), `MUTED` or `STANDBY` (grey). A new word cross-fades in within
    /// 33 ms; on a screen whose header row reaches the centre it sits under the row.
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
        let tip = match word {
            "LISTENING" => "Listening: say \"JAI\" and your command (F1 mutes the microphone).",
            "THINKING" => "Thinking: working on your request.",
            "SPEAKING" => "Speaking (Esc interrupts).",
            "MUTED" => "The microphone is off (F1 or Ctrl+M turns it on).",
            _ => "Standby: say \"JAI\" or click the emblem to wake him.",
        };
        ui.interact(rect, ui.id().with("hud_status_pill"), Sense::hover()).on_hover_text(tip);
    }

    /// The card under the emblem after JAI looked at the screen (`screen_context`), as in the
    /// reference video: `● VIDEO DETECTED`, what it was, and its quick actions (a click asks
    /// him); it leaves by itself after a few seconds (the pointer on it keeps it).
    fn screen_card(&mut self, ui: &mut Ui, screen: Rect, frame: &crate::rose_emblem_renderer::EmblemFrame) {
        let rect = screen_card_rect(screen, frame);
        // (The pointer on it holds it, fully shown.)
        let Some((card, age)) = crate::screen_context::card(ui.rect_contains_pointer(rect)) else { return };
        ui.ctx().request_repaint_after(Duration::from_millis(40));
        let alpha = crate::screen_context::card_alpha(age);
        let painter = ui.painter_at(screen);
        let accent = crate::hud_board::active_colour().gamma_multiply(alpha);
        painter.rect_filled(rect, Rounding::same(3.0), rgba([3, 7, 12], 0.92 * alpha));
        painter.rect_stroke(rect, Rounding::same(3.0), stroke(1.0, crate::hud_board::border_colour().gamma_multiply(alpha)));
        let amber = Color32::from_rgb(0xFF, 0xA5, 0x30).gamma_multiply(alpha);
        let left = rect.left() + 12.0;
        painter.circle_filled(pos2(left + 3.0, rect.top() + 14.0), 3.0, amber);
        painter.text(pos2(left + 12.0, rect.top() + 14.0), Align2::LEFT_CENTER, format!("{} DETECTED", card.kind), strip_font(), amber);
        // (A long window title is shortened to the card's width.)
        let font = FontId::proportional(12.0);
        let line = fit_width(&painter, &card.line, &font, rect.width() - 24.0);
        painter.text(pos2(left, rect.top() + 33.0), Align2::LEFT_CENTER, line, font, STRIP_INK.gamma_multiply(alpha));
        let mut x = left;
        for (n, (word, ask)) in card.actions.iter().enumerate() {
            let galley = painter.layout_no_wrap(word.to_string(), strip_font(), accent);
            let key = Rect::from_min_size(pos2(x - 3.0, rect.bottom() - 21.0), galley.size() + vec2(6.0, 6.0));
            let response = ui.interact(key, ui.id().with(("hud_screen_card", n)), Sense::click()).on_hover_text(*ask);
            if response.hovered() {
                painter.rect_filled(key, Rounding::same(2.0), accent.gamma_multiply(0.14));
            }
            painter.galley(key.min + vec2(3.0, 3.0), galley, accent);
            x = key.right() + 16.0;
            if response.clicked() {
                crate::screen_context::dismiss();
                self.command(HudCommand::Chat { text: (*ask).to_string() });
            }
        }
    }

    /// While Sir speaks, his voice shows as a ring of radial ticks round the emblem, as on the
    /// reference orb (only at home, not docked; the emblem's own rings follow JAI's voice).
    fn voice_rim(&mut self, painter: &egui::Painter, m: &HudModel, frame: &crate::rose_emblem_renderer::EmblemFrame, dt: f32) {
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
        if self.voice_rim < 0.01 {
            self.voice_rim = 0.0;
            return;
        }
        painter.ctx().request_repaint();
        let clock = painter.ctx().input(|i| i.time) as f32;
        let colour = if verdict.is_some() { crate::hud_style::warn_colour() } else { crate::hud_board::active_colour() };
        for (from, to, alpha) in rim_ticks(frame.centre, frame.radius, self.voice_rim, clock) {
            painter.line_segment([from, to], stroke(1.4, colour.gamma_multiply(alpha)));
        }
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
        // Like every block, it moves with the mouse.
        let drag = ui.interact(rect, egui::Id::new("hud_comms_drag"), Sense::drag());
        if drag.dragged() {
            self.comms_offset += drag.drag_delta();
        }
        let mut painter = ui.painter_at(screen);
        painter.set_opacity(fade);
        crate::hud_board::paint_dialogue(&painter, rect, &block);
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
            Frame::none().fill(crate::hud_board::BLOCK_FILL).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO).inner_margin(Margin::symmetric(10.0, 6.0)).show(ui, |ui| {
                ui.set_width(FIND_BAR_WIDTH - 20.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("FIND IN BLOCKS").font(strip_font()).color(crate::hud_board::active_colour()));
                    let input = ui.add(egui::TextEdit::singleline(&mut self.find_text).desired_width(220.0).font(FontId::proportional(14.0)).hint_text("voltage, deadline …"));
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
                    ui.label(RichText::new(tally).font(strip_font()).color(STRIP_MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let key = egui::Button::new(RichText::new("[ X ]").font(strip_font()).color(STRIP_INK)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO);
                        close = ui.add(key).on_hover_text("End the search (Esc)").clicked();
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
        if crate::hud_dock::is_detached(CONSOLE_ID) {
            // (Closed while it is a window of its own: it plays its exit, then goes.)
            if !detached_keep(ctx, ViewportId::from_hash_of("jai_text_console"), self.console_open) {
                return;
            }
            self.console_at_edge = false;
            let mut builder = egui::ViewportBuilder::default().with_title("Text Console — J.A.I.").with_app_id("jai").with_inner_size([CONSOLE_WIDTH, 640.0]).with_min_inner_size([320.0, 260.0]).with_decorations(false);
            if let Some((at, size)) = detached_placement(ctx, CONSOLE_ID) {
                builder = builder.with_position(at).with_inner_size(size);
            }
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
                egui::CentralPanel::default().frame(Frame::none().fill(BLOCK_SLATE).inner_margin(Margin::same(12.0))).show(ctx, |ui| {
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
            let frame = Frame::none().fill(crate::hud_board::BLOCK_FILL).rounding(Rounding::ZERO).inner_margin(Margin::same(12.0));
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
                inner_border(&painter, rect, 1.0, crate::hud_board::border_colour().gamma_multiply(slide));
                crate::hud_board::brackets(&painter, rect.shrink(0.75), crate::hud_board::bracket_colour().gamma_multiply(slide));
                crate::hud_dock::publish_block(CONSOLE_ID, "Text console", rect);
                publish_console_rect(Some(rect));
            }
        }
        if act.close {
            self.console_open = false;
            crate::hud_dock::dock(CONSOLE_ID);
        }
        if act.pin {
            self.console_pinned = !self.console_pinned;
        }
        if act.detach {
            if crate::hud_dock::is_detached(CONSOLE_ID) {
                crate::hud_dock::dock(CONSOLE_ID);
            } else {
                crate::hud_dock::detach(CONSOLE_ID);
            }
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
        ui.horizontal(|ui| {
            let title = ui.add(egui::Label::new(RichText::new("TEXT // CONSOLE").font(strip_font()).color(crate::hud_board::active_colour())).sense(Sense::drag()));
            if detached && title.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let pinned = self.console_pinned;
                let chip = |ui: &mut Ui, text: &str, on: bool, hint: &str| {
                    let ink = if on { crate::hud_board::active_colour() } else { STRIP_INK };
                    ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(ink)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO)).on_hover_text(hint).clicked()
                };
                act.close |= chip(ui, "[ CLOSE ]", false, "Close (Esc)");
                act.detach |= chip(ui, if detached { "[ DOCK ]" } else { "[ DETACH ]" }, false, "Its own window / back inside the HUD");
                act.pin |= chip(ui, "[ PIN ]", pinned, "Keep it in front (Esc leaves it open)");
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let segment = |ui: &mut Ui, text: &str, on: bool| {
                let ink = if on { Color32::BLACK } else { STRIP_INK };
                let fill = if on { crate::hud_board::active_colour() } else { rgba([3, 7, 12], 0.9) };
                ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(ink)).fill(fill).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO)).clicked()
            };
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
                ui.label(RichText::new("+ DROP A FILE ON THE HUD · NO FILE LOADED").font(strip_font()).color(STRIP_MUTED));
            }
        }
        ui.add_space(8.0);
        let log_height = (ui.available_height() - self.console_foot).max(60.0);
        egui::ScrollArea::vertical().max_height(log_height).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            for line in m.messages.iter().filter(|l| !l.text.trim().is_empty()) {
                let (who, colour) = match line.who {
                    Speaker::Sir => ("USER", STRIP_MUTED),
                    Speaker::Jai => ("J.A.I.", crate::hud_board::active_colour()),
                    Speaker::System => ("SYSTEM", STRIP_MUTED),
                };
                ui.label(RichText::new(format!("[{who} // {}]", line.at)).font(strip_font()).color(colour));
                let ink = if line.faded { STRIP_MUTED } else { STRIP_INK };
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
                    sharp_block(ui, "SYSTEM POWER // CONFIRMATION", "PENDING", |ui| this.power_card(ui, &p, card));
                }
                for card in &cards {
                    sharp_block(ui, "WHATSAPP // MESSAGE CARD", "PENDING", |ui| this.message_card(ui, &p, card));
                }
                if let Some(clip) = &clip {
                    sharp_block(ui, "INSTANT REPLAY // CLIP", &format!("{} S", clip.seconds), |ui| clip_card(ui, &p, clip));
                }
                for spec in &widgets {
                    let label = format!("WIDGET // {}", spec.title.to_uppercase());
                    sharp_block(ui, &label, short_type_upper(spec), |ui| widget_view::widget_bare(ui, &p, spec, &this.widget_host));
                }
            });
        };
        // As the desktop's background the column stands in a window of its own, where it would
        // stand on the board: as tall as its blocks, Sir's to move (a drag on its ground).
        if pieces_in_windows(ctx) {
            const MARGIN: f32 = 10.0;
            let place = Rect::from_min_size(board_to_screen(ctx, pos2(screen.left() + 24.0, top)), vec2(SYSTEM_COLUMN, 220.0)).expand(MARGIN);
            piece_window(ctx, ViewportId::from_hash_of("jai_system_column"), "System \u{2014} J.A.I.", place, |ui| {
                if ui.interact(ui.max_rect(), ui.id().with("system_column_ground"), Sense::drag()).drag_started() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }
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
        floating_block(ctx, "hud_history", "History", pos2(screen.right() - 24.0 - size.x, screen.top() + 12.0 + HEADER_H + 14.0), vec2(size.x, size.y + 120.0), |ui| {
            {
                ui.set_width(size.x);
                // (An area hands on only the height it had before: `column_scroll`.)
                ui.set_max_height(size.y + 120.0);
                sharp_block(ui, "HISTORY // CONVERSATION & REPORTS", "[ CLOSE ]", |ui| {
                    if ui.add(egui::Button::new(RichText::new("[ CLOSE ]").font(strip_font()).color(STRIP_MUTED)).frame(false)).clicked() {
                        close = true;
                    }
                    egui::ScrollArea::vertical().max_height(size.y).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                        if !self.history_reports.is_empty() {
                            ui.label(RichText::new("REPORTS").font(strip_font()).color(crate::hud_board::active_colour()));
                            for (id, topic, when, blocks, sources) in &self.history_reports {
                                let text = format!("{}  ·  {blocks} BLOCKS · {sources} SOURCES  ·  {}", topic.to_uppercase(), when.get(..16).unwrap_or(when));
                                if ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(STRIP_INK)).frame(false)).on_hover_text("Put this report on the board").clicked() {
                                    present = Some(*id);
                                }
                            }
                            ui.add_space(8.0);
                        }
                        ui.label(RichText::new("CONVERSATION").font(strip_font()).color(crate::hud_board::active_colour()));
                        for line in m.messages.iter().filter(|l| !l.text.trim().is_empty()) {
                            let (who, colour) = match line.who {
                                Speaker::Sir => ("YOU //", STRIP_MUTED),
                                Speaker::Jai => ("J.A.I. //", crate::hud_board::active_colour()),
                                Speaker::System => ("SYSTEM //", STRIP_MUTED),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(who).font(strip_font()).color(colour));
                                let ink = if line.faded { STRIP_MUTED } else { STRIP_INK };
                                ui.add(egui::Label::new(RichText::new(&line.text).size(13.0).color(ink)).wrap());
                                if line.who == Speaker::Jai && ui.add(egui::Button::new(RichText::new("[ DETACH ]").font(strip_font()).color(STRIP_MUTED)).frame(false)).on_hover_text("Open in its own window").clicked() {
                                    open_window = Some(line.text.clone());
                                }
                            });
                            ui.add_space(3.0);
                        }
                    });
                });
            }
        });
        if close {
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
        floating_block(ctx, "hud_camera", "Camera", pos2(screen.center().x - 200.0, screen.top() + 12.0 + HEADER_H + 14.0), vec2(400.0, 330.0), |ui| {
            {
                ui.set_width(400.0);
                sharp_block(ui, &format!("CAMERA // {name}"), &status, |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 225.0), Sense::hover());
                    match &self.camera_texture {
                        Some((_, t)) => {
                            ui.painter().image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        None => {
                            ui.painter().rect_filled(rect, Rounding::ZERO, Color32::from_rgb(6, 10, 15));
                            ui.painter().text(rect.center(), Align2::CENTER_CENTER, if status == "STARTING" { "CONNECTING…" } else { status.as_str() }, strip_font(), STRIP_MUTED);
                        }
                    }
                    if ui.add(egui::Button::new(RichText::new("[ CAMERA OFF ]").font(strip_font()).color(STRIP_INK)).frame(false)).clicked() {
                        close = true;
                    }
                });
            }
        });
        if close {
            self.camera_open = false;
            crate::camera_feed::stop();
            self.camera_texture = None;
        }
    }

    /// `[ SCREENS ]`: the monitors, sending the HUD to one of them, and one board across all.
    fn screens_block(&mut self, ctx: &egui::Context) {
        let screen = home_screen(ctx);
        let monitors = crate::monitors::list();
        let iface = self.app.config().read(|c| c.interface.clone());
        // The monitor the HUD is on (none of them alone while it spans them).
        let ppp = ctx.pixels_per_point();
        let here = ctx.input(|i| i.viewport().inner_rect).filter(|_| !iface.span_all_monitors).and_then(|r| crate::monitors::monitor_at(&monitors, (r.center().x * ppp) as i32, (r.center().y * ppp) as i32));
        let width = SCREENS_WIDTH.min(screen.width() - 32.0);
        let mut picked = ScreensPick::default();
        // Always at the home screen's top right, also after the HUD changed monitors.
        egui::Area::new(egui::Id::new("hud_screens")).fixed_pos(pos2(screen.right() - 24.0 - width, screen.top() + 12.0 + HEADER_H + 14.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.set_width(width);
            picked = screens_menu(ui, &monitors, iface.span_all_monitors, here);
        });
        if let Some(n) = picked.send {
            // The fullscreen board (also one that spans the monitors) goes there at once; a
            // plain window is moved as any window is. No language model either way.
            if !send_board_to_monitor(self.app.config(), n) {
                self.run_calls(vec![("move_window_to_monitor", serde_json::json!({ "app_or_window_name": "jai", "target_monitor": n }))]);
            }
        }
        if let Some(on) = picked.span.filter(|on| *on != iface.span_all_monitors) {
            let mut next = iface.clone();
            next.span_all_monitors = on;
            next.fullscreen = !on;
            self.set_interface(next);
        }
        if picked.close {
            self.screens_open = false;
        }
    }

    /// Settings → Look: the essentials, and the full Interface window.
    fn look_card(&mut self, ui: &mut Ui, iface: &InterfaceSettings) {
        let p = self.palette;
        card(ui, &p, "LOOK", |_| {}, |ui| {
            let mut s = iface.clone();
            ui.checkbox(&mut s.fullscreen, "Fullscreen HUD board");
            ui.checkbox(&mut s.animations, "Animations");
            ui.checkbox(&mut s.always_on_top, "Always on top");
            ui.add_space(4.0);
            if ui.button("Theme, colours, zoom and panels…").clicked() {
                self.settings_open = true;
            }
            muted(ui, &p, "Or just ask: “JAI, go fullscreen”, “orb mode”, “clear the board”.");
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
        use crate::hud_dock::{is_detached, HudViewMode as V};
        info!("[HUD] Navigation: {}{}", mode.id(), if toggle { " (click)" } else { "" });
        let tools = self.app.tools().clone();
        let lab = &tools.stark.simlab;
        let engine = &tools.widgets;
        let in_app_sims: Vec<String> = lab.list().into_iter().map(|s| s.id).filter(|id| !is_detached(&format!("sim:{id}"))).collect();
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
                // The in-app tool blocks close; detached OS windows stay where Sir put them.
                for id in &in_app_sims {
                    lab.close(id);
                }
                for w in engine.windows().into_iter().filter(|w| !is_detached(&w.id)) {
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

    /// Dialogs, the settings and hub windows, JAI's extra windows, push-to-talk and the frame
    /// rate: everything after the HUD's own canvas.
    fn after_body(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        let iface = iface.clone();
        mark("dialogs");
        self.dialogs(ctx);
        if SETTINGS_REQUEST.swap(false, Ordering::AcqRel) {
            self.settings_open = true;
        }
        if WORKOUT_REQUEST.swap(false, Ordering::AcqRel) {
            self.workout.open = true;
        }
        if self.settings_open {
            self.settings_window(ctx, &iface);
        }
        self.hub.poll_request();
        if self.hub.open {
            self.hub.show(ctx, &self.palette, &self.app, &self.rt);
        }
        // His windows of their own that are on their way somewhere ("tidy the windows", "move
        // it to my second monitor"): one step of their glide.
        if crate::own_windows::step() {
            ctx.request_repaint();
        }
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
        if SCREENS_REQUEST.swap(false, Ordering::AcqRel) {
            self.screens_open = true;
        }
        // The Theme Studio; `[ SAVE THEME ]` saves and applies the theme.
        if let Some(t) = theme_studio::show(ctx, &iface) {
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
                    Tool::Interface => self.settings_open = true,
                    Tool::ThemeStudio => show_theme_studio(true),
                    Tool::Screens => self.screens_open = true,
                    Tool::Hub => self.hub.open = true,
                    Tool::FeatureLab => show_features(true),
                    Tool::Mixer => show_mixer(true),
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
        let own_backdrop = iface.portrait_dashboard && !(iface.desktop_background && !iface.orb_mode) && !self.as_background();
        // (As the orb or the slim strip he holds no monitor: any of them can hold it.)
        if let Some(crate::desktop_backdrop::Refused(why)) = crate::desktop_backdrop::show(ctx, own_backdrop && !windows_still(ctx), if iface.orb_mode { 0 } else { self.hwnd }, iface.span_all_monitors && !iface.orb_mode, !iface.orb_mode, true, &state) {
            lock(&self.model).toast(why, true);
            // (Off, or the refusal would come again with every frame.)
            self.app.config().update(|c| c.interface.portrait_dashboard = false);
        }
        mark("mixer");
        mixer_view::show(ctx, &self.rt);
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
        // As the desktop's background he mostly draws for nobody: at rest, or while a window
        // covers his emblem, twelve frames a second are plenty, and two beside a game. (The
        // corner orb asks for its own frames while it is out, the pointer over the desktop
        // for one with every move.)
        let fps = if self.as_background() && (!active || (crate::wallpaper::hidden() && self.corner_orb.is_home())) {
            if crate::hud_feel::game_in_front() {
                2.0
            } else {
                fps.min(12.0)
            }
        } else {
            fps
        };
        ctx.request_repaint_after(Duration::from_secs_f32(1.0 / fps));
    }
}

/// The fastest the HUD ever draws.
pub const MAX_FPS: f32 = 180.0;

/// The HUD's frame rate: with animations off 8; at rest 30; while something moves 60 — or the
/// display's own refresh rate (60 … `MAX_FPS`) when Sir is at the HUD (it has the focus or the
/// pointer), so dragging a block is as smooth as his monitor is fast.
pub fn target_fps(animations: bool, active: bool, attended: bool, display_hz: f32) -> f32 {
    match (animations, active, attended) {
        (false, ..) => 8.0,
        (true, false, _) => 30.0,
        (true, true, false) => 60.0,
        (true, true, true) => display_hz.clamp(60.0, MAX_FPS),
    }
}

/// The HUD's frame rate at rest: 30 while Sir is at the PC, 10 when nobody is looking closely.
pub fn rest_fps(unwatched: bool) -> f32 {
    if unwatched {
        10.0
    } else {
        30.0
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
        let unwatched = !crate::platform::in_test_session() && (crate::vram_guard::is_dormant() || crate::win32::idle_seconds().is_some_and(|idle| idle >= AWAY_AFTER_SECS));
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

/// A detached window's first position and size: exactly where its block sat in the HUD (screen
/// points), so popping it out never throws it onto another monitor.
pub(super) fn detached_placement(ctx: &egui::Context, id: &str) -> Option<(Pos2, Vec2)> {
    // Where each detached window first went and the step it took aside, kept while detached
    // (worked out per frame, a window dragged away would pull the other back).
    static FIRST: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (Pos2, Vec2)>>> = std::sync::LazyLock::new(Default::default);
    // ("Move the radar to my second monitor" on a HUD that covers one: where it was sent.)
    let wanted = match crate::window_commands::os_placement(id) {
        Some(rect) => rect,
        None => {
            let origin = ctx.input(|i| i.viewport().inner_rect.map(|r| r.min))?;
            let at = crate::hud_dock::block_rect(id)?.translate(origin.to_vec2());
            // A second view popped out from the spot the first left steps aside (2026-11-09: the
            // 3D model covered the pictures).
            let mut first = lock(&FIRST);
            first.retain(|k, _| crate::hud_dock::is_detached(k));
            let step = match first.get(id) {
                Some((_, step)) => *step,
                None => {
                    let mut step = Vec2::ZERO;
                    for _ in 0..8 {
                        if !first.values().any(|(p, s)| (*p + *s - (at.min + step)).length() < 12.0) {
                            break;
                        }
                        step += vec2(34.0, 30.0);
                    }
                    first.insert(id.to_string(), (at.min, step));
                    step
                }
            };
            at.translate(step)
        }
    };
    let placed = clamp_to_work_area(wanted, &crate::monitors::recent(), ctx.pixels_per_point());
    Some((placed.min, placed.size()))
}

/// `rect` (screen points) moved, and shrunk if it must be, into the work area of the monitor its
/// middle is on (else the nearest one): a window of its own never hangs off a screen's edge or
/// under the taskbar, where its border and its title row would be lost.
pub(crate) fn clamp_to_work_area(rect: Rect, monitors: &[crate::monitors::Monitor], ppp: f32) -> Rect {
    let ppp = ppp.max(0.1);
    let area = monitors
        .iter()
        .map(|m| Rect::from_min_max(pos2(m.work[0] as f32, m.work[1] as f32) / ppp, pos2(m.work[2] as f32, m.work[3] as f32) / ppp))
        .filter(|r| r.is_positive())
        .min_by(|a, b| a.distance_sq_to_pos(rect.center()).total_cmp(&b.distance_sq_to_pos(rect.center())));
    let Some(area) = area else { return rect };
    let size = rect.size().min(area.size());
    let min = pos2(rect.min.x.clamp(area.min.x, area.max.x - size.x), rect.min.y.clamp(area.min.y, area.max.y - size.y));
    Rect::from_min_size(min, size)
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
        "theme_studio" => ViewportId::from_hash_of("jai_theme_studio"),
        "discord_audio" => ViewportId::from_hash_of("jai_discord_audio"),
        "pairing" => ViewportId::from_hash_of("jai_satellite_pairing"),
        // ("Dock it back" found no 3D window: these had no entry.)
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
            matches!(a.action, Action::Restore | Action::Dock)
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
    let still = windows_still(ctx);
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
        Action::Detach => {
            if still {
                dock::detach(id);
            } else {
                dock::begin_detach(id);
            }
        }
        Action::Dock => return None,
        Action::ToMonitor => return Some(block_to_monitor(ctx, id, rect, a)),
        Action::SideBySide | Action::Swap => {
            let other = other.filter(|o| *o != id)?;
            match dock::whereabouts(other) {
                Whereabouts::Board => {}
                Whereabouts::Tray => {
                    dock::restore(other);
                    return Some(false);
                }
                Whereabouts::Window => {
                    dock::dock(other);
                    return Some(false);
                }
                Whereabouts::Closed => return None,
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

/// "Move the radar to my second monitor": on a HUD that spans that monitor the block glides over
/// to its part of the board; otherwise it becomes a window of its own in the middle of that
/// monitor (after its exit). False: not yet.
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
        return true;
    }
    // Else a window of its own there.
    let work = points(target.work);
    crate::window_commands::set_os_placement(id, crate::window_commands::fit(Rect::from_center_size(work.center(), rect.size()), work));
    if windows_still(ctx) {
        crate::hud_dock::detach(id);
    } else {
        crate::hud_dock::begin_detach(id);
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
        Action::Detach => return None,
        Action::Dock => {
            wc::note_window_fullscreen(id, false);
            dock::dock(id);
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
                        // (One on the board, one in a window: both on the board.)
                        Whereabouts::Board => {
                            dock::dock(id);
                            return Some(false);
                        }
                        Whereabouts::Closed => return None,
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
                let chip = ui
                    .add(egui::Button::new(RichText::new(format!("[ {name} ]")).font(strip_font()).color(STRIP_INK)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO))
                    .on_hover_text("Put away. Click, or say \"restore it\", to bring it back.");
                if chip.clicked() {
                    crate::hud_dock::restore(id);
                }
            }
        });
    });
}

/// Width of the bottom strip's level meter.
const METER_W: f32 = 72.0;

/// The lit part of one meter line, `len` px from `from`: dark crimson fading in at the left,
/// full crimson, white at the tip (the reference's red-to-white meter).
fn meter_ribbon(mesh: &mut egui::Mesh, from: Pos2, len: f32, width: f32) {
    if len < 0.5 {
        return;
    }
    let stops = [(0.0, Color32::from_rgba_unmultiplied(0xFF, 0x1E, 0x56, 0)), (0.45, Color32::from_rgb(0xC8, 0x14, 0x40)), (0.85, Color32::from_rgb(0xFF, 0x3A, 0x6A)), (1.0, Color32::WHITE)];
    let h = width / 2.0;
    for w in stops.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (xa, xb) = (from.x + len * a.0, from.x + len * b.0);
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(pos2(xa, from.y - h), a.1);
        mesh.colored_vertex(pos2(xb, from.y - h), b.1);
        mesh.colored_vertex(pos2(xb, from.y + h), b.1);
        mesh.colored_vertex(pos2(xa, from.y + h), a.1);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
}

/// The strip's badge and its ink: `MUTE` (microphone off) in crimson, `SLEEP`, else `LIVE`.
fn strip_badge(muted: bool, sleeping: bool) -> (&'static str, Color32) {
    if muted {
        ("MUTE", Color32::from_rgb(0xFF, 0x3A, 0x6A))
    } else if sleeping {
        ("SLEEP", STRIP_MUTED)
    } else {
        ("LIVE", Color32::WHITE)
    }
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
    let needs = |k: usize| widths[k..].iter().sum::<f32>() + 6.0 * (items.len() - k).saturating_sub(1) as f32 + if k > 0 { more_w + 6.0 } else { 0.0 };
    let first = (0..items.len()).find(|k| needs(*k) <= right - left).unwrap_or(items.len());
    let mut x = right;
    let mut placed = Vec::new();
    for (item, w) in items.iter().zip(&widths).skip(first).rev() {
        placed.push((item.id, Rect::from_min_size(pos2(x - w, y), vec2(*w, HEADER_H))));
        x -= w + 6.0;
    }
    placed.reverse();
    HeaderRow {
        placed,
        hidden: items[..first].iter().filter(|item| item.boxed).map(|item| item.id).collect(),
        more: (first > 0).then(|| Rect::from_min_size(pos2(x - more_w, y), vec2(more_w, HEADER_H))),
    }
}

/// The COMMS block's width and centre: half the screen (360-820 px), centred in what the text
/// console leaves free, and never under the docked emblem's corner (`hud_dock::DOCK_INSET` plus its
/// radius on the right).
fn comms_placement(screen: Rect, console_width: f32) -> (f32, f32) {
    let free_right = screen.right() - console_width.max(0.0);
    let free_width = (free_right - screen.left()).max(0.0);
    let corner = crate::hud_dock::DOCK_INSET + crate::rose_emblem_renderer::DOCKED_DIAMETER / 2.0 + 12.0;
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

/// Frame-time diagnostics: a frame over 120 ms is logged with the parts that took most of it
/// (at most every 5 s), and with `JAI_LOG=jai=debug` the frame rate every 2 s.
fn frame_stats(took: Duration, parts: &[(&'static str, f32)]) {
    let mut clock = lock(&FRAME_CLOCK);
    if took > Duration::from_millis(120) && clock.last_slow.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
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
    pub detach: bool,
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
    detach: bool,
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
                let chip = egui::Button::new(RichText::new(text).font(strip_font()).color(if n == 0 { crate::hud_board::active_colour() } else { STRIP_INK })).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO);
                if ui.add(chip).on_hover_text(if n == 0 { "Tab completes the line with this" } else { "Click to use this" }).clicked() {
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
            .inner_margin(Margin::symmetric(6.0, 5.0))
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
        let transmit = ui
            .add(egui::Button::new(RichText::new("[ TRANSMIT -> ]").font(strip_font()).color(crate::hud_board::active_colour())).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO))
            .clicked();
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

/// `#050A10`: the canvas of an in-app tool block.
const BLOCK_SLATE: Color32 = Color32::from_rgb(0x05, 0x0A, 0x10);
/// The same under the pointer: the block Sir is using stands a little brighter than the rest.
const BLOCK_SLATE_LIFTED: Color32 = Color32::from_rgb(0x0A, 0x12, 0x1C);

/// What a detached window shows outside its frame while the frame unfolds or folds: an OS window
/// cannot be see-through, so it is a pane darker than the slate (the frame draws itself out of
/// the dark glass and folds back into it).
const WINDOW_VOID: Color32 = Color32::from_rgb(0x02, 0x04, 0x07);

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
    if motion.moving() {
        ctx.request_repaint();
        let band = motion_band(screen, &motion);
        // Outside the frame: the dark glass. Inside it, below the scan edge: the slate.
        for void in [Rect::from_min_max(screen.min, pos2(screen.max.x, band.top())), Rect::from_min_max(pos2(screen.min.x, band.bottom()), screen.max)] {
            if void.height() > 0.0 {
                painter.rect_filled(void, Rounding::ZERO, WINDOW_VOID);
            }
        }
        let shown = motion_content_clip(screen, &motion);
        let unresolved = Rect::from_min_max(pos2(band.min.x, shown.bottom()), band.max);
        if unresolved.height() > 0.0 {
            painter.rect_filled(unresolved, Rounding::ZERO, BLOCK_SLATE);
        }
        if motion.content < 1.0 && shown.height() > 0.0 {
            painter.rect_filled(shown, Rounding::ZERO, BLOCK_SLATE.gamma_multiply(1.0 - motion.content));
        }
        paint_block_frame(&painter, screen, &motion, Color32::TRANSPARENT, crate::hud_board::border_colour(), 1.0);
    } else {
        inner_border(&painter, screen, 1.0, crate::hud_board::border_colour());
    }
    paint_window_motion(&painter, screen, &motion);
    // Esc in a view he put in full screen ("full screen the Earth") brings its window back.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape) && i.viewport().fullscreen == Some(true)) {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
    }
    reveal_when_drawn(ctx);
    keep_above_hud(ctx);
}

/// A view in a window of its own stays above the HUD it came from: Windows keeps an owned window
/// above its owner, so the fullscreen board (raised by a reply, a click on it, its watchdog)
/// never hides it (2026-11-09: a detached 3D model seemed to close; it was behind the board).
fn keep_above_hud(ctx: &egui::Context) {
    #[cfg(windows)]
    {
        let (viewport, hud) = (ctx.viewport_id(), SHARED.hwnd.load(Ordering::Acquire));
        // (As the desktop's background he owns none of them: an owned window has no key in the
        // taskbar and no place in Alt+Tab. Sir, 2026-11-14: "real windows like Discord".)
        let wanted = !crate::wallpaper::in_use();
        if viewport == ViewportId::ROOT || hud == 0 || lock(&DETACHED_MOTIONS).owned.contains(&viewport) == wanted {
            return;
        }
        if ctx.input(|i| i.viewport().title.clone()).is_some_and(|title| crate::win32::own_window_titled(&title, if wanted { hud } else { 0 })) {
            let mut motions = lock(&DETACHED_MOTIONS);
            if wanted {
                motions.owned.insert(viewport);
            } else {
                motions.owned.remove(&viewport);
            }
        }
    }
}

/// A piece of the board (a block of a report, the column of his system blocks) in a window of
/// its own, while he is the desktop's background: no frame of Windows', none of his bars (the
/// piece wears its own header), his icon, a key in the taskbar and a place in Alt+Tab. It
/// comes up at `place` (screen points) and is Sir's to move, to size and to put in front of
/// other programs from then on; `body` draws it and may start a drag of the window
/// (`ViewportCommand::StartDrag`). Never where the HUD has no windows to give
/// (`Context::embed_viewports`). (Sir, 2026-11-14: "all windows that Jay opens including the
/// briefing should be real windows like Discord", "not only in-app windows".)
pub(crate) fn piece_window(ctx: &egui::Context, viewport: ViewportId, title: &str, place: Rect, body: impl FnOnce(&mut Ui)) {
    detached_keep(ctx, viewport, true);
    let builder = egui::ViewportBuilder::default().with_title(title).with_app_id("jai").with_decorations(false).with_position(place.min).with_inner_size(place.size()).with_min_inner_size([200.0, 110.0]);
    let mut body = Some(body);
    ctx.show_viewport_immediate(viewport, for_test_session(builder), |ctx, _class| {
        let Some(body) = body.take() else { return };
        egui::CentralPanel::default().frame(Frame::none().fill(OBSIDIAN)).show(ctx, body);
        HudApp::resize_grips(ctx);
        reveal_when_drawn(ctx);
    });
}

/// A block of the board's own that floats over it (the history, the camera): an area on the
/// board at `at`, or, while he is the desktop's background, a window of its own of `size`
/// that comes up there and is moved by a drag on its ground.
fn floating_block(ctx: &egui::Context, id: &str, title: &str, at: Pos2, size: Vec2, body: impl FnOnce(&mut Ui)) {
    if !pieces_in_windows(ctx) {
        egui::Area::new(egui::Id::new(id)).default_pos(at).order(egui::Order::Foreground).show(ctx, body);
        return;
    }
    const MARGIN: f32 = 10.0;
    let place = Rect::from_min_size(board_to_screen(ctx, at), size).expand(MARGIN);
    piece_window(ctx, ViewportId::from_hash_of(("jai_floating_block", id)), &format!("{title} \u{2014} J.A.I."), place, |ui| {
        if ui.interact(ui.max_rect(), ui.id().with("floating_block_ground"), Sense::drag()).drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        Frame::none().inner_margin(Margin::same(MARGIN)).show(ui, body);
    });
}

/// The piece's window is gone (its piece left the board): nothing of it counts for a next one.
pub(crate) fn piece_window_gone(viewport: ViewportId) {
    lock(&DETACHED_MOTIONS).forget(viewport);
}

/// Whether the board's pieces stand in windows of their own: while he is the desktop's
/// background, where the HUD has windows to give.
pub(crate) fn pieces_in_windows(ctx: &egui::Context) -> bool {
    crate::wallpaper::in_use() && !ctx.embed_viewports()
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
    let mut motions = lock(&DETACHED_MOTIONS);
    if motions.revealed.insert(viewport) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        info!("[HUD] A window of its own shows itself, its first frame drawn ({viewport:?})");
    }
}

/// The top row of a borderless detached window: its title (drag it to move the window) and
/// `[ X ]`; true when Sir closed it (once its exit has played).
pub(super) fn detached_title_bar(ui: &mut Ui, ctx: &egui::Context, title: &str) -> bool {
    detached_bar(ui, ctx, title, false).0
}

/// The top row of a borderless detached window: its title (drag it to move the window),
/// `[ CLOSE ]`, and with `dock` a `[ DOCK ]` key that brings the view back onto the board.
/// Returns (Sir closed it, once its exit has played; he asked it back onto the board).
fn detached_bar(ui: &mut Ui, ctx: &egui::Context, title: &str, dock: bool) -> (bool, bool) {
    let (mut clicked, mut docked) = (false, false);
    ui.horizontal(|ui| {
        let label = ui.add(egui::Label::new(RichText::new(title.to_uppercase()).font(strip_font()).color(crate::hud_board::active_colour())).sense(Sense::drag()));
        if label.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let key = |ui: &mut Ui, text: &str| ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(STRIP_INK)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO));
            clicked = key(ui, "[ CLOSE ]").clicked();
            if dock {
                docked = key(ui, "[ DOCK ]").on_hover_text("Back onto the board").clicked();
            }
        });
    });
    if clicked && windows_still(ctx) {
        return (true, false);
    }
    let viewport = ctx.viewport_id();
    let mut motions = lock(&DETACHED_MOTIONS);
    motions.opened(viewport, ctx.cumulative_pass_nr());
    if clicked && !motions.leaving.contains_key(&viewport) {
        motions.leaving.insert(viewport, (Instant::now(), true));
        crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Dismiss);
        ctx.request_repaint();
    }
    // The exit its [ CLOSE ] started has played: now it closes.
    (motions.leaving.get(&viewport).is_some_and(|(at, by_hand)| *by_hand && at.elapsed() >= crate::hud_dock::CLOSE_FADE), docked)
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
                if pass == 0 {
                    self.revealed.remove(&viewport);
                    self.owned.remove(&viewport);
                }
                now
            }
        };
        self.shown.insert(viewport, (pass, at, now));
        at
    }

    fn forget(&mut self, viewport: ViewportId) {
        self.shown.remove(&viewport);
        self.leaving.remove(&viewport);
        self.revealed.remove(&viewport);
        self.hidden.remove(&viewport);
        self.owned.remove(&viewport);
        self.looked.remove(&viewport);
    }
}

/// How often a window that has shown itself is looked at from outside.
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
        #[cfg(windows)]
        {
            let hud = SHARED.hwnd.load(Ordering::Acquire);
            let title = ctx.input(|i| i.raw.viewports.get(&viewport).and_then(|v| v.title.clone()));
            if hud != 0 && !crate::wallpaper::in_use() && title.is_some_and(|title| crate::win32::own_window_titled(&title, hud)) {
                motions.owned.insert(viewport);
            }
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
        if motions.leaving.get(&viewport).is_some_and(|(_, by_hand)| !by_hand) {
            motions.leaving.remove(&viewport);
            ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(true));
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

/// A tool view as a ROSE block inside the main HUD — the default for every view (Simulation Lab,
/// telemetry, schedule, the QR link, JAI's markdown windows): `#050A10` slate, a 1 px
/// `#2B7A9E` frame, white L-corner brackets, the title on the left and `[ DETACH ]` `[ X ]`
/// on the right. Movable and resizable on the board; its rect is published so the emblem knows
/// whether the centre is covered (`hud_dock::block_rects`).
pub(super) fn hud_block_window(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], body: impl FnOnce(&mut Ui)) -> BlockAction {
    let mut action = BlockAction::default();
    // Closing ("close the feature lab", its [ CLOSE ], the menu, Ctrl+W), minimising ("minimise
    // it") or detaching ("detach it", its [ DETACH ]): the block plays its exit, then leaves the
    // board that way.
    let fade = crate::hud_dock::closing_progress(id);
    let tray = crate::hud_dock::is_minimized(id);
    if fade.is_some_and(|t| t >= 1.0) || (tray && fade.is_some()) {
        // Off the board from this frame on (the emblem stops making room for it at once).
        crate::hud_dock::off_board(id);
        if crate::hud_dock::take_detaching(id) {
            action.detach = true;
        } else if !crate::hud_dock::take_minimizing(id, title) {
            action.close = true;
        }
        return action;
    }
    // Put away in the tray: nothing on the board until it is restored (with its entrance).
    if tray {
        return action;
    }
    // On the board (and not on its way out): a window of its own opens where it stands again,
    // not where "move it to my second monitor" once sent it.
    if fade.is_none() {
        crate::window_commands::forget_os_placement(id);
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
        // Depth: the block floats on a shadow, the one under the pointer is lifted, and the
        // emblem lights the edge that faces it. Wear on the frame once it stands.
        let using = ctx.animate_bool_with_time(area.with("using"), !still && ctx.pointer_hover_pos().is_some_and(|at| frame_rect.contains(at)), crate::hud_style::motion::FAST.as_secs_f32());
        let accent = crate::hud_board::active_colour();
        // (`with_clip_rect` only ever narrows a clip: the shell's painter gets its own.)
        let mut shell = ui.painter().clone();
        shell.set_clip_rect(frame_rect.expand(48.0));
        crate::hud_style::shadow(&shell, motion_band(frame_rect, &motion), motion.frame * dim);
        // (The window clips what its content draws to the content's own room, a margin inside
        // the frame: painted with that, the frame's border and the outer strip of its surface
        // never showed, and a block was brackets round a dark patch. The shell is painted on the
        // whole of the block's rect.)
        shell.set_clip_rect(frame_rect);
        paint_block_frame_of(&shell, frame_rect, &motion, mix(BLOCK_SLATE, BLOCK_SLATE_LIFTED, using).gamma_multiply(dim), mix(edge, accent, using * 0.6).gamma_multiply(dim), if found { 2.0 } else { 1.0 }, Some(crate::hud_style::seed_of(id)));
        if !motion.moving() {
            if let Some(orb) = crate::hud_dock::orb() {
                crate::hud_style::emblem_light(&shell, frame_rect, orb, accent, dim);
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
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), crate::hud_style::HEADER_H), Sense::hover());
        let mid = row.center().y;
        let labels = [
            ("[ CLOSE ]", false, "Close (Ctrl+W closes the block under the pointer)"),
            ("[ DETACH ]", false, "Open in its own window (say \"dock all windows\" to bring it back)"),
            ("[ PIN ]", pinned, "Pinned, it stays through \"close the windows\" and keeps its place (Ctrl+P)"),
        ];
        // (A narrow block keeps its title whole and lets its keys go, [ PIN ] first: the menu
        // and Ctrl+P still pin. The keys are quiet until the pointer is over their block.)
        let wanted = crate::hud_style::title_galley(ui.painter(), title, ts(14.0), accent, f32::INFINITY).size().x + 26.0;
        let fit = crate::hud_style::keys_that_fit(ui.painter(), &labels, row.width(), wanted);
        let (key, keys_left) = crate::hud_style::keys(ui, area.with("header"), row.right(), mid, &labels[..fit], content_alpha * (0.62 + 0.38 * using));
        match key {
            Some(0) => close_asked = true,
            // The block plays its exit first; the new window comes up after it.
            Some(1) if still => action.detach = true,
            Some(1) => {
                info!("[HUD] '{id}': its [ DETACH ] asks it into a window of its own");
                crate::hud_dock::begin_detach(id);
            }
            Some(2) => {
                crate::hud_dock::toggle_pin(id);
            }
            _ => {}
        }
        let lettered = crate::hud_style::fitted_title(ui.painter(), title, ts(14.0), accent.gamma_multiply(content_alpha), (keys_left - row.left() - 22.0).max(20.0));
        let title_rect = Rect::from_min_size(pos2(row.left(), mid - lettered.size().y * 0.5), lettered.size());
        ui.painter().galley(title_rect.min, lettered, Color32::PLACEHOLDER);
        if crate::hud_style::paint_light(ui.painter(), pos2(title_rect.right() + 10.0, mid), crate::hud_style::light(id), ctx.input(|i| i.time), content_alpha) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        // (A click-only sense: a drag that begins here still moves the block.)
        let header = ui
            .interact(Rect::from_min_max(row.min, pos2(keys_left - 4.0, row.bottom())), area.with("title"), Sense::click())
            .on_hover_text("Drag to move, drag an edge to resize, double-click for large and back. Right-click: copy, pin, maximize, export, close. Esc or Ctrl+W close, Ctrl+G maximize, Ctrl+P pin, Ctrl+E export.");
        if header.double_clicked() {
            header_maximize = true;
        }
        // (The title row takes the right click itself: the block's menu hangs on it too, or a
        // right click on the title opened nothing.)
        header.context_menu(|ui| header_menu = crate::block_actions::menu(ui, pinned, maximized));
        if crate::hud_dock::take_esc_close(id) {
            close_asked = true;
        }
        let r = ui.cursor();
        ui.painter().line_segment([pos2(r.left(), r.top()), pos2(r.left() + ui.available_width(), r.top())], stroke(1.0, crate::hud_board::border_colour()));
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
        paint_window_motion(&painter, rect, &motion);
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
                // Let go while it moved: it is thrown, and glides on a little.
                if released && !still && velocity.length() >= crate::hud_style::THROW_FROM {
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
                match crate::hud_style::glide_on(at, velocity, bounds, ctx.input(|i| i.stable_dt)) {
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
        } else if let Some(natural) = natural.filter(|_| held.is_none() && !still && !full && !motion.moving()) {
            crate::hud_dock::fit_to_content(id, natural, bounds);
        }
        // Ctrl+W / Ctrl+M / Ctrl+P on the block under the pointer; right-click: the block menu.
        let mut chosen = inner.response.contains_pointer().then(|| block_shortcut(ctx)).flatten().or(header_menu);
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
    // Closed by hand (the chip, the menu, Ctrl+W): the block leaves with its cue, after its exit
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

/// Blocks Sir threw: where each is and how fast it still glides (`hud_style::glide_on`).
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
/// The OLED palette: the motion keeps its lines and drops its soft glows (true black stays black).
static OLED_MOTION: AtomicBool = AtomicBool::new(false);

/// Whether windows and cards play their motion (false with the animations off).
pub(crate) fn motion_enabled() -> bool {
    !REDUCED_MOTION.load(Ordering::Relaxed)
}

/// Draws this context's windows at rest (`true`) or with their entrance and exit.
pub(crate) fn set_still(ctx: &egui::Context, still: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(STILL_WINDOWS), still));
}

/// One frame of a window's entrance (`opening` 0 … 1 of `hud_dock::OPEN_ANIM`, 400 ms) or exit
/// (`closing` 0 … 1 of `hud_dock::CLOSE_FADE`, 200 ms): one motion language for every block,
/// detached window, pop-up card and board block.
///
/// In: a hairline draws out from the centre (0–90 ms, ease-out expo); the frame unfolds from it
/// about its middle (90–250 ms, ease-out quint) with the corner brackets leading (80–210 ms),
/// the unfolding edges white-hot and cooling; the content resolves top to bottom behind a soft
/// scan edge (250–370 ms); one faint glow settles on the border (to 400 ms).
/// Out, quicker: the content goes (0–70 ms), the frame folds back into the hairline (40–130
/// ms, ease-in cubic) with the brackets riding it, the line contracts to a point (120–190 ms)
/// and the point flashes once (~175 ms).
///
/// Nothing is scaled: the frame and the content are clipped and faded, so text never lays out
/// again while a window comes or goes, and its neighbours never move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowMotion {
    /// The frame's height as a share of the window's, about its middle (1: whole).
    pub unfold: f32,
    /// The hairline across the middle: its width share and brightness.
    pub line: Option<(f32, f32)>,
    /// How far down the content has resolved (0 top … 1: all of it).
    pub resolve: f32,
    /// The frame's fill and border.
    pub frame: f32,
    /// The content's opacity (title row included).
    pub content: f32,
    /// The corner brackets' height as a share of the window's, about its middle (1: the corners).
    pub brackets: f32,
    pub brackets_alpha: f32,
    /// The glow settling on the border at the end of an entrance (0 … 1).
    pub glow: f32,
    /// The point's flash at the end of an exit (0 … 1).
    pub flash: f32,
}

/// The entrance's beats, in ms of `hud_dock::OPEN_ANIM`.
const IN_LINE: (f32, f32) = (0.0, 56.0);
const IN_UNFOLD: (f32, f32) = (56.0, 156.0);
const IN_BRACKETS: (f32, f32) = (50.0, 131.0);
const IN_RESOLVE: (f32, f32) = (156.0, 231.0);
const IN_SETTLE: (f32, f32) = (156.0, 250.0);
/// The exit's beats, in ms of `hud_dock::CLOSE_FADE`.
const OUT_CONTENT: (f32, f32) = (0.0, 70.0);
const OUT_FOLD: (f32, f32) = (40.0, 130.0);
const OUT_BRACKETS: (f32, f32) = (30.0, 120.0);
const OUT_CONTRACT: (f32, f32) = (120.0, 190.0);
const OUT_FLASH: f32 = 175.0;

/// 0 before `beat`, 1 after it, linear between.
fn beat(ms: f32, (from, to): (f32, f32)) -> f32 {
    ((ms - from) / (to - from)).clamp(0.0, 1.0)
}

fn ease_out_expo(t: f32) -> f32 {
    if t >= 1.0 { 1.0 } else { 1.0 - 2f32.powf(-10.0 * t.max(0.0)) }
}

fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(5)
}

fn ease_in_cubic(t: f32) -> f32 {
    t.clamp(0.0, 1.0).powi(3)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

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
        let unfold = ease_out_quint(beat(ms, IN_UNFOLD));
        // The line is the frame's edges until they part; it fades as they do.
        let line_alpha = 1.0 - beat(ms, (IN_UNFOLD.0, IN_UNFOLD.0 + 50.0));
        Self {
            unfold,
            line: (line_alpha > 0.0).then(|| (ease_out_expo(beat(ms, IN_LINE)), line_alpha)),
            resolve: ease_in_out_cubic(beat(ms, IN_RESOLVE)),
            frame: smooth(IN_UNFOLD.0 - 10.0, IN_UNFOLD.0 + 30.0, ms),
            content: 1.0,
            brackets: ease_out_quint(beat(ms, IN_BRACKETS)),
            brackets_alpha: smooth(IN_BRACKETS.0, IN_BRACKETS.0 + 40.0, ms),
            glow: smooth(IN_SETTLE.0 - 40.0, IN_SETTLE.0, ms) * (1.0 - smooth(IN_SETTLE.0, IN_SETTLE.1, ms)),
            flash: 0.0,
        }
    }

    fn exit(t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let ms = t * crate::hud_dock::CLOSE_FADE.as_secs_f32() * 1000.0;
        let unfold = 1.0 - ease_in_cubic(beat(ms, OUT_FOLD));
        let width = 1.0 - ease_in_cubic(beat(ms, OUT_CONTRACT));
        // The line shows once the frame has folded into it, until it is a point.
        let line_alpha = smooth(OUT_FOLD.1 - 30.0, OUT_FOLD.1, ms);
        Self {
            unfold,
            line: (line_alpha > 0.0 && width > 0.0).then_some((width, line_alpha)),
            resolve: 1.0,
            frame: 1.0 - smooth(OUT_FOLD.1 - 20.0, OUT_FOLD.1 + 10.0, ms),
            content: 1.0 - smooth(OUT_CONTENT.0, OUT_CONTENT.1, ms),
            brackets: 1.0 - ease_in_cubic(beat(ms, OUT_BRACKETS)),
            brackets_alpha: 1.0 - smooth(OUT_BRACKETS.0 + 30.0, OUT_BRACKETS.1 + 10.0, ms),
            glow: 0.0,
            flash: (1.0 - (ms - OUT_FLASH).abs() / 25.0).max(0.0),
        }
    }

    /// Anything of the entrance or exit still to draw.
    pub fn moving(&self) -> bool {
        *self != Self::REST
    }
}

/// Where a window's frame stands in this frame of its motion: its rect folded about its middle.
pub(crate) fn motion_band(rect: Rect, m: &WindowMotion) -> Rect {
    if m.unfold >= 1.0 {
        return rect;
    }
    Rect::from_center_size(rect.center(), vec2(rect.width(), rect.height() * m.unfold.max(0.0)))
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

/// A frame's border drawn inside `rect`: `width` points rounded to whole physical pixels (at
/// least one), on the pixel grid. Nothing of it lies outside the rect, so a window flush with
/// its clip or the screen's edge keeps all four sides at any DPI (a stroke is centred on its
/// line: half of it was cut off there).
pub(crate) fn inner_border(painter: &egui::Painter, rect: Rect, width: f32, colour: Color32) {
    if colour.a() == 0 || !rect.is_positive() {
        return;
    }
    let ppp = painter.ctx().pixels_per_point();
    let t = (width * ppp).round().max(1.0) / ppp;
    let r = painter.round_rect_to_pixels(rect);
    if r.width() <= 2.0 * t || r.height() <= 2.0 * t {
        painter.rect_filled(r, Rounding::ZERO, colour);
        return;
    }
    for side in [
        Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + t)),
        Rect::from_min_max(pos2(r.min.x, r.max.y - t), r.max),
        Rect::from_min_max(pos2(r.min.x, r.min.y + t), pos2(r.min.x + t, r.max.y - t)),
        Rect::from_min_max(pos2(r.max.x - t, r.min.y + t), r.max - vec2(0.0, t)),
    ] {
        painter.rect_filled(side, Rounding::ZERO, colour);
    }
}

/// A block's frame in this frame of its motion: the slate fill and the border, both on the band
/// the frame has unfolded to. The edges run white-hot while they travel and cool as they land;
/// `found` (the search lights the block) draws the border two pixels wide.
fn paint_block_frame(painter: &egui::Painter, rect: Rect, m: &WindowMotion, fill: Color32, edge: Color32, width: f32) {
    paint_block_frame_of(painter, rect, m, fill, edge, width, None);
}

/// The same for a block that stands: with its own wear (`seed`: the block's, so each has its
/// own and keeps it), a frame that has aged and what use leaves on its surface.
fn paint_block_frame_of(painter: &egui::Painter, rect: Rect, m: &WindowMotion, fill: Color32, edge: Color32, width: f32, seed: Option<u64>) {
    let band = motion_band(rect, m);
    if band.height() < 0.5 || m.frame <= 0.0 {
        return;
    }
    // (Glass lit from above: `hud_style::surface`.)
    crate::hud_style::surface(painter, band, fill, m.frame);
    let heat = (1.0 - m.unfold).clamp(0.0, 1.0);
    let colour = mix(edge, Color32::WHITE, heat * 0.7).gamma_multiply(m.frame.max(heat));
    match seed.filter(|_| !m.moving() && fill.a() > 0) {
        Some(seed) => {
            crate::hud_style::patina(painter, band, m.frame);
            crate::hud_style::worn_frame(painter, band, colour, width, seed, 1.0);
        }
        // (While it unfolds or folds, and for a frame without a surface: the plain line.)
        None => inner_border(painter, band, width, colour),
    }
}

/// The two short diagonals in a resizable block's bottom-right corner (inside its border).
fn paint_resize_grip(painter: &egui::Painter, rect: Rect, colour: Color32) {
    let corner = rect.right_bottom() - vec2(4.0, 4.0);
    for k in [5.0, 9.0] {
        painter.line_segment([corner - vec2(k, 0.0), corner - vec2(0.0, k)], stroke(1.0, colour));
    }
}

/// 0 below `from`, 1 above `to`, a smooth S between.
fn smooth(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// What a window's entrance or exit draws over it (the hairline, the scan edge, the settling
/// glow, the closing point's flash), and its corner brackets (at rest: where they always are,
/// inset by half their stroke so a window at the screen's edge keeps them whole). `fill` is the
/// window's own slate: the scan edge fades the freshly resolved content up out of it.
pub(super) fn paint_window_motion(painter: &egui::Painter, rect: Rect, m: &WindowMotion) {
    paint_window_motion_on(painter, rect, m, BLOCK_SLATE);
}

pub(crate) fn paint_window_motion_on(painter: &egui::Painter, rect: Rect, m: &WindowMotion, fill: Color32) {
    let accent = crate::hud_board::active_colour();
    let ppp = painter.ctx().pixels_per_point();
    // One point as whole physical pixels: the hairline's thickness at any DPI.
    let px = ppp.round().max(1.0) / ppp;
    let soft = !OLED_MOTION.load(Ordering::Relaxed);
    if m.brackets_alpha > 0.0 && m.brackets > 0.0 {
        let r = Rect::from_center_size(rect.center(), vec2(rect.width(), rect.height() * m.brackets)).shrink(0.75);
        // White-hot while they travel, the theme's own once they are home.
        let travel = (1.0 - m.brackets).abs().min(1.0);
        let colour = mix(crate::hud_board::bracket_colour(), Color32::WHITE, travel * 0.6).gamma_multiply(m.brackets_alpha);
        crate::hud_board::brackets(painter, r, colour);
    }
    if let Some((width, strength)) = m.line {
        let c = painter.round_pos_to_pixels(rect.center());
        let half = (rect.width() * 0.5 * width).max(px);
        let core = Rect::from_min_max(pos2(c.x - half, c.y), pos2(c.x + half, c.y + px));
        if soft {
            for (grow, a) in [(4.0, 0.08), (2.0, 0.18)] {
                painter.rect_filled(core.expand2(vec2(grow * 0.5, grow)), Rounding::ZERO, accent.gamma_multiply(a * strength));
            }
        }
        painter.rect_filled(core, Rounding::ZERO, mix(accent, Color32::WHITE, 0.55).gamma_multiply(strength));
    }
    if m.resolve > 0.0 && m.resolve < 1.0 {
        // The scan edge: the content above it fades up out of the slate, a soft light trails
        // it and its line is bright.
        let band = motion_band(rect, m);
        let inner = band.shrink2(vec2(1.0, 0.0));
        let y = painter.round_to_pixel(band.top() + band.height() * m.resolve);
        let fade = 26.0f32.min(y - band.top());
        if fade > 1.0 {
            let mut mesh = egui::Mesh::default();
            let above = Rect::from_min_max(pos2(inner.left(), y - fade), pos2(inner.right(), y));
            let (clear, slate) = (Color32::TRANSPARENT, fill.gamma_multiply(0.92));
            let glow = if soft { accent.gamma_multiply(0.16) } else { Color32::TRANSPARENT };
            for (colour_top, colour_bottom) in [(clear, slate), (clear, glow)] {
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
        painter.rect_filled(Rect::from_min_max(pos2(inner.left(), y - px), pos2(inner.right(), y)), Rounding::ZERO, mix(accent, Color32::WHITE, 0.4).gamma_multiply(0.85));
    }
    if m.glow > 0.0 {
        inner_border(painter, rect, 1.0, accent.gamma_multiply(0.75 * m.glow));
        if soft {
            painter.rect_stroke(rect.expand(1.5), Rounding::ZERO, stroke(2.0, accent.gamma_multiply(0.14 * m.glow)));
        }
    }
    if m.flash > 0.0 {
        let c = painter.round_pos_to_pixels(rect.center());
        if soft {
            painter.circle_filled(c, 7.0, accent.gamma_multiply(0.22 * m.flash));
            painter.rect_filled(Rect::from_center_size(c, vec2(26.0 * m.flash, px)), Rounding::ZERO, accent.gamma_multiply(0.6 * m.flash));
        }
        painter.circle_filled(c, 2.2, Color32::WHITE.gamma_multiply(0.95 * m.flash));
    }
}

/// `a` → `b` by `t`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

/// How far a block that does not answer the search steps back.
const SEARCH_DIM: f32 = 0.22;
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
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(content).size(13.5).color(STRIP_INK)).wrap());
        });
    })
}

/// A block of the board for a module outside the GUI (`body` draws its content). Returns
/// whether it was closed. Its `[ DETACH ]` makes it a window of its own with the same body
/// (drawn inside the HUD's frame, so `body` may borrow what its caller holds), and that
/// window's `[ DOCK ]` brings it back. (2026-11-13, Sir: "detaching windows doesn't work but
/// it closes them": these blocks had the key and threw the detach away.)
pub(crate) fn block(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], body: impl FnOnce(&mut Ui)) -> bool {
    let viewport = ViewportId::from_hash_of(format!("jai_block_{id}"));
    // (Where the HUD has no windows of its own to give, an offscreen session, every block
    // stays on the board.)
    if !crate::hud_dock::adopt(id) || ctx.embed_viewports() {
        crate::hud_dock::window_gone(id);
        // (On the board: nothing of an earlier window of its own counts for the next one.)
        detached_keep(ctx, viewport, true);
        let action = hud_block_window(ctx, id, title, size, body);
        if action.detach {
            crate::hud_dock::detach(id);
        }
        return action.close;
    }
    detached_keep(ctx, viewport, true);
    let name = title.split("//").next().unwrap_or(title).trim();
    let mut builder = egui::ViewportBuilder::default().with_title(format!("{name} \u{2014} J.A.I.")).with_app_id("jai").with_inner_size(size).with_min_inner_size([320.0, 180.0]).with_decorations(false);
    if let Some(icon) = window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = detached_placement(ctx, id) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = for_test_session(builder);
    // In a window of its own it is still one of his blocks: "close the windows", "close it" and
    // its name mean it too (`hud_dock::close_named`, `close_all_blocks`).
    crate::hud_dock::publish_window(id, title);
    let (mut closed, mut docked) = (crate::hud_dock::closing_progress(id).is_some(), false);
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
        egui::CentralPanel::default().frame(Frame::none().fill(BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
            // (Its own keys, on top of a close that was asked for by word.)
            let (by_key, to_board) = detached_bar(ui, ctx, title, true);
            closed |= by_key;
            docked |= to_board;
            body(ui);
        });
        paint_window_border(ctx);
    });
    if closed || docked {
        // (Docked: back on the board from the next frame. Closed: on the board when it is
        // next asked for.)
        crate::hud_dock::window_gone(id);
        crate::hud_dock::dock(id);
        lock(&DETACHED_MOTIONS).forget(viewport);
        if docked {
            info!("[HUD] '{id}' is back on the board (its window's [ DOCK ])");
        }
    }
    closed
}

/// A window of the board's own (the interface settings, the hub, a question to Sir) as one of
/// his blocks: on the board, or a window of its own (`[ DETACH ]`, and always while he is the
/// desktop's background). `open` goes false when Sir closes it. (Until 2026-11-13 these were
/// plain windows inside the board and nowhere else. Sir: "there should be none windows that
/// are exclusive to JAI's UI".)
pub(crate) fn board_window(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], open: &mut bool, body: impl FnOnce(&mut Ui)) {
    if !*open {
        crate::hud_dock::forget(id);
        return;
    }
    if block(ctx, id, title, size, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, body);
    }) {
        *open = false;
        crate::hud_dock::forget(id);
    }
}

/// The keyboard shortcut for the block under the pointer, if one was pressed this frame:
/// Ctrl+W closes it, Ctrl+G grows it to the full board (and back), Ctrl+P pins it, Ctrl+E
/// exports it as Markdown. (Ctrl+M is the microphone's.)
fn block_shortcut(ctx: &egui::Context) -> Option<crate::block_actions::BlockAction> {
    use crate::block_actions::BlockAction as Menu;
    use egui::{Key, Modifiers};
    ctx.input_mut(|i| {
        [(Key::W, Menu::Close), (Key::G, Menu::Maximize), (Key::P, Menu::Pin), (Key::E, Menu::ExportMarkdown)].into_iter().find(|(key, _)| i.consume_key(Modifiers::CTRL, *key)).map(|(_, action)| action)
    })
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
    let part = |m: &crate::monitors::Monitor| Rect::from_min_max(pos2(m.rect[0] as f32, m.rect[1] as f32) / ppp, pos2(m.rect[2] as f32, m.rect[3] as f32) / ppp).translate(-window.min.to_vec2());
    let spanning = monitors.first().is_some_and(|m| m.primary && inside.expand(2.0).contains_rect(part(m)));
    // (A monitor that merely touches the window's edge shows nothing of it.)
    let regions: Vec<Rect> = monitors.iter().map(|m| part(m).intersect(inside)).filter(|r| r.width() > 1.0 && r.height() > 1.0).collect();
    if regions.len() > 1 { (regions, spanning) } else { (Vec::new(), false) }
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
    let key = |ui: &mut Ui, text: &str, ink: Color32, hint: &str, mark: &dyn Fn() -> String| {
        let response = ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(ink)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO)).on_hover_text(hint);
        if crate::window_drill::enabled() {
            crate::window_drill::mark(&mark(), response.rect);
        }
        response.clicked()
    };
    let title = format!("SCREENS // {} DISPLAY{}", monitors.len(), if monitors.len() == 1 { "" } else { "S" });
    sharp_block(ui, &title, if spanning { "SPANNING ALL" } else { "ONE SCREEN" }, |ui| {
        for m in monitors {
            ui.horizontal(|ui| {
                // The button takes its place at the right first (beside both lines); the two
                // lines get what is left.
                ui.set_min_height(28.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if here == Some(m.index) {
                        ui.add(egui::Label::new(RichText::new("HUD IS HERE").font(strip_font()).color(STRIP_MUTED)).selectable(false));
                    } else if key(ui, "[ MOVE HERE ]", crate::hud_board::active_colour(), "The fullscreen board on this monitor", &|| format!("screens:move:{}", m.index)) {
                        picked.send = Some(m.index);
                    }
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let name = format!("DISPLAY {}{}", m.index, if m.primary { "  //  PRIMARY" } else { "" });
                        ui.add(egui::Label::new(RichText::new(name).font(strip_font()).color(STRIP_INK)).truncate().selectable(false));
                        let place = format!("{} x {}  AT  {}, {}", m.width(), m.height(), m.rect[0], m.rect[1]);
                        ui.add(egui::Label::new(RichText::new(place).font(strip_font()).color(STRIP_MUTED)).truncate().selectable(false));
                    });
                });
            });
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            if monitors.len() > 1 {
                if spanning {
                    if key(ui, "[ BACK TO ONE MONITOR ]", crate::hud_board::active_colour(), "The fullscreen board on your first monitor again", &|| "screens:span:off".into()) {
                        picked.span = Some(false);
                    }
                } else if key(ui, "[ SPAN ALL MONITORS ]", crate::hud_board::active_colour(), "One board across every monitor; the header, the console and the emblem stay on your first", &|| "screens:span:on".into()) {
                    picked.span = Some(true);
                }
            }
            if key(ui, "[ CLOSE ]", STRIP_MUTED, "Close (Esc)", &|| "screens:close".into()) {
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
    let out = egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, blocks);
    let (view, above) = (out.inner_rect, out.state.offset.y);
    let below = out.content_size.y - view.height() - above;
    let fade = |edge: Rect, solid_at_bottom: bool| {
        let (top, bottom) = if solid_at_bottom { (Color32::TRANSPARENT, OBSIDIAN) } else { (OBSIDIAN, Color32::TRANSPARENT) };
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

fn sharp_block<R>(ui: &mut Ui, label: &str, tag: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    // (The frame every block wears, `hud_style`: its surface goes behind the content once the
    // content has said how tall it is.)
    let back = ui.painter().add(egui::Shape::Noop);
    let shown = Frame::none().inner_margin(Margin::same(12.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        let tag = ui.painter().layout_no_wrap(tag.to_string(), strip_font(), STRIP_MUTED);
        let title = crate::hud_style::fitted_title(ui.painter(), label, ts(13.0), crate::hud_board::active_colour(), (row.width() - tag.size().x - 14.0).max(20.0));
        ui.painter().galley(pos2(row.left(), row.center().y - title.size().y * 0.5), title, Color32::PLACEHOLDER);
        ui.painter().galley(pos2(row.right() - tag.size().x, row.center().y - tag.size().y * 0.5), tag, STRIP_MUTED);
        ui.painter().line_segment([pos2(row.left(), row.bottom() + 3.0), pos2(row.right(), row.bottom() + 3.0)], stroke(1.0, crate::hud_board::border_colour()));
        ui.add_space(8.0);
        body(ui)
    });
    let rect = shown.response.rect;
    let mut frame = crate::hud_style::surface_shapes(rect, crate::hud_board::BLOCK_FILL, 1.0);
    frame.push(egui::Shape::rect_stroke(rect.shrink(0.5), Rounding::ZERO, stroke(1.0, crate::hud_board::border_colour())));
    ui.painter().set(back, egui::Shape::Vec(frame));
    crate::hud_board::brackets(ui.painter(), rect, crate::hud_board::bracket_colour());
    ui.add_space(10.0);
    shown.inner
}

/// The glow in the middle of the canvas for a theme colour: the colour itself at about a
/// twelfth, on obsidian (Arc Cyan's is the old cyan slate, his yellow's a warm umber).
pub(crate) fn vignette_for(primary: Color32) -> Color32 {
    let lift = |c: u8, floor: u8| ((c as f32 * 0.085) as u8).saturating_add(floor);
    Color32::from_rgb(lift(primary.r(), OBSIDIAN.r()), lift(primary.g(), OBSIDIAN.g()), lift(primary.b(), OBSIDIAN.b()))
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
                Color32::from_white_alpha(a)
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
    // The glass itself: uneven light, darker edges, dust and hairline scratches, per screen of
    // a HUD that spans several, else over the one it is on. (It was only drawn on a spanned
    // HUD: on a single screen there was no glass at all.)
    let screens = monitor_regions(ctx);
    if screens.is_empty() {
        crate::hud_style::glass(&painter, rect);
    }
    for screen in screens {
        crate::hud_style::glass(&painter, screen);
    }
}

/// The obsidian canvas: `#020408` with a radial vignette of the theme's colour in the centre
/// (fading to black at the edges) and the 28 px dot matrix at 6 %.
pub(crate) fn paint_canvas(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    painter.rect_filled(rect, Rounding::ZERO, OBSIDIAN);
    let mut mesh = egui::Mesh::default();
    let home = home_screen(ctx);
    let centre = home.center();
    let reach = home.size() * 0.75;
    let vignette = vignette_for(crate::hud_board::active_colour());
    let mid = blend(vignette, OBSIDIAN, 0.55);
    mesh.colored_vertex(centre, vignette);
    let n = 72;
    for k in 0..n {
        let a = TAU * k as f32 / n as f32;
        let dir = vec2(a.cos(), a.sin());
        mesh.colored_vertex(centre + dir * reach * 0.42, mid);
        mesh.colored_vertex(centre + dir * reach, OBSIDIAN);
    }
    for k in 0..n as u32 {
        let next = (k + 1) % n as u32;
        let (m0, o0, m1, o1) = (1 + 2 * k, 2 + 2 * k, 1 + 2 * next, 2 + 2 * next);
        mesh.add_triangle(0, m0, m1);
        mesh.add_triangle(m0, o0, o1);
        mesh.add_triangle(m0, o1, m1);
    }
    painter.add(Shape::mesh(mesh));
    let mut dots = egui::Mesh::default();
    let dot = Color32::from_white_alpha((255.0 * DOT_ALPHA) as u8);
    let mut y = rect.top() + DOT_SPACING / 2.0;
    while y < rect.bottom() {
        let mut x = rect.left() + DOT_SPACING / 2.0;
        while x < rect.right() {
            dots.add_colored_rect(Rect::from_min_size(pos2(x.round(), y.round()), vec2(1.0, 1.0)), dot);
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
        mark("start");
        crate::live_shots::tick(ctx);
        // `--window-drill`: its next step, from what the previous frame drew.
        if let Some(drill) = self.drill.as_mut() {
            drill.step(ctx, self.hwnd);
        }
        crate::window_drill::begin_frame();
        let now = Instant::now();
        // Clamped: the first frame after an idle second never makes an animation jump.
        let dt = crate::rose_emblem_renderer::safe_dt(now.duration_since(self.last_frame).as_secs_f32());
        if ctx.input(|i| i.pointer.any_down() || i.pointer.delta().length() > 0.5 || !i.keys_down.is_empty()) {
            self.last_activity = now;
        }

        let iface = self.sync_interface(ctx);
        // Settings → Animations off: windows come and go without their motion; the OLED palette
        // keeps the motion's lines and drops its glows.
        REDUCED_MOTION.store(!iface.animations, Ordering::Relaxed);
        OLED_MOTION.store(self.palette.oled, Ordering::Relaxed);
        // (The console publishes where it stands again if it is drawn this frame.)
        publish_console_rect(None);
        self.sync_background(ctx, &iface);
        self.enforce_fullscreen(ctx, &iface);
        self.sync_orb_window(ctx, &iface);
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
        crate::interface::publish_layout(iface.layout_state((screen.width() * iface.scale, screen.height() * iface.scale)));
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if iface.orb_mode {
            self.pure_orb(ctx, &m, &iface);
        } else {
            paint_canvas(ctx);
            // (Offscreen renders stay clean: the verifier reads their colours.)
            if iface.display_finish && !self.palette.oled && !windows_still(ctx) {
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
            // Night mode: the HUD dims (by the clock or because Sir said so).
            crate::hud_feel::set_night_by_clock(iface.night_mode);
            // (A scripted session shows the dimming from its 34th to its 46th second on request.)
            let night = (crate::hud_feel::night_active() || (demo("night") && (34.0..46.0).contains(&hud_age()))) && !windows_still(ctx);
            self.night = crate::hud_style::motion::approach(self.night, if night { 1.0 } else { 0.0 }, dt, Duration::from_millis(900));
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
            }
            self.popups(ctx, dt);
            self.room_alert_overlay(ctx, &m);
            // (Neither the fullscreen board nor the one across the monitors is resized by hand,
            // nor the desktop's background.)
            if iface.frameless && !iface.fullscreen && !iface.span_all_monitors && !maximized && !self.as_background() {
                Self::resize_grips(ctx);
            }
            // As the desktop's background: the orb that leaves it when Sir speaks to him unseen.
            self.corner_orb_frame(ctx, &m, &iface);
        }
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
        #[cfg(windows)]
        {
            // (Looked up twice a second, not in every frame.)
            static SEEN: Mutex<Option<(Instant, Vec<crate::win32_desktop::OwnWindow>)>> = Mutex::new(None);
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
                    ui.label(RichText::new("No window of mine is open, Sir.").size(13.5).color(STRIP_MUTED));
                }
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    for w in rows {
                        ui.horizontal(|ui| {
                            let name = w.title.trim_end_matches("J.A.I.").trim_end_matches([' ', '\u{2014}', '-']).to_string();
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let key = |ui: &mut Ui, text: &str, hint: &str| ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(STRIP_INK)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO)).on_hover_text(hint).clicked();
                                if key(ui, "[ CLOSE ]", "Close it") {
                                    act = Some((w.hwnd, 2, w.rect));
                                }
                                if key(ui, "[ HERE ]", "Onto the monitor you are using") {
                                    act = Some((w.hwnd, 1, w.rect));
                                }
                                if key(ui, "[ FRONT ]", "In front of the other windows") {
                                    act = Some((w.hwnd, 0, w.rect));
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new(RichText::new(name).size(13.5).color(STRIP_INK)).truncate());
                                });
                            });
                        });
                        ui.add_space(3.0);
                    }
                });
            });
            match act {
                Some((hwnd, 0, _)) => crate::win32_desktop::raise(hwnd),
                Some((hwnd, 1, [l, t, r, b])) => {
                    let monitors = crate::monitors::recent();
                    if let Some(on) = crate::wallpaper::monitor_in_use(&monitors) {
                        let (w, h) = ((r - l).min(on.work[2] - on.work[0]), (b - t).min(on.work[3] - on.work[1]));
                        crate::own_windows::glide(hwnd, [l, t, r - l, b - t], [(on.work[0] + on.work[2] - w) / 2, (on.work[1] + on.work[3] - h) / 2, w, h]);
                        crate::win32_desktop::raise(hwnd);
                    }
                }
                Some((hwnd, _, _)) => crate::win32_desktop::close(hwnd),
                None => {}
            }
            if closed {
                WINDOWS_LIST.store(false, Ordering::Release);
            }
        }
        #[cfg(not(windows))]
        let _ = ctx;
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
                Some((format!("{}  {:02}:{:02}", if remaining.is_some() { "TIMER" } else { "STOPWATCH" }, secs / 60, secs % 60), share))
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
        let (accent, ink) = (crate::hud_board::active_colour(), Color32::from_rgb(0xE8, 0xF2, 0xFC));
        let line_font = FontId::new(15.5, egui::FontFamily::Name("label".into()));
        // (Laid out first: the whole of it stands level with the emblem's middle.)
        let laid: Vec<(&str, Vec<Arc<egui::Galley>>)> = sections.iter().map(|(title, lines)| (*title, lines.iter().map(|l| p.layout(l.clone(), line_font.clone(), ink, width)).collect())).collect();
        let height: f32 = laid.iter().map(|(_, lines)| 30.0 + lines.iter().map(|g| g.size().y + 5.0).sum::<f32>() + 14.0).sum();
        let mut y = (centre.y - height / 2.0).max(home.top() + HEADER_H + 30.0);
        for (title, lines) in laid {
            p.text(pos2(x, y), Align2::LEFT_TOP, title, FontId::monospace(10.5), accent);
            y += 17.0;
            p.line_segment([pos2(x, y), pos2(x + width, y)], egui::Stroke::new(1.0f32, crate::hud_style::white(0.10)));
            y += 13.0;
            for galley in lines {
                let h = galley.size().y;
                p.galley(pos2(x, y), galley, ink);
                y += h + 5.0;
            }
            y += 14.0;
        }
        // (The minutes on it count down.)
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// The orb that leaves the background (`wallpaper::Orb`). Spoken to while another program's
    /// window covers the emblem, it flies from the emblem's place to a corner of the monitor
    /// Sir is using, smaller, in front of everything, with what is being said beside it; five
    /// seconds after the last of it, it flies back and is part of the background again. Sir
    /// can drag it anywhere (it comes out there on that monitor from then on); a click opens
    /// the console, a double click brings the board back.
    fn corner_orb_frame(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings) {
        use crate::wallpaper::{OrbPhase, Standing};
        let (centre, radius) = self.emblem_spot;
        if self.backdrop != Standing::On || self.boot.is_some() || radius <= 0.0 {
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
        let timer_label = timers.first().map(|(line, _)| line.clone());
        let progress: Option<Option<f32>> = timers.first().map(|(_, share)| *share).or_else(|| crate::hud_board::researching().then_some(None));
        let working = m.quick.as_ref().is_some_and(|(_, _, until)| *until > now)
            || crate::hud_lines::offered().is_some()
            || progress.is_some()
            // (A question of his, or orders he offers, wait for Sir's word.)
            || crate::ask_first::pending().is_some()
            || crate::did_you_mean::current().is_some();
        let gaming = crate::hud_feel::game_in_front();
        // (A bit smaller than on the background, and smaller still beside a game.)
        let size = ((radius * 2.0 * ppp) * if gaming { 0.3 } else { 0.42 }).clamp(96.0, 168.0).round() as i32;
        let was_home = self.corner_orb.is_home();
        let mut out_on = self.orb_out_on;
        // (A scripted session can ask to see it fly although nothing covers its emblem: `JAI_DEMO=orb`.)
        let hidden = crate::wallpaper::hidden() || demo("orb");
        // The monitor it would come out on (a scripted session's: only its own).
        let in_use = if was_home { crate::wallpaper::monitor_in_use(&crate::monitors::recent()) } else { None };
        if was_home && in_use.is_none() {
            return;
        }
        let frame = self.corner_orb.step(now, talking || working, hidden, home, || {
            let work = in_use.map_or([0, 0, 1920, 1040], |m| m.work);
            out_on = Some((work, size));
            crate::wallpaper::corner(work, size, crate::wallpaper::spot_on(work))
        });
        self.orb_out_on = out_on;
        let Some(frame) = frame else {
            if !was_home {
                info!("[DESKTOP] The orb is back in the background");
            }
            return;
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
        let words = resting && !gaming && (!(said.is_empty() && answered.is_empty() && m.state == "idle") || timer_label.is_some() || asked);
        let orb_px = frame.rect.width().round();
        const WORDS_WIDTH: f32 = 380.0;
        let window_px = if words { vec2(orb_px + (12.0 + WORDS_WIDTH) * ppp, orb_px.max(96.0 * ppp)) } else { Vec2::splat(orb_px) };
        // Sir moved it by hand: it rests where he let go, and comes out there on that monitor.
        if let Some(actual) = ctx.input(|i| i.raw.viewports.get(&viewport).and_then(|v| v.outer_rect)) {
            let at = pos2((actual.min.x * ppp).round(), (actual.min.y * ppp).round());
            if let Some(moved) = self.corner_orb.let_go(at, crate::wallpaper::button_down()) {
                let monitors = crate::monitors::recent();
                if let Some(on) = monitors.iter().find(|m| m.contains(moved.center().x as i32, moved.center().y as i32)) {
                    crate::wallpaper::remember_spot(on.work, crate::wallpaper::spot_of(on.work, orb_px as i32, at));
                    info!("[DESKTOP] Sir put the orb at {:.0},{:.0}: its place on that monitor from now on", at.x, at.y);
                }
            }
        }
        let at = self.corner_orb.resting().map_or(frame.rect.min, |r| r.min);
        let builder = egui::ViewportBuilder::default()
            .with_title("J.A.I. // orb")
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
        #[cfg(windows)]
        {
            if was_home {
                self.orb_tended = (now, now.checked_sub(Duration::from_secs(1)).unwrap_or(now));
                crate::win32_desktop::orb_sets_out();
            }
            let (born, last) = self.orb_tended;
            // (Its flight out begins once its window is on show; it waits a second for that at most.)
            if !crate::win32_desktop::orb_on_show() && now.duration_since(born) < Duration::from_secs(1) {
                self.corner_orb.wait(now);
            }
            if now.duration_since(born) < Duration::from_millis(1500) || now.duration_since(last) > Duration::from_millis(300) {
                self.orb_tended.1 = now;
                let drawn = ctx.cumulative_pass_nr_for(viewport) > 0;
                crate::win32::run_detached(move || {
                    if let Some(hwnd) = crate::win32::own_window("J.A.I. // orb") {
                        crate::win32_desktop::tend_orb(hwnd, gaming, drawn);
                    }
                });
            }
        }
        let content = CornerOrbContent {
            orb_px,
            progress,
            words,
            said,
            answered,
            timer: timer_label,
            state: m.state.clone(),
            live_words: iface.live_words,
            // What waits for his word: a question before a guess is carried out, or the
            // orders closest to one he did not know.
            question: crate::ask_first::pending().map(|(question, _)| question),
            choices: crate::did_you_mean::current().map(|offer| offer.choices.into_iter().map(|c| c.label).collect()).unwrap_or_default(),
        };
        let mut did = OrbClick::Nothing;
        let emblem = &self.emblem;
        ctx.show_viewport_immediate(viewport, builder, |ctx, _class| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| did = corner_orb_ui(ui, emblem, &content));
        });
        match did {
            OrbClick::Board => self.app.config().update(|c| c.interface.desktop_background = false),
            OrbClick::Console => {
                self.console_open = true;
                self.console_focus = true;
            }
            OrbClick::Drag if self.corner_orb.grabbed() => ctx.send_viewport_cmd_to(viewport, ViewportCommand::StartDrag),
            OrbClick::Drag => {}
            OrbClick::Yes => self.command(HudCommand::Chat { text: "yes".into() }),
            OrbClick::No => crate::ask_first::drop_it(),
            OrbClick::Choice(k) => crate::did_you_mean::pick(k),
            OrbClick::Nothing => {}
        }
        // (It moves, or waits to go back: the board draws on while it is out.)
        ctx.request_repaint_after(Duration::from_millis(if resting { 50 } else { 8 }));
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

    // ---- The gallery: what the look-and-feel pieces of 2026-11-12 draw, rendered offscreen as
    // PNGs (target/ui-snapshots/gallery_*.png) to be looked at. Each asserts only that it drew.

    fn gallery(name: &str, size: Vec2, frames: usize, mut ui: impl FnMut(&egui::Context)) {
        let mut snap = crate::ui_snapshot::Snapshot::new(size);
        let canvas = snap.render(frames, |ctx| {
            paint_canvas(ctx);
            ui(ctx);
        });
        canvas.save(name);
        assert!(canvas.brightness(Rect::from_min_size(Pos2::ZERO, size)) > 1.0, "{name}: nothing was drawn");
    }

    const GALLERY: Vec2 = Vec2::new(1280.0, 760.0);

    fn emblem_at_rest() -> RoseEmblem {
        let mut emblem = RoseEmblem::new(crate::rose_emblem_renderer::RoseEmblemState::AwakeFrontEmblem);
        emblem.step(1.0 / 60.0, 0.0, false);
        emblem
    }

    /// Tool blocks: the header every block wears, and what a block shows while it loads, has
    /// nothing to show, or failed.
    #[test]
    fn gallery_blocks_and_their_states() {
        use crate::hud_style::{Light, Outline};
        let _g = crate::hud_dock::test_lock();
        let ids = ["gallery_stats", "gallery_picture", "gallery_failed", "gallery_empty", "gallery_list", "gallery_chart"];
        for (id, light) in ids.iter().zip([Light::Good, Light::Busy, Light::Fault, Light::Rest, Light::Busy, Light::Warn]) {
            crate::hud_style::set_light(id, light);
        }
        gallery("gallery_blocks", GALLERY, 8, |ctx| {
            crate::hud_dock::begin_frame();
            let p = Palette::new(&InterfaceSettings::default());
            hud_block_window(ctx, ids[0], "System telemetry", [400.0, 240.0], |ui| {
                for (key, value, share) in [("CPU", "23.4% · 16 threads", 0.23), ("RAM", "18.2 / 31.9 GB", 0.57), ("RTX 4070", "52°C · 12%", 0.12), ("VRAM", "3.1 / 12.0 GB", 0.26)] {
                    row(ui, &p, key, value.to_string());
                    meter(ui, &p, share, 6.0, p.accent, &[]);
                }
            });
            hud_block_window(ctx, ids[1], "Pictures // Eiffel Tower", [400.0, 240.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::hud_style::outline(ui, room.shrink(6.0), Outline::Picture);
            });
            hud_block_window(ctx, ids[2], "3D studio // apple", [400.0, 240.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::hud_style::failed(ui, room, "The model could not be downloaded (no connection).");
            });
            hud_block_window(ctx, ids[3], "Notes", [400.0, 220.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::hud_style::empty(ui, room, "No notes yet. Say: add a note saying call Tom at five.");
            });
            hud_block_window(ctx, ids[4], "Processes", [400.0, 220.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::hud_style::outline(ui, room.shrink(6.0), Outline::List);
            });
            hud_block_window(ctx, ids[5], "Network", [400.0, 220.0], |ui| {
                let room = ui.available_rect_before_wrap();
                crate::hud_style::outline(ui, room.shrink(6.0), Outline::Chart);
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

    #[test]
    fn gallery_notifications_and_edge_light() {
        use crate::hud_notify::{Card, Edge};
        crate::hud_notify::edge(Edge::Notice);
        std::thread::sleep(Duration::from_millis(220));
        gallery("gallery_notifications", GALLERY, 2, |ctx| {
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
        let emblem = emblem_at_rest();
        let mut bands = [0.0f32; crate::pc_audio::BANDS];
        for (k, b) in bands.iter_mut().enumerate() {
            *b = (0.92 - k as f32 * 0.03) * (0.55 + 0.45 * (k as f32 * 1.7).sin().abs());
        }
        crate::hud_lines::dismiss_offer();
        crate::hud_lines::heard("Jay, open the mixer and turn the music");
        let draw = |ctx: &egui::Context, state: &'static str, lyric: Option<&str>, spectrum: bool| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let screen = ctx.screen_rect();
                let painter = ui.painter_at(screen);
                emblem.paint(&painter, screen);
                let frame = emblem.frame(screen);
                if spectrum {
                    paint_spectrum_ring(&painter, &frame, &bands);
                    crate::hud_lines::paint_mic_label(&painter, frame.centre, frame.radius, "TOO QUIET", 1.0);
                } else {
                    crate::hud_style::ripple(&painter, frame.centre, frame.radius, 0.2, crate::hud_board::active_colour());
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

    /// The emblem close up: its housing, its uneven light, the glass over it.
    #[test]
    fn gallery_emblem_close_up() {
        let emblem = emblem_at_rest();
        gallery("gallery_emblem", vec2(900.0, 900.0), 2, |ctx| {
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let screen = ctx.screen_rect();
                emblem.paint_in(&ui.painter_at(screen), screen);
            });
        });
    }

    #[test]
    fn gallery_idle_screen_and_startup() {
        use crate::startup_sequence::Kind;
        let emblem = emblem_at_rest();
        let base = |ctx: &egui::Context| -> crate::rose_emblem_renderer::EmblemFrame {
            let screen = ctx.screen_rect();
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| emblem.paint(&ui.painter_at(screen), screen));
            emblem.frame(screen)
        };
        crate::idle_screen::show_glance(crate::idle_screen::Glance { next: Some(("Physics".into(), chrono::Local::now().naive_local() + chrono::Duration::minutes(25), "204".into())), weather: Some((7.0, "light rain".into())), lesson: None });
        gallery("gallery_idle", GALLERY, 2, |ctx| {
            let f = base(ctx);
            crate::idle_screen::paint(ctx, ctx.screen_rect(), f.centre, f.radius, 1.0);
        });
        for (name, kind, ms) in [("gallery_startup_long_a", Kind::Long, 2400), ("gallery_startup_long_b", Kind::Long, 3950), ("gallery_startup_short", Kind::Short, 900)] {
            gallery(name, GALLERY, 2, |ctx| {
                let f = base(ctx);
                crate::startup_sequence::paint(ctx, ctx.screen_rect(), f.centre, f.radius, kind, Duration::from_millis(ms));
            });
        }
    }

    #[test]
    fn gallery_mini_strip_and_subtitles() {
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
        let emblem = RoseEmblem::new(crate::rose_emblem_renderer::RoseEmblemState::AwakeFrontEmblem);
        let orb = |name: &str, c: CornerOrbContent| {
            gallery(name, vec2(560.0, 168.0), 2, |ctx| {
                ctx.layer_painter(egui::LayerId::background()).rect_filled(ctx.screen_rect(), Rounding::ZERO, Color32::from_rgb(0xD8, 0xDC, 0xE0));
                egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                    corner_orb_ui(ui, &emblem, &c);
                });
            });
        };
        let content = |said: &str, answered: &str| CornerOrbContent {
            orb_px: 168.0,
            progress: None,
            words: true,
            said: said.into(),
            answered: answered.into(),
            timer: None,
            state: "speaking".into(),
            live_words: false,
            question: None,
            choices: Vec::new(),
        };
        orb("gallery_corner_orb_words", CornerOrbContent { progress: Some(Some(0.35)), timer: Some("TIMER  03:12".into()), ..content("Jay, what is on my calendar tomorrow?", "Two things, Sir: physics at 8:35 in room 204 and the dentist at half past two.") });
        orb("gallery_corner_orb_question", CornerOrbContent { question: Some("Did you mean: create a protocol called 'evening', Sir?".into()), ..content("Jay, save that as evening.", "") });
        orb("gallery_corner_orb_choices", CornerOrbContent { choices: vec!["save this desk layout as evening".into(), "show my desk layouts".into(), "restore the desk layout evening".into()], ..content("Jay, keep the desk like this.", "") });
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
                        sharp_block(ui, "WIDGET // DAILY BRIEFING SUMMARY", "NOTE", |ui| {
                            for line in lines {
                                ui.add(egui::Label::new(RichText::new(line).size(13.5).color(STRIP_INK)).wrap());
                                ui.add_space(4.0);
                            }
                        });
                        sharp_block(ui, "WIDGET // A SHORT ONE", "NOTE", |ui| {
                            ui.label(RichText::new("One line.").size(13.5).color(STRIP_INK));
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

    /// `--monitor 2`: the fullscreen board covers that monitor from its first frame and a window
    /// starts in its middle; without it, the board is the primary's at (0, 0). A board across
    /// every monitor is turned off for such a session in `main` (its settings stay in memory).
    #[test]
    fn a_test_session_opens_on_the_monitor_it_names() {
        let second = Rect::from_min_size(pos2(2560.0, 0.0), vec2(2560.0, 1440.0));
        let windowed = vec2(1500.0, 940.0);
        let board = InterfaceSettings { fullscreen: true, orb_mode: false, span_all_monitors: false, ..InterfaceSettings::default() };
        let g = startup_geometry(&board, windowed, Some(second));
        assert!(g.fullscreen && !g.transparent);
        assert_eq!((g.size, g.position), (vec2(2560.0, 1440.0), Some([2560.0, 0.0])));
        assert_eq!(startup_geometry(&board, windowed, None).position, Some([0.0, 0.0]));
        let window = InterfaceSettings { fullscreen: false, ..board.clone() };
        let g = startup_geometry(&window, windowed, Some(second));
        assert!(!g.fullscreen && g.transparent && g.size == windowed);
        assert_eq!(g.position, Some([2560.0 + 1280.0 - 750.0, 720.0 - 470.0]));
        assert_eq!(startup_geometry(&window, windowed, None).position, None, "without --monitor, where Windows puts it");
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
        let (light, dark) = (grain.pixels.iter().filter(|p| p.r() > 0).count(), grain.pixels.iter().filter(|p| p.r() == 0 && p.a() > 0).count());
        assert!(light > GRAIN_TILE * GRAIN_TILE / 4 && dark > GRAIN_TILE * GRAIN_TILE / 4, "lighter and darker specks: {light} / {dark}");
        // His yellow glows warm (red and green lead), Arc Cyan cool, and both stay dark.
        let warm = vignette_for(Color32::from_rgb(0xFF, 0xD4, 0x00));
        assert!(warm.r() > warm.b() && warm.g() > warm.b() && warm.r() < 40, "{warm:?}");
        let cool = vignette_for(Color32::from_rgb(0x00, 0xF0, 0xFF));
        assert!(cool.b() > cool.r() && cool.b() < 40, "{cool:?}");
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
        assert!(start.moving() && start.unfold == 0.0 && start.frame == 0.0 && start.resolve == 0.0 && start.brackets_alpha == 0.0);
        for n in 0..=100 {
            let (m, ms) = (entrance(n), ms_in(n));
            assert_eq!(m.flash, 0.0, "no flash on the way in ({ms} ms)");
            assert!(m.brackets >= m.unfold - 1e-6, "the brackets lead the frame ({ms} ms)");
            if ms < IN_UNFOLD.1 {
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
                    assert!(w1 > w0, "the hairline draws out of the middle ({ms} ms)");
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
        assert!(exit(35).content < 1e-6 && exit(35).unfold > 0.9, "the content is gone while the frame has barely folded");
        let end = exit(100);
        assert!(end.frame < 1e-6 && end.content < 1e-6 && end.brackets_alpha < 1e-6 && end.unfold < 1e-6 && end.line.is_none());
        let flash = (0..=100).max_by(|a, b| exit(*a).flash.total_cmp(&exit(*b).flash)).unwrap_or(0);
        assert!((150.0..=200.0).contains(&ms_out(flash)) && exit(flash).flash > 0.9, "the point flashes once at the end: {} ms", ms_out(flash));
        // Closed while it was still opening: the exit takes over.
        assert_eq!(WindowMotion::of(Some(0.4), Some(0.2)), exit(20));
    }

    /// A border drawn inside its rect: whole pixels at any scale, nothing outside the rect (a
    /// block flush with the screen's edge keeps all four sides).
    #[test]
    fn borders_stay_inside_their_rect_at_any_scale() {
        for ppp in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 200.0));
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("border_test")));
                inner_border(&painter, rect, 1.0, Color32::WHITE);
            });
            let shapes: Vec<Rect> = out.shapes.iter().map(|c| c.shape.visual_bounding_rect()).filter(|r| r.is_positive()).collect();
            assert_eq!(shapes.len(), 4, "four sides at {ppp}");
            for side in &shapes {
                assert!(rect.expand(1e-3).contains_rect(*side), "inside the rect at {ppp}: {side:?}");
                let thin = side.width().min(side.height()) * ppp;
                assert!((thin - thin.round()).abs() < 1e-3 && thin >= 1.0 - 1e-3, "whole pixels at {ppp}: {thin}");
            }
            let union = shapes.iter().copied().reduce(|a, b| a.union(b)).unwrap_or(Rect::NOTHING);
            assert!((union.min - rect.min).length() < 1e-3 && (union.max - rect.max).length() < 1e-3, "all four edges at {ppp}");
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
        install_fonts(&ctx);
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

    /// A body that asks for more room than the block has (the 3D studio's footer did, by 10 px)
    /// is cut off at the block's edge: the block keeps its size frame after frame.
    #[test]
    fn a_block_never_grows_to_its_content() {
        let _g = crate::hud_dock::test_lock();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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

    /// Its [ DETACH ] (with the motion on): the block plays its exit and then asks for a window
    /// of its own, never for its close, and the window opens where the block stood.
    #[test]
    fn a_detached_block_leaves_for_its_own_window() {
        let _g = crate::hud_dock::test_lock();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        set_still(&ctx, false);
        let id = "detach_probe";
        let frame = || {
            let mut out = BlockAction::default();
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                crate::hud_dock::begin_frame();
                out = hud_block_window(ctx, id, "Detach probe", [600.0, 400.0], |ui| {
                    ui.label("probe");
                });
            });
            out
        };
        // Up, and settled at the size of what it shows (one word: the smallest block).
        let start = Instant::now();
        while start.elapsed() < crate::hud_dock::OPEN_ANIM + crate::hud_dock::GLIDE + Duration::from_millis(200) {
            frame();
        }
        let stood = crate::hud_dock::block_rect(id).expect("drawn");
        assert!(stood.width() < 400.0 && stood.height() < 300.0, "a block round one word is a small one: {stood:?}");
        crate::hud_dock::begin_detach(id);
        let start = Instant::now();
        let mut left = None;
        while left.is_none() && start.elapsed() < crate::hud_dock::CLOSE_FADE * 3 {
            let out = frame();
            assert!(!out.close, "[ DETACH ] closed the block");
            left = out.detach.then_some(());
        }
        assert!(left.is_some(), "the block never asked for its own window");
        assert_eq!(crate::hud_dock::block_rect(id), Some(stood), "the window's place is the block's");
        crate::hud_dock::forget(id);
    }

    /// "Move it to the middle", "put it on the left half", "full screen it", "minimise it",
    /// "restore it": the HUD carries each out on the block in its next frames.
    #[test]
    fn blocks_go_where_sir_sends_them() {
        use crate::window_commands::{Action, Arrangement, Position};
        let _g = crate::hud_dock::test_lock();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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

    /// A window of its own never hangs off a screen: it is moved (and shrunk only when it must
    /// be) into the work area of the monitor its middle is on.
    #[test]
    fn detached_windows_land_inside_a_work_area() {
        let monitor = |index: usize, x: i32, primary: bool| crate::monitors::Monitor { index, name: format!("M{index}"), rect: [x, 0, x + 1920, 1080], work: [x, 0, x + 1920, 1040], primary };
        let desk = [monitor(1, 0, true), monitor(2, 1920, false)];
        let fits = |r: Rect, ppp: f32| {
            let placed = clamp_to_work_area(r, &desk, ppp);
            assert!(desk.iter().any(|m| Rect::from_min_max(pos2(m.work[0] as f32, m.work[1] as f32) / ppp, pos2(m.work[2] as f32, m.work[3] as f32) / ppp).expand(1e-3).contains_rect(placed)), "{r:?} at {ppp}: {placed:?}");
            placed
        };
        // Flush against the right edge of the second monitor and under its taskbar.
        let placed = fits(Rect::from_min_size(pos2(3500.0, 900.0), vec2(800.0, 500.0)), 1.0);
        assert_eq!(placed.size(), vec2(800.0, 500.0), "only moved");
        assert_eq!(placed.max, pos2(3840.0, 1040.0));
        // Larger than a monitor: shrunk to it. At 150 % the work area is in points.
        assert_eq!(fits(Rect::from_min_size(pos2(-50.0, -50.0), vec2(2400.0, 1300.0)), 1.0).size(), vec2(1920.0, 1040.0));
        let scaled = fits(Rect::from_min_size(pos2(1200.0, 600.0), vec2(500.0, 300.0)), 1.5);
        assert!(scaled.size() == vec2(500.0, 300.0) && scaled.max.y <= 1040.0 / 1.5 + 1e-3, "{scaled:?}");
        // Already inside: untouched.
        let inside = Rect::from_min_size(pos2(100.0, 100.0), vec2(600.0, 400.0));
        assert_eq!(fits(inside, 1.0), inside);
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
        assert_eq!(pill_colours("LISTENING")[1], Color32::from_rgb(0x3E, 0xE0, 0x7A));
        assert_eq!(pill_colours("THINKING")[0], Color32::from_rgb(0xFF, 0xA5, 0x30));
        assert_eq!(pill_colours("SPEAKING")[0], Color32::from_rgb(0xFF, 0x5A, 0x3A));
        assert_eq!(rim_target("listening", "idle", 0.9, 0.0), 0.0, "silence");
        assert!(rim_target("listening", "speaking", 0.6, 0.0) >= 0.6);
        assert!(rim_target("idle", "pre_speech", 0.0, 0.0) >= 0.25, "a quiet voice still shows");
        assert_eq!(rim_target("speaking", "speaking", 0.9, 0.0), 0.0, "never while JAI speaks");
        assert_eq!(rim_target("listening", "speaking", 0.9, 1.0), 0.0, "never docked");
        let centre = pos2(500.0, 400.0);
        let ticks = rim_ticks(centre, 100.0, 1.0, 2.5);
        assert_eq!(ticks.len(), 120);
        for (from, to, alpha) in &ticks {
            let (r0, r1) = ((*from - centre).length(), (*to - centre).length());
            assert!(r0 >= 107.9 && r1 > r0 && r1 <= r0 + 22.01 && *alpha <= 1.0, "{r0} {r1} {alpha}");
        }
        let lengths: Vec<f32> = ticks.iter().map(|(a, b, _)| (*b - *a).length()).collect();
        assert!(lengths.iter().copied().fold(0.0, f32::max) - lengths.iter().copied().fold(f32::MAX, f32::min) > 5.0, "the lengths ripple");
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let hints = key_hints(&painter, 1800.0, 1060.0);
            assert_eq!(hints.iter().map(|h| h.0).collect::<Vec<_>>(), ["F1 MUTE", "F11 FULLSCREEN", "ESC INTERRUPT"]);
            assert!((hints[2].1.right() - 1800.0).abs() < 0.5);
            assert!(hints.windows(2).all(|w| w[0].1.right() + 10.0 < w[1].1.left()), "apart");
            let free = pill_rect(&painter, screen, &[Rect::from_min_size(pos2(1500.0, 10.0), vec2(400.0, HEADER_H))]).expect("room");
            assert!((free.center().x - 960.0).abs() < 0.5 && free.top() < 12.0, "top centre: {free:?}");
            let crowded = pill_rect(&painter, screen, &[Rect::from_min_size(pos2(700.0, 10.0), vec2(1200.0, HEADER_H))]).expect("room");
            assert!(crowded.right() <= 690.0 && crowded.top() < 12.0, "left of a row that reaches the centre: {crowded:?}");
            assert_eq!(pill_rect(&painter, screen, &[Rect::from_min_size(pos2(40.0, 10.0), vec2(1860.0, HEADER_H))]), None, "no room, no pill");
            // The card's line: a short title whole, a long one cut to the card's width.
            let font = FontId::proportional(12.0);
            assert_eq!(fit_width(&painter, "Settings", &font, 316.0), "Settings");
            let long = fit_width(&painter, &"How the Apollo guidance computer landed on the Moon - YouTube — Mozilla Firefox".repeat(2), &font, 316.0);
            assert!(long.ends_with('…') && painter.layout_no_wrap(long.clone(), font.clone(), Color32::WHITE).size().x <= 316.0, "{long}");
            assert!(long.chars().count() > 20, "as much as fits: {long}");
        });
        let home = crate::rose_emblem_renderer::EmblemFrame { centre: pos2(960.0, 500.0), radius: 200.0, flat: 1.0, dock: 0.0 };
        let card = screen_card_rect(screen, &home);
        assert!(card.top() > 700.0 && (card.center().x - 960.0).abs() < 0.5, "under the emblem: {card:?}");
        let low = crate::rose_emblem_renderer::EmblemFrame { centre: pos2(1900.0, 1000.0), ..home };
        assert!(screen.contains_rect(screen_card_rect(screen, &low)), "always whole on the screen");
        let docked = crate::rose_emblem_renderer::EmblemFrame { dock: 1.0, ..home };
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
        let (centre, _) = crate::rose_emblem_renderer::home_pose(home, 0.0);
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
                assert_eq!(frame.find_exact("[ MOVE HERE ]").len(), if spanning { 3 } else { 2 }, "{case}");
                assert_eq!(frame.find_exact("HUD IS HERE").len(), usize::from(!spanning), "{case}");
                // The switch says what it will do, and does it.
                let (label, turns) = if spanning { ("[ BACK TO ONE MONITOR ]", false) } else { ("[ SPAN ALL MONITORS ]", true) };
                let key = frame.find_exact(label);
                assert_eq!(key.len(), 1, "{case}: {label}");
                stage.click(key[0].center(), egui::PointerButton::Primary, &mut menu);
                assert_eq!(picked.get(), ScreensPick { span: Some(turns), ..Default::default() }, "{case}");
                // `[ MOVE HERE ]` names its own monitor (the rows are in the monitors' order).
                let mut moves = frame.find_exact("[ MOVE HERE ]");
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
                ui.set_width(340.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("DISPLAY 3  MONITOR 3 (1080×1920 AT -1080,-290)").font(strip_font()).color(STRIP_INK));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(egui::Button::new(RichText::new("[ MONITOR 3 ]").font(strip_font())).frame(false)));
                });
            });
        });
        let found = visual::text_over_text(&frame);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("[ MONITOR 3 ]"), "{found:?}");
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
        assert_eq!(target_fps(true, false, true, 180.0), 30.0);
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
        install_fonts(&ctx);
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
        // The meter: crimson rising to a white tip, nothing when silent.
        let mut mesh = egui::Mesh::default();
        meter_ribbon(&mut mesh, pos2(0.0, 0.0), 0.0, 1.5);
        assert!(mesh.vertices.is_empty());
        meter_ribbon(&mut mesh, pos2(0.0, 0.0), 50.0, 1.5);
        let tip = mesh.vertices.iter().max_by(|a, b| a.pos.x.total_cmp(&b.pos.x)).unwrap();
        assert_eq!(tip.color, Color32::WHITE);
        let start = mesh.vertices.iter().min_by(|a, b| a.pos.x.total_cmp(&b.pos.x)).unwrap();
        assert_eq!(start.color.a(), 0, "fades in from nothing");
        assert!(mesh.vertices.iter().all(|v| v.pos.x <= 50.0 + 1e-3));
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
        assert_eq!((rest_fps(false), rest_fps(true)), (30.0, 10.0));
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
                let emblem = crate::hud_dock::docked_centre(screen, console).x - crate::rose_emblem_renderer::DOCKED_DIAMETER / 2.0;
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
