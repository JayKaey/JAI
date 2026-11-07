//! Native HUD window: egui drawn with OpenGL (eframe/glow) straight into a desktop window — no
//! browser, no webview, no console. It lives on the main thread for the whole session; closing
//! it hides it to the tray, and Jarvis restyles it live when Sir asks (`InterfaceSettings`).
//!
//! Backend events are applied to a shared `HudModel` by a background task, so nothing is missed
//! while the window is hidden; the window drives `app::Jarvis` with `HudCommand`s.
//!
//! The window is the HUD board: an obsidian canvas with the gyroscopic ring emblem
//! (`rose_emblem_renderer`), the board of sharp technical blocks (`hud_board`), a micro header
//! strip and a micro telemetry strip. Every exchange appears in the `COMMS // LIVE RESPONSE` block
//! above the strip; the header's `[ CONSOLE ]` (Ctrl+L, "open text interface") slides
//! out the text console with the full transcript and an input line. Jarvis' live widgets,
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

#[path = "gui_mixer.rs"]
mod mixer_view;

#[path = "gui_network_radar.rs"]
mod radar_view;

#[path = "gui_disk_map.rs"]
mod disk_map_view;

#[path = "gui_earth.rs"]
mod earth_view;

#[path = "gui_discord.rs"]
mod discord_view;

#[path = "gui_workout.rs"]
mod workout_view;
/// The HUD drawn without a window (the live self-test and the exhaustive verifier).
#[path = "gui_offscreen.rs"]
pub(crate) mod offscreen;

use crate::app::Jarvis;
use crate::rose_emblem_renderer::{EmblemInputs, RoseEmblem};
use crate::config::ConfigStore;
use crate::ears::AudioLevel;
use crate::hud::{ConfigState, HudCommand, HudEvent};
use crate::interface::{self, HudLayout, InterfaceSettings, WindowCommand, THEMES};
use crate::memory::MemoryEntry;
use crate::modality::{InputSource, Trigger, VoiceResponseMode};
use crate::skills::JarvisSkill;
use crate::telemetry::SystemTelemetry;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Frame, Key, Layout, Margin,
    Pos2, Rect, RichText, Rounding, Sense, Shape, Stroke, Ui, Vec2, ViewportCommand, ViewportId,
};
use std::collections::VecDeque;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tracing::{info, warn};

const WINDOW_TITLE: &str = "J.A.R.V.I.S. HUD";
const MAX_MESSAGES: usize = 200;
const MAX_TOOL_LINES: usize = 60;
const MEMORY_CATEGORIES: [&str; 6] = ["preference", "personal", "project", "schedule", "system", "general"];
// ------------------------------------------------------------------- process-wide handle ----

enum Wake {
    Open,
    Quit,
}

/// Lets the tray, voice commands and a second `JARVIS.exe` reach the window from any thread.
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

/// Whether Jarvis can open native windows (false when headless).
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

/// The part of the HUD in-app blocks live in right now (nothing while no HUD is up).
pub fn block_area() -> Rect {
    context().map_or(Rect::NOTHING, |ctx| block_bounds(home_screen(&ctx)))
}

/// The HUD's native window handle (0 while it has none).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hud_hwnd() -> isize {
    SHARED.hwnd.load(Ordering::Acquire)
}

/// Widget content changed or a window closed: a repaint is enough (Jarvis' windows redraw and
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
/// second `JARVIS.exe`, restyling requests). Does nothing in headless mode.
pub fn show() {
    if !SHARED.running.load(Ordering::Acquire) {
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
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
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

/// Opens (or closes) the Feature Lab: every tool Jarvis has, each with its `[ TRY ]`.
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

/// Hides the HUD to the tray; Jarvis keeps running.
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

/// Ends `run`; called once Jarvis' own shutdown work is done.
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
        return Err("Jarvis is running without his HUD window (headless), so there is no window to change.".into());
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

/// Resizes the HUD window (logical points) — `control_jarvis_interface` set_window_size.
pub fn set_window_size(width: f32, height: f32) -> Result<(), String> {
    if !SHARED.running.load(Ordering::Acquire) {
        return Err("Jarvis is running without his HUD window (headless), so there is no window to resize.".into());
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

/// The HUD's link to the backend; it follows Jarvis' events from `attach` on, whether or not a
/// window is open.
pub struct Hud {
    app: Jarvis,
    rt: Handle,
    model: Arc<Mutex<HudModel>>,
    wake: mpsc::Receiver<Wake>,
}

impl Hud {
    /// Starts following Jarvis (before he says anything, so the chat shows it all).
    pub fn attach(app: Jarvis, rt: Handle) -> Self {
        let (tx, wake) = mpsc::channel();
        *lock(&SHARED.wake) = Some(tx);
        let model = Arc::new(Mutex::new(HudModel::default()));
        feed_model(&app, &rt, &model);
        SHARED.running.store(true, Ordering::Release);
        Self { app, rt, model, wake }
    }

    /// Runs the window on the calling (main) thread until Jarvis quits. Returns early — Jarvis
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
                    "The J.A.R.V.I.S. window could not be opened ({e}). He keeps listening in the tray; \
                     updating the graphics driver usually fixes this."
                ));
                break;
            }
            // On Windows the window only ends when Jarvis quits. Elsewhere closing it lands here,
            // and the tray (or "show yourself") opens a new one.
        }
        SHARED.running.store(false, Ordering::Release);
        lock(&SHARED.wake).take();
    }
}

/// Keeps `model` current from backend events, whether or not a window is open.
fn feed_model(app: &Jarvis, rt: &Handle, model: &Arc<Mutex<HudModel>>) {
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
fn send(app: &Jarvis, rt: &Handle, model: &Arc<Mutex<HudModel>>, cmd: HudCommand) {
    let (app, model) = (app.clone(), Arc::clone(model));
    rt.spawn(async move {
        if let Some(reply) = app.handle_command(cmd).await {
            lock(&model).apply(reply);
            request_repaint();
        }
    });
}

fn open_window(app: &Jarvis, rt: &Handle, model: &Arc<Mutex<HudModel>>) -> Result<(), String> {
    let (iface, placement, maximized) = app.config().read(|c| (c.interface.clone(), c.hud_window, c.hud_maximized));
    let on_top = if iface.orb_mode { iface.orb_on_top } else { iface.always_on_top };
    let level = if on_top { egui::viewport::WindowLevel::AlwaysOnTop } else { egui::viewport::WindowLevel::Normal };
    let (size, min) = if iface.orb_mode { (vec2(iface.orb_size_px, iface.orb_size_px), vec2(60.0, 60.0)) } else { (layout_size(iface.layout), min_size(iface.layout)) };
    let geometry = startup_geometry(&iface, size);
    let fullscreen = geometry.fullscreen;
    let viewport = egui::ViewportBuilder::default()
        .with_title(window_title(&iface))
        .with_app_id("jarvis")
        .with_inner_size(geometry.size)
        .with_resizable(true)
        .with_min_inner_size(min)
        // A see-through background ("opacity") and the floating orb need a transparent surface,
        // but on Windows a transparent window does not take the whole monitor in fullscreen (it
        // stayed a ~60 % rectangle in the top-left corner): the fullscreen board is opaque.
        .with_transparent(geometry.transparent)
        .with_decorations(!iface.frameless && !iface.orb_mode && !fullscreen && !iface.span_all_monitors)
        .with_fullscreen(fullscreen)
        .with_maximized(maximized && !fullscreen)
        .with_window_level(level)
        .with_icon(Arc::new(crate::app_icon::generate_rose_gyro_app_icon()));
    // Fullscreen: pinned to the primary monitor's full resolution at (0, 0) from the first frame.
    let viewport = match geometry.position {
        Some(at) => viewport.with_position(at),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: placement.is_none() && !fullscreen,
        persist_window: false,
        ..Default::default()
    };
    if fullscreen {
        info!("[HUD] Fullscreen board: {:.0}×{:.0} at (0, 0), opaque", geometry.size.x, geometry.size.y);
    }
    let restore = placement.filter(|_| !maximized && !fullscreen && !iface.orb_mode);
    let (app, rt, model) = (app.clone(), rt.clone(), Arc::clone(model));
    eframe::run_native(
        "J.A.R.V.I.S.",
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

/// The gyroscope emblem icon (256 px) for Jarvis' other windows; rendered once.
pub(crate) fn window_icon() -> Option<egui::IconData> {
    static ICON: std::sync::OnceLock<egui::IconData> = std::sync::OnceLock::new();
    Some(ICON.get_or_init(crate::app_icon::generate_rose_gyro_app_icon).clone())
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
/// mode never starts fullscreen). Otherwise the layout's window size, transparent.
fn startup_geometry(iface: &InterfaceSettings, windowed: Vec2) -> StartupGeometry {
    let spanning = iface.span_all_monitors && !iface.orb_mode;
    let fullscreen = iface.fullscreen && !iface.orb_mode && !spanning;
    if !fullscreen && !spanning {
        return StartupGeometry { fullscreen, transparent: true, size: windowed, position: None };
    }
    // One board across every monitor starts as the opaque borderless board on the first one
    // (never winit's fullscreen); `enforce_fullscreen` stretches it over the others at once.
    let (w, h) = primary_screen_points().unwrap_or((windowed.x, windowed.y));
    StartupGeometry { fullscreen, transparent: false, size: vec2(w, h), position: Some([0.0, 0.0]) }
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
    Jarvis,
    System,
}

struct ChatLine {
    id: String,
    who: Speaker,
    /// The bubble's header: "YOU" or "J.A.R.V.I.S." (empty for system lines).
    meta: String,
    /// A tiny pill beside the header: the latency ("140 ms"), "not for Jarvis", a time.
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
    skills: Vec<JarvisSkill>,
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
                    Some("not for Jarvis".to_string())
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
                            who: if sir { Speaker::Sir } else { Speaker::Jarvis },
                            pill: Some(time.get(11..16).unwrap_or(&time).to_string()),
                            detail: format!("An earlier session, {time}"),
                            meta: if sir { "YOU".to_string() } else { "J.A.R.V.I.S.".to_string() },
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
                    self.messages.retain(|l| !(l.who == Speaker::Jarvis && l.id == message_id));
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
                line.meta = "J.A.R.V.I.S.".to_string();
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

    /// Jarvis' (possibly still streaming) reply with this id.
    fn reply(&mut self, id: &str) -> &mut ChatLine {
        match self.messages.iter().rposition(|l| l.id == id && l.who == Speaker::Jarvis) {
            Some(i) => &mut self.messages[i],
            None => {
                self.push(ChatLine {
                    id: id.to_string(),
                    who: Speaker::Jarvis,
                    pill: None,
                    detail: String::new(),
                    meta: "J.A.R.V.I.S.".into(),
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

    fn toast(&mut self, text: String, error: bool) {
        let until = Instant::now() + Duration::from_secs(if error { 7 } else { 4 });
        self.toasts.push_back(Toast { text, error, until });
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
            (interface::mix([230, 247, 255], accent, 0.08), interface::mix([125, 159, 189], accent, 0.15))
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
            green: semantic([0, 150, 100], [0, 255, 170]),
            amber: semantic([190, 110, 0], [255, 176, 32]),
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

/// Segoe UI (+ Semibold for headings, Segoe UI Symbol for icons) and Cascadia Mono when
/// available, with egui's bundled fonts as fallbacks.
pub(crate) fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
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
    let monospace = [pick(&["mono", "symbols"]), fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default()].concat();
    // Always registered (the board and the emblem use it); Semibold when Bahnschrift is missing.
    let display = [pick(&["display", "segoe-semibold", "segoe", "symbols"]), bold.clone()].concat();
    for (name, bytes) in loaded {
        fonts.font_data.insert(name.to_string(), FontData::from_owned(bytes));
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Monospace, monospace);
    fonts.families.insert(bold_family(), bold);
    fonts.families.insert(FontFamily::Name("display".into()), display);
    ctx.set_fonts(fonts);
}

fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

thread_local! {
    /// Text size factor of the window being drawn: 1.0 for the HUD (its zoom is egui's), a
    /// Jarvis window's own `font_scale` while that window draws (egui's zoom is shared by all
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
            (TextStyle::Button, FontId::proportional(12.5 * f)),
            (TextStyle::Heading, FontId::new(17.0 * f, bold_family())),
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
    /// Smoothed loudness of Sir's microphone and of Jarvis' voice (0-1): the strings vibrate
    /// with them.
    mic: f32,
    voice: f32,
}

impl Reactor {
    /// What the emblem's voice equaliser follows: Jarvis' own voice only (his TTS playback
    /// level) — Sir talking into the microphone leaves it calm.
    fn emblem_voice(&self) -> f32 {
        self.voice
    }

    fn step(&mut self, state: &str, level: Option<&AudioLevel>, dt: f32) {
        self.step_with(state, level, dt, crate::tts::output_level());
    }

    /// `step` with Jarvis' playback level given (`tts::output_level()`, 0-1 of full scale).
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

/// What the emblem's voice ticks follow after a second of `state` with Jarvis' own playback at
/// `jarvis` (0-1) and Sir's microphone at `mic` (0-1): the reactor's own mapping, as the visual
/// verifier asks it. The microphone never reaches the emblem.
pub(crate) fn emblem_voice_of(state: &str, jarvis: f32, mic: f32) -> f32 {
    let level = AudioLevel { rms: mic, rms_db: -62.0 + 42.0 * mic.clamp(0.0, 1.0), probability: mic, noise_floor: 0.0, start_gate: 0.0, start_threshold: 0.0, release_threshold: 0.0, phase: "listening", speaking: mic > 0.5 };
    let mut reactor = Reactor::default();
    for _ in 0..60 {
        reactor.step_with(state, Some(&level), 1.0 / 60.0, jarvis / 6.0);
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
    let seen = ctx.data_mut(|d| std::mem::replace(d.get_temp_mut_or_insert_with(egui::Id::new("jarvis_layers_seen"), || 0usize), layers));
    if layers > seen {
        ctx.request_discard("a new area was measured");
    }
}

// ------------------------------------------------------------------ the node-graph core ----
// Jarvis' subsystems sit on a sphere (`orb_3d`), tied together by strings through the interior.
// The strings vibrate with Sir's voice and Jarvis' own, and signal particles run along them
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
            settings.reconcile(&c.interface);
            c.interface = settings;
        });
    }

    /// The fullscreen board on monitor `index` (1-based; None or a monitor that is gone: the
    /// primary one), from wherever the window is: spanning every monitor, fullscreen on
    /// another one, or a window. winit makes a window fullscreen on the monitor it is on, so
    /// the window is put there as a small plain window first.
    fn fullscreen_on(&mut self, ctx: &egui::Context, index: Option<usize>) {
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
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
        self.fullscreen_check = Instant::now();
        self.fullscreen_fixes = 0;
        info!("[HUD] Fullscreen board on {}", m.describe());
    }

    /// Applies interface changes (from Jarvis or the Interface window) to the live window.
    fn sync_interface(&mut self, ctx: &egui::Context) -> InterfaceSettings {
        let current = self.app.config().read(|c| c.interface.clone());
        // The HUD's colours (board, console, core, emblem) cross-fade to the chosen theme.
        if crate::theme_engine::follow(&current.hud_palette) {
            info!("[THEME] {}", current.hud_palette);
        }
        let fading = crate::theme_engine::fading();
        if fading {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        // Every window's colours follow the theme's fade (and Ultron Mode), not only the board.
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
            if prev.always_on_top != current.always_on_top {
                let level = if current.always_on_top {
                    egui::viewport::WindowLevel::AlwaysOnTop
                } else {
                    egui::viewport::WindowLevel::Normal
                };
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
            }
            // The window's three shapes: one board across every monitor, the fullscreen board on
            // one monitor, a (maximised) window. Each change of shape has one way through.
            let shape = |i: &InterfaceSettings| (i.span_all_monitors, i.fullscreen);
            if shape(&prev) != shape(&current) && !current.orb_mode {
                let target = lock(&HUD_MONITOR).take();
                if current.span_all_monitors {
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
                    // Back from spanning: Sir's first monitor (or the one he named). From a
                    // window: the monitor the window is on.
                    if prev.span_all_monitors || target.is_some() {
                        self.fullscreen_on(ctx, target);
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
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
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                }
            } else if let Some(index) = take_monitor_request(&current) {
                self.fullscreen_on(ctx, Some(index));
            }
            if prev.frameless != current.frameless {
                ctx.send_viewport_cmd(ViewportCommand::Decorations(!current.frameless));
            }
            if prev.title != current.title {
                ctx.send_viewport_cmd(ViewportCommand::Title(window_title(&current)));
            }
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            if prev.layout != current.layout && !current.fullscreen && !maximized {
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
                        crate::win32::run_detached(move || crate::win32::restore_placement(hwnd, rect));
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
    fn resize_grips(&self, ctx: &egui::Context) {
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

    /// The close button hides the window to the tray (Windows); Jarvis keeps running.
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
                    .on_hover_text("A custom rule Jarvis applies per answer (set by voice or chat)");
            }
            if cfg.voice_muted {
                pill(ui, &p, "voice muted", p.red).on_hover_text("Say \"unmute your voice\" to hear Jarvis again");
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
                    .on_hover_text("How long you must pause before Jarvis treats your sentence as finished");
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
                if slider(ui, &p, "Jarvis voice loudness", format!("{loudness:.0} %"), egui::Slider::new(&mut loudness, 0.0..=200.0).step_by(5.0)).changed() {
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

    /// Jarvis' contextual cards beside the orb, and what Sir clicks on them.
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
                // any other card's button goes to Jarvis as a request.
                PopupAction::Button(label) => match crate::proactive_butler::click(&label) {
                    Some(fix) => self.run_calls(vec![(fix.tool, fix.args)]),
                    None => self.command(HudCommand::Chat { text: label }),
                },
            }
        }
    }

    fn toasts(&self, ctx: &egui::Context, m: &HudModel) {
        if m.toasts.is_empty() {
            return;
        }
        let p = self.palette;
        // (At the bottom right of the home screen: the window's own corner is no monitor's on a
        // board that spans several.)
        let home = home_screen(ctx);
        egui::Area::new(egui::Id::new("hud_toasts"))
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(pos2(home.right() - 150.0, home.bottom() - (TELEMETRY_H + 30.0)))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for t in &m.toasts {
                    // Sharp technical notices, like the board's blocks.
                    let shown = Frame::none()
                        .fill(crate::hud_board::BLOCK_FILL)
                        .stroke(stroke(1.0, if t.error { p.red } else { crate::hud_board::border_colour() }))
                        .rounding(Rounding::ZERO)
                        .inner_margin(Margin::symmetric(13.0, 9.0))
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.label(RichText::new(if t.error { "ALERT //" } else { "NOTICE //" }).font(FontId::monospace(10.0)).color(if t.error { p.red } else { STRIP_MUTED }));
                            let colour = if t.error { blend(p.red, p.text, 0.45) } else { p.text };
                            ui.add(egui::Label::new(RichText::new(&t.text).size(ts(11.5)).color(colour)).wrap());
                        });
                    crate::hud_board::brackets(ui.painter(), shown.response.rect, crate::hud_board::bracket_colour());
                    ui.add_space(6.0);
                }
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        if self.confirm_shutdown {
            egui::Window::new("Shut down?")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Shut down J.A.R.V.I.S. completely?");
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
        }
        if let Some(name) = self.confirm_delete_skill.clone() {
            egui::Window::new("Delete skill?")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
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
        }
    }

    /// Manual counterpart of `configure_interface`.
    fn settings_window(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        let p = self.palette;
        let mut open = self.settings_open;
        let mut s = iface.clone();
        egui::Window::new(RichText::new("Interface").font(bold(14.0)).color(p.accent))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                muted(ui, &p, "Or just ask: “Jarvis, make the interface gold”, “hide the system panel”, “zoom in”, “stay on top”.");
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
                // The live layout state Jarvis controls by voice ("zen mode", "make the orb bigger").
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
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "J.A.R.V.I.S. IS SHUTTING DOWN", bold(16.0), p.accent);
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
const VIGNETTE: Color32 = Color32::from_rgb(0x07, 0x15, 0x22);
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
/// The COMMS block (what Sir said, what Jarvis answers) fades this long after the last word.
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

struct HudApp {
    app: Jarvis,
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
    /// Widgets and Jarvis' extra windows send their clicks through this.
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
    /// The COMMS block's lines: (Sir's line, Jarvis' line) and when each last changed.
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
    /// How tall the console's foot (the completions and the input line) was in the last frame:
    /// the log above it takes what is left, so the input line is never pushed out of the
    /// console when the completions appear or wrap.
    console_foot: f32,
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
    fn new(cc: &eframe::CreationContext<'_>, app: Jarvis, rt: Handle, model: Arc<Mutex<HudModel>>, restore: Option<[i32; 4]>) -> Self {
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
        }
        #[cfg(not(windows))]
        let hwnd = {
            let _ = restore;
            0
        };
        info!("[HUD] Native window open (HUD board)");
        crate::camera_feed::refresh_devices();
        Self::build(app, rt, model, hwnd)
    }

    /// The HUD's state for a window that exists (`new`) or for the offscreen HUD, which has
    /// none (`hwnd` 0: nothing here ever touches a native window then).
    fn build(app: Jarvis, rt: Handle, model: Arc<Mutex<HudModel>>, hwnd: isize) -> Self {
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
            window_seen: ([0; 4], None),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let (space, escape, focused) = ctx.input(|i| (i.key_down(Key::Space), i.key_pressed(Key::Escape), i.focused));
        if escape {
            // Esc closes the innermost thing first: the console, the spotlight, an overlay, then
            // Jarvis' speech.
            if self.find_open {
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
            } else {
                self.command(HudCommand::Interrupt);
            }
        }
        self.ptt_key = space && !typing && focused;
        // Ctrl+1/2/3 reply mode, Ctrl+M microphone, Ctrl+L type to Jarvis, Ctrl+B briefing.
        let ctrl = egui::Modifiers::COMMAND;
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(ctrl, key));
        for (key, mode) in [(Key::Num1, VoiceResponseMode::MirrorInput), (Key::Num2, VoiceResponseMode::AlwaysSpeak), (Key::Num3, VoiceResponseMode::TextOnly)] {
            if pressed(key) {
                self.command(HudCommand::SetResponseMode { mode });
            }
        }
        if pressed(Key::M) {
            let enabled = !self.app.config_state().mic_enabled;
            self.command(HudCommand::SetMic { enabled });
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
            let mut iface = self.app.config().read(|c| c.interface.clone());
            iface.fullscreen = !iface.fullscreen;
            iface.orb_mode = false;
            info!("[HUD] F11: {}", if iface.fullscreen { "fullscreen" } else { "windowed (maximised)" });
            self.set_interface(iface);
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
            let shape = if iface.orb_mode { "the floating orb" } else if iface.span_all_monitors { "one board across every monitor" } else if iface.fullscreen { "the fullscreen board" } else { "a window" };
            info!("[HUD] Window at {},{} {}×{} ({shape})", now[0], now[1], now[2], now[3]);
        }
    }

    /// The fullscreen board fills its monitor: `Fullscreen(true)` on the first frame, and every
    /// 1.5 s a check that the window really covers the monitor (re-applied when it does not).
    fn enforce_fullscreen(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        self.frames += 1;
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
        if self.frames == 1 {
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
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
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
    fn pure_orb(&mut self, ctx: &egui::Context, m: &HudModel, iface: &InterfaceSettings) {
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
            let canvas = ui.interact(ui.max_rect(), ui.id().with("hud_canvas"), Sense::click_and_drag());
            if canvas.drag_started_by(egui::PointerButton::Primary) && !iface.fullscreen && !iface.span_all_monitors {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if canvas.double_clicked() {
                let mut next = iface.clone();
                next.fullscreen = !iface.fullscreen;
                self.set_interface(next);
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
            // board (whose blocks are laid out around its corner) and when Sir tells it to get
            // out of the way; a window never moves it (windows open around it, and one Sir drags
            // over it simply covers it). The core sphere docks it on its own.
            let board_rects = crate::hud_board::visible_rects(area);
            self.centre_covered = iface.orb_aside || crate::hud_dock::centre_covered(screen, &board_rects);
            // The board fades out while the core sphere fades in over it.
            let core_t = ctx.animate_bool_with_time(egui::Id::new("hud_core_view"), self.core_open, 0.35);
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

            // The emblem (drawn over the board; its corner cell is kept free), wherever Sir put it.
            self.emblem.glide(screen, dt);
            self.emblem.paint(&ui.painter_at(screen), screen);
            let frame = self.emblem.frame(screen);
            // New windows open around the orb and the board's blocks, never on them.
            crate::hud_dock::publish_obstacles(Some(Rect::from_center_size(frame.centre, Vec2::splat(frame.radius * 2.3))), board_rects);
            let hit = Rect::from_center_size(frame.centre, vec2(frame.radius * 1.6, frame.radius * 1.6));
            let emblem = ui.interact(hit, ui.id().with("hud_emblem"), Sense::click_and_drag());
            // Drag the emblem anywhere on the board; it stays there (and windows open around it).
            if let Some(p) = emblem.interact_pointer_pos().filter(|_| emblem.dragged_by(egui::PointerButton::Primary)) {
                self.emblem.drag_to(vec2((p.x - screen.left()) / screen.width(), (p.y - screen.top()) / screen.height()));
                ctx.request_repaint();
            }
            if emblem.drag_stopped() {
                if let Some(f) = self.emblem.custom_home() {
                    let mut next = iface.clone();
                    next.orb_board_pos = Some([f.x, f.y]);
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

            if iface.comms_block {
                self.comms_block(ui, m, screen);
            }
            self.header_strip(ui, ctx, iface, screen);
            self.telemetry_strip(ui, m, screen, dt);
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
            history: self.history_open,
            console: self.console_open,
            rebooting,
            camera: self.camera_open,
            screens: self.screens_open,
            mic_muted: !self.app.config_state().mic_enabled || crate::listening_lock::is_locked(),
            ultron: crate::ultron_mode::active(),
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
                crate::tts::play_earcon(crate::jarvis_voice_dsp::Earcon::PowerDown);
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
            Some("ultron") => self.app.set_ultron(!crate::ultron_mode::active(), Trigger::Input(InputSource::TextGui)),
            Some("mic") => {
                // Muted (or "stop listening"): one click and he listens again.
                let unmute = lit.mic_muted;
                if crate::listening_lock::is_locked() {
                    crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
                }
                self.command(HudCommand::SetMic { enabled: unmute });
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
            if item(ui, "SHUT DOWN J.A.R.V.I.S.") {
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
        // `[CALL · LISTENING]` / `[CALL · TALKING]` while Jarvis is in Sir's Discord call.
        if let Some(call) = crate::discord_bridge::status_label() {
            let ink = crate::hud_board::active_colour();
            let galley = painter.layout_no_wrap(call.to_string(), strip_font(), ink);
            let rect = Rect::from_min_size(pos2(x, y), vec2(galley.size().x + 12.0, TELEMETRY_H));
            painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, ink));
            painter.galley(rect.center() - galley.size() / 2.0, galley, ink);
            ui.interact(rect, ui.id().with("hud_call"), Sense::hover()).on_hover_text("Jarvis hears the others in your Discord call (not your microphone). Say \"stop listening to the call\" to end it.");
            x = rect.right() + 14.0;
        }
        // What he said last, inline (fading out with the caption timer).
        if let Some(line) = m.messages.iter().rev().find(|l| l.who == Speaker::Sir && !l.faded) {
            let room = (screen.right() - x - crate::hud_dock::DOCK_INSET * 2.0 - 40.0).max(0.0);
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
    }

    /// `COMMS // LIVE RESPONSE`: the latest exchange as a sharp block in the band above the
    /// telemetry strip, so every reply is seen as well as heard (a text-only reply too). It fades
    /// `CAPTION_SECS` after the last word; the text console and HISTORY keep everything.
    fn comms_block(&mut self, ui: &mut Ui, m: &HudModel, screen: Rect) {
        let sir = m.messages.iter().rev().find(|l| l.who == Speaker::Sir && !l.faded).map(|l| l.text.clone()).unwrap_or_default();
        let jarvis = m.messages.iter().rev().find(|l| l.who == Speaker::Jarvis);
        let (reply, streaming, at) = jarvis.map(|l| (l.text.clone(), l.streaming, l.at.clone())).unwrap_or_default();
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
            self.console_at_edge = false;
            let mut builder = egui::ViewportBuilder::default().with_title("Text Console — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([CONSOLE_WIDTH, 640.0]).with_min_inner_size([320.0, 260.0]).with_decorations(false);
            if let Some((at, size)) = detached_placement(ctx, CONSOLE_ID) {
                builder = builder.with_position(at).with_inner_size(size);
            }
            ctx.show_viewport_immediate(ViewportId::from_hash_of("jarvis_text_console"), builder, |ctx, _| {
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
            let frame = Frame::none().fill(crate::hud_board::BLOCK_FILL).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO).inner_margin(Margin::same(12.0));
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
                crate::hud_board::brackets(&ctx.layer_painter(inner.response.layer_id), rect, crate::hud_board::bracket_colour());
                crate::hud_dock::publish_block(CONSOLE_ID, "Text console", rect);
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
        ui.add_space(8.0);
        let log_height = (ui.available_height() - self.console_foot).max(60.0);
        egui::ScrollArea::vertical().max_height(log_height).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            for line in m.messages.iter().filter(|l| !l.text.trim().is_empty()) {
                let (who, colour) = match line.who {
                    Speaker::Sir => ("USER", STRIP_MUTED),
                    Speaker::Jarvis => (crate::ultron_mode::console_name(), crate::hud_board::active_colour()),
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
        // own recent requests first, then Jarvis' command vocabulary). Tab takes the first.
        if self.completions.0 != self.console_input {
            let recent: Vec<String> = m.messages.iter().rev().filter(|l| l.who == Speaker::Sir).take(40).map(|l| l.text.clone()).collect();
            self.completions = (self.console_input.clone(), crate::command_palette::complete(&self.console_input, &recent, 4));
        }
        if !self.completions.1.is_empty() {
            let mut taken: Option<String> = None;
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab)) {
                taken = self.completions.1.first().cloned();
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                for (n, text) in self.completions.1.iter().enumerate() {
                    let chip = egui::Button::new(RichText::new(text).font(strip_font()).color(if n == 0 { crate::hud_board::active_colour() } else { STRIP_INK })).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO);
                    if ui.add(chip).on_hover_text(if n == 0 { "Tab completes the line with this" } else { "Click to use this" }).clicked() {
                        taken = Some(text.clone());
                    }
                }
            });
            if let Some(text) = taken {
                self.console_input = text;
                self.console_focus = true;
            }
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 150.0).max(120.0);
            let input = Frame::none()
                .stroke(stroke(1.0, crate::hud_board::border_colour()))
                .rounding(Rounding::ZERO)
                .inner_margin(Margin::symmetric(6.0, 5.0))
                .show(ui, |ui| ui.add(egui::TextEdit::singleline(&mut self.console_input).desired_width(width).font(FontId::proportional(14.0)).frame(false).hint_text("Type to Jarvis — English, Deutsch…")))
                .inner;
            if self.console_focus {
                input.request_focus();
                self.console_focus = false;
            }
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let transmit = ui
                .add(egui::Button::new(RichText::new("[ TRANSMIT -> ]").font(strip_font()).color(crate::hud_board::active_colour())).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO))
                .clicked();
            if enter || transmit {
                let text = self.console_input.trim().to_string();
                if !text.is_empty() {
                    act.send = Some(text);
                }
                // Enter keeps the line focused for the next command.
                self.console_focus = enter;
            }
        });
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
    /// Jarvis' live widgets. Returns whether anything is shown.
    fn system_column(&mut self, ctx: &egui::Context, m: &HudModel, screen: Rect) -> bool {
        let power: Vec<PowerCardView> = m.messages.iter().filter_map(|l| l.power.clone()).filter(|c| c.status == "pending").collect();
        let cards: Vec<MessageCardView> = m.messages.iter().filter_map(|l| l.card.clone()).filter(|c| c.status == "pending").collect();
        let clip = m.messages.iter().rev().take(6).find_map(|l| l.clip.clone());
        let widgets: Vec<crate::widgets::DynamicWidgetSpec> = self.app.tools().widgets.widgets().into_iter().filter(|w| w.dock == crate::widgets::Dock::Panel).collect();
        if power.is_empty() && cards.is_empty() && clip.is_none() && widgets.is_empty() {
            return false;
        }
        let p = self.palette;
        let top = screen.top() + 12.0 + HEADER_H + 14.0;
        egui::Area::new(egui::Id::new("hud_system_column"))
            .default_pos(pos2(screen.left() + 24.0, top))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.set_width(SYSTEM_COLUMN);
                egui::ScrollArea::vertical().max_height(screen.height() - top - 90.0).auto_shrink([false, true]).show(ui, |ui| {
                    for card in &power {
                        sharp_block(ui, "SYSTEM POWER // CONFIRMATION", "PENDING", |ui| self.power_card(ui, &p, card));
                    }
                    for card in &cards {
                        sharp_block(ui, "WHATSAPP // MESSAGE CARD", "PENDING", |ui| self.message_card(ui, &p, card));
                    }
                    if let Some(clip) = &clip {
                        sharp_block(ui, "INSTANT REPLAY // CLIP", &format!("{} S", clip.seconds), |ui| clip_card(ui, &p, clip));
                    }
                    for spec in &widgets {
                        let label = format!("WIDGET // {}", spec.title.to_uppercase());
                        sharp_block(ui, &label, "LIVE", |ui| widget_view::widget_card(ui, &p, spec, &self.widget_host, false));
                    }
                });
            });
        true
    }

    /// `[ HISTORY ]`: saved reports (click to put one on the board) and the conversation.
    fn history_overlay(&mut self, ctx: &egui::Context, m: &HudModel) {
        let screen = home_screen(ctx);
        let size = vec2(560.0f32.min(screen.width() - 48.0), (screen.height() * 0.72).max(240.0));
        let mut present: Option<i64> = None;
        let mut close = false;
        let mut open_window: Option<String> = None;
        egui::Area::new(egui::Id::new("hud_history"))
            .default_pos(pos2(screen.right() - 24.0 - size.x, screen.top() + 12.0 + HEADER_H + 14.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(size.x);
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
                                Speaker::Jarvis => ("J.A.R.V.I.S. //", crate::hud_board::active_colour()),
                                Speaker::System => ("SYSTEM //", STRIP_MUTED),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(who).font(strip_font()).color(colour));
                                let ink = if line.faded { STRIP_MUTED } else { STRIP_INK };
                                ui.add(egui::Label::new(RichText::new(&line.text).size(13.0).color(ink)).wrap());
                                if line.who == Speaker::Jarvis && ui.add(egui::Button::new(RichText::new("[ DETACH ]").font(strip_font()).color(STRIP_MUTED)).frame(false)).on_hover_text("Open in its own window").clicked() {
                                    open_window = Some(line.text.clone());
                                }
                            });
                            ui.add_space(3.0);
                        }
                    });
                });
            });
        if close {
            self.history_open = false;
        }
        if let Some(text) = open_window {
            self.command(HudCommand::OpenTextWindow { title: "J.A.R.V.I.S.".into(), text });
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
        egui::Area::new(egui::Id::new("hud_camera"))
            .default_pos(pos2(screen.center().x - 200.0, screen.top() + 12.0 + HEADER_H + 14.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
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
                self.run_calls(vec![("move_window_to_monitor", serde_json::json!({ "app_or_window_name": "jarvis", "target_monitor": n }))]);
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
            muted(ui, &p, "Or just ask: “Jarvis, go fullscreen”, “orb mode”, “clear the board”.");
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
                    self.run_tool("open_jarvis_view", serde_json::json!({ "view": view }));
                }
            }
            V::TextConsole => {
                self.console_open = if toggle { !self.console_open } else { true };
                self.console_focus = self.console_open;
            }
        }
    }

    /// Dialogs, the settings and hub windows, Jarvis' extra windows, push-to-talk and the frame
    /// rate: everything after the HUD's own canvas.
    fn after_body(&mut self, ctx: &egui::Context, iface: &InterfaceSettings) {
        let iface = iface.clone();
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
        // Jarvis' tool views: ROSE blocks inside the HUD, or their own OS windows when detached.
        crate::hud_dock::begin_frame();
        widget_view::show_windows(ctx, &self.widget_host);
        // The Simulation Lab's windows (60 FPS while they animate).
        simlab_view::show_simulations(ctx, &self.widget_host);
        // The phone pairing window (QR code), when open.
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
        mixer_view::show(ctx, &self.rt);
        // The network radar, the disk map and the Earth, when open.
        radar_view::show(ctx, &self.rt);
        disk_map_view::show(ctx, &self.rt);
        earth_view::show(ctx, &self.rt);
        // The spatial workspace: floating glass windows.
        crate::workspace_canvas::show(ctx, &self.app, &self.rt);
        self.workout.show(ctx, &self.palette, &self.app, &self.rt);

        let ptt = self.ptt_button || self.ptt_key;
        if ptt != self.ptt_sent {
            self.ptt_sent = ptt;
            self.command(HudCommand::Ptt { active: ptt });
        }

        // While something moves (the emblem unfolds or docks, blocks cascade, Jarvis listens,
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
        let fps = target_fps(iface.animations, active, attended, display_hz());
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
    let origin = ctx.input(|i| i.viewport().inner_rect.map(|r| r.min))?;
    let block = crate::hud_dock::block_rect(id)?;
    Some((origin + block.min.to_vec2(), block.size()))
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
    history: bool,
    console: bool,
    rebooting: bool,
    camera: bool,
    screens: bool,
    /// The microphone is off or the privacy lock holds ("stop listening").
    mic_muted: bool,
    ultron: bool,
}

fn header_items(lit: &HeaderState, reboot: &str, source: &str) -> Vec<HeaderItem> {
    let item = |id: &'static str, text: &str, on: bool, boxed: bool| HeaderItem { id, text: text.to_string(), on, boxed };
    vec![
        item("theme", "THEME", false, true),
        item("ultron", "ULTRON", lit.ultron, true),
        item("phone", "PHONE QR", lit.phone, true),
        item("core", "CORE", lit.core, true),
        item("modules", "MODULES", false, true),
        item("features", "FEATURES", lit.features, true),
        item("history", "HISTORY", lit.history, true),
        item("view", "VIEW", false, true),
        item("clear", "CLEAR VIEW", false, true),
        item("console", "CONSOLE", lit.console, true),
        item("reboot", reboot, lit.rebooting, true),
        item("camera", "CAMERA", lit.camera, true),
        item("source_label", "CAMERA SOURCE", false, false),
        item("source", source, false, true),
        item("screens", "SCREENS", lit.screens, true),
        item("mic", if lit.mic_muted { "MIC: MUTED" } else { "MIC: ON" }, lit.mic_muted, true),
    ]
}

/// `[ MODULES ]`: each Stark module and the tool calls (name, JSON arguments) that open it.
const MODULES: &[(&str, &[(&str, &str)])] = &[
    ("HOLOMAT — MATH · PHYSICS · CHEM LAB", &[("open_jarvis_view", r#"{"view": "simulation_lab"}"#)]),
    ("CHRONOS — WEBUNTIS · ITSLEARNING · CALENDAR", &[("open_jarvis_view", r#"{"view": "schedule"}"#)]),
    ("MEDIA & AUDIO MIXER — PLAYLISTS · APP VOLUMES", &[("control_audio_mixer", r#"{"action": "list"}"#), ("control_music", r#"{"action": "list_playlists"}"#)]),
    ("SENTINEL & AEGIS — TELEMETRY · VRAM · SERVERS", &[("open_jarvis_view", r#"{"view": "system_stats"}"#), ("server_deck", "{}")]),
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

/// Frame-time diagnostics: a frame over 120 ms is logged (at most every 5 s), and with
/// `JARVIS_LOG=jarvis=debug` the frame rate every 2 s.
fn frame_stats(took: Duration) {
    let mut clock = lock(&FRAME_CLOCK);
    if took > Duration::from_millis(120) && clock.last_slow.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
        clock.last_slow = Some(Instant::now());
        warn!("[HUD] Slow frame: {} ms", took.as_millis());
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

/// The HUD's own work per frame over its last frames: (frames, average ms, worst ms). None
/// before the first frame.
pub fn frame_times() -> Option<(usize, f64, f64)> {
    let clock = lock(&FRAME_CLOCK);
    let n = clock.recent.len();
    (n > 0).then(|| (n, clock.recent.iter().map(|ms| *ms as f64).sum::<f64>() / n as f64, clock.recent.iter().copied().fold(0.0f32, f32::max) as f64))
}

/// What Sir did with an in-app block this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockAction {
    pub close: bool,
    pub detach: bool,
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

/// `#050A10`: the canvas of an in-app tool block.
const BLOCK_SLATE: Color32 = Color32::from_rgb(0x05, 0x0A, 0x10);

/// A borderless detached window's frame: the theme's 1 px border and corner brackets (Windows
/// draws no grey frame of its own around a Jarvis window).
pub(super) fn paint_window_border(ctx: &egui::Context) {
    let rect = ctx.screen_rect().shrink(0.5);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("jarvis_window_border")));
    painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, crate::hud_board::border_colour()));
    crate::hud_board::brackets(&painter, rect, crate::hud_board::bracket_colour());
}

/// The top row of a borderless detached window: its title (drag it to move the window) and
/// `[ X ]`; true when Sir closed it.
pub(super) fn detached_title_bar(ui: &mut Ui, ctx: &egui::Context, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        let label = ui.add(egui::Label::new(RichText::new(title.to_uppercase()).font(strip_font()).color(crate::hud_board::active_colour())).sense(Sense::drag()));
        if label.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui
                .add(egui::Button::new(RichText::new("[ CLOSE ]").font(strip_font()).color(STRIP_INK)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO))
                .clicked();
        });
    });
    close
}

/// A tool view as a ROSE block inside the main HUD — the default for every view (Simulation Lab,
/// telemetry, schedule, the QR link, Jarvis' markdown windows): `#050A10` slate, a 1 px
/// `#2B7A9E` frame, white L-corner brackets, the title on the left and `[ DETACH ]` `[ X ]`
/// on the right. Movable and resizable on the board; its rect is published so the emblem knows
/// whether the centre is covered (`hud_dock::block_rects`).
pub(super) fn hud_block_window(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], body: impl FnOnce(&mut Ui)) -> BlockAction {
    let mut action = BlockAction::default();
    // "Close the feature lab": the block fades out, then leaves through its own close path.
    let fade = crate::hud_dock::closing_progress(id);
    if fade.is_some_and(|t| t >= 1.0) {
        action.close = true;
        return action;
    }
    let alpha = 1.0 - fade.unwrap_or(0.0);
    if fade.is_some() {
        ctx.request_repaint();
    }
    // The block's screen: on a spanned HUD the monitor it stands on (the one under the pointer
    // while Sir drags it across), so no block straddles a bezel or sits where no screen shows.
    let regions = block_regions(ctx);
    let dragged = crate::hud_dock::grabbed(id).and_then(|_| ctx.input(|i| i.pointer.primary_down().then(|| i.pointer.interact_pos()).flatten()));
    let anchor = dragged.or_else(|| crate::hud_dock::block_rect(id).map(|r| r.center()));
    let mut bounds = anchor.map_or(regions[0], |at| region_at(&regions, at));
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
    let fresh = ctx.memory(|m| m.area_rect(area)).is_none_or(|old| (fits && !bounds.expand(2.0).contains_rect(old)) || (!crate::hud_dock::is_open(id) && crate::hud_dock::collides(id, old)));
    let held = crate::hud_dock::take_forced(id).or_else(|| fresh.then(|| new_block_rect(ctx, id, &regions, size + frame_pad)));
    // A place the dock gave on another monitor makes that monitor the block's screen.
    if let Some(r) = held {
        bounds = region_at(&regions, r.center());
    }
    let (at, size) = held.map_or((bounds.min, size), |r| (r.min, (r.size() - frame_pad).max(Vec2::splat(40.0))));
    // The search across the blocks: a block that answers it is lit, the others step back.
    let needle = crate::hud_dock::search();
    let found = !needle.is_empty() && crate::hud_dock::search_matches(&needle, title, &crate::hud_dock::block_content(id).unwrap_or_default());
    let alpha = alpha * if needle.is_empty() || found { 1.0 } else { SEARCH_DIM };
    let (pinned, maximized) = (crate::hud_dock::is_pinned(id), crate::hud_dock::is_maximized(id));
    let edge = if found { crate::hud_board::active_colour() } else { crate::hud_board::border_colour() };
    let frame = Frame::none()
        .fill(BLOCK_SLATE.gamma_multiply(alpha))
        .stroke(stroke(if found { 2.0 } else { 1.0 }, edge.gamma_multiply(alpha)))
        .rounding(Rounding::ZERO)
        .inner_margin(Margin::same(BLOCK_MARGIN));
    let window = egui::Window::new(title).id(area).title_bar(false).collapsible(false).resizable(true).frame(frame).default_pos(at).default_size(size).constrain_to(bounds);
    let window = match held {
        Some(r) => window.current_pos(r.min).min_size(size).max_size(size),
        None => window.min_size(Vec2::from(crate::hud_dock::MIN_BLOCK) - frame_pad),
    };
    let shown = window.show(ctx, |ui| {
        if alpha < 1.0 {
            ui.multiply_opacity(alpha);
        }
        // The block keeps the size it was given (or dragged to), however short its content.
        ui.set_min_size(ui.available_size());
        ui.horizontal(|ui| {
            // Not selectable: a drag that begins on the title moves the block (selectable text
            // would take the drag for a selection).
            ui.add(egui::Label::new(RichText::new(title.to_uppercase()).font(strip_font()).color(crate::hud_board::active_colour())).truncate().selectable(false))
                .on_hover_text("Drag to move, drag an edge to resize. Right-click: copy, pin, maximize, export, close. Ctrl+W close, Ctrl+G maximize, Ctrl+P pin, Ctrl+E export.");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let chip = |ui: &mut Ui, text: &str, hint: &str| {
                    ui.add(egui::Button::new(RichText::new(text).font(strip_font()).color(STRIP_INK)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO))
                        .on_hover_text(hint)
                        .clicked()
                };
                if chip(ui, "[ CLOSE ]", "Close (Ctrl+W closes the block under the pointer)") {
                    action.close = true;
                }
                if chip(ui, "[ DETACH ]", "Open in its own window (say \"dock all windows\" to bring it back)") {
                    action.detach = true;
                }
                if pinned {
                    ui.add(egui::Label::new(RichText::new("PINNED").font(strip_font()).color(crate::hud_board::active_colour())).selectable(false))
                        .on_hover_text("Stays through \"close the windows\" and keeps its place. Right-click to unpin.");
                }
            });
        });
        let r = ui.cursor();
        ui.painter().line_segment([pos2(r.left(), r.top()), pos2(r.left() + ui.available_width(), r.top())], stroke(1.0, crate::hud_board::border_colour()));
        ui.add_space(6.0);
        body(ui);
    });
    if let Some(inner) = shown {
        let rect = inner.response.rect;
        crate::hud_board::brackets(&ctx.layer_painter(inner.response.layer_id), rect, crate::hud_board::bracket_colour());
        if crate::hud_dock::publish_block(id, title, rect) {
            crate::tts::play_cue(crate::jarvis_voice_dsp::Earcon::Materialize);
        }
        // Moved with the mouse, a block stays exactly under the pointer: where it stood when
        // the button went down plus the whole way the pointer has come since. (egui's own
        // window move begins a few pixels late and rounds every frame.) A resize or a drag
        // inside the block's content is left alone: only a block that egui is moving is taken.
        let (pressed, down, released, origin, at) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.press_origin(), i.pointer.interact_pos()));
        if pressed {
            if let Some(origin) = origin.filter(|o| rect.contains(*o)) {
                crate::hud_dock::grab(id, rect, origin);
            }
        } else if let Some((home, origin)) = crate::hud_dock::grabbed(id).filter(|_| down || released) {
            let moved = (rect.min - home.min).length() > 0.5 && (rect.size() - home.size()).length() <= 0.5;
            if let Some(at) = at.filter(|_| moved) {
                crate::hud_dock::hold_once(id, Rect::from_min_size(home.min + (at - origin), home.size()));
                ctx.request_repaint();
            }
        }
        if !down {
            crate::hud_dock::let_go(id);
        }
        // Ctrl+W / Ctrl+M / Ctrl+P on the block under the pointer; right-click: the block menu.
        let mut chosen = inner.response.contains_pointer().then(|| block_shortcut(ctx)).flatten();
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
                        crate::tts::play_cue(crate::jarvis_voice_dsp::Earcon::Spotlight);
                    }
                }
                Menu::ExportMarkdown => export(Format::Markdown),
                Menu::ExportJson => export(Format::Json),
                Menu::Close => action.close = true,
            }
            ctx.request_repaint();
        }
    }
    // Closed by hand (the chip, the menu, Ctrl+W): the block leaves with its cue.
    if action.close {
        crate::tts::play_cue(crate::jarvis_voice_dsp::Earcon::Dismiss);
    }
    action
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

/// An in-app block with a plain text body, exactly as Jarvis' own Markdown windows are framed
/// (the visual verifier fills the board with these). Its text scrolls when it is longer than
/// the block; `content` is what `Copy Content` and the search read. True when it was closed.
pub(crate) fn text_block(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], content: &str) -> bool {
    crate::hud_dock::describe_block(id, content);
    hud_block_window(ctx, id, title, size, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(content).size(13.5).color(STRIP_INK)).wrap());
        });
    })
    .close
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
fn sharp_block<R>(ui: &mut Ui, label: &str, tag: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    let shown = Frame::none()
        .fill(crate::hud_board::BLOCK_FILL)
        .stroke(stroke(1.0, crate::hud_board::border_colour()))
        .rounding(Rounding::ZERO)
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(label).font(strip_font()).color(STRIP_MUTED)).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(tag).font(strip_font()).color(STRIP_MUTED)));
            });
            ui.add_space(4.0);
            body(ui)
        });
    crate::hud_board::brackets(ui.painter(), shown.response.rect, crate::hud_board::bracket_colour());
    ui.add_space(10.0);
    shown.inner
}

/// The obsidian canvas: `#020408` with a dark cyan-slate radial vignette in the centre
/// (`#071522` fading to black at the edges) and the 28 px dot matrix at 6 %.
pub(crate) fn paint_canvas(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    painter.rect_filled(rect, Rounding::ZERO, OBSIDIAN);
    let mut mesh = egui::Mesh::default();
    let home = home_screen(ctx);
    let centre = home.center();
    let reach = home.size() * 0.75;
    let mid = blend(VIGNETTE, OBSIDIAN, 0.55);
    mesh.colored_vertex(centre, VIGNETTE);
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
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // A panic anywhere in a frame costs that frame, never the HUD (the next one redraws).
        let started = Instant::now();
        if crate::crash_guard::guard("HUD frame", || self.frame(ctx)).is_none() {
            self.last_frame = Instant::now();
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        frame_stats(started.elapsed());
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
        self.enforce_fullscreen(ctx, &iface);
        self.sync_orb_window(ctx, &iface);
        self.note_window(ctx, &iface);
        self.handle_close(ctx);
        self.handle_keys(ctx);
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

        // The HUD's text scale (Jarvis' extra windows set their own while they draw).
        TEXT_SCALE.with(|s| s.set(iface.font_scale));
        let screen = ctx.screen_rect();
        crate::interface::publish_layout(iface.layout_state((screen.width() * iface.scale, screen.height() * iface.scale)));
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if iface.orb_mode {
            self.pure_orb(ctx, &m, &iface);
        } else {
            paint_canvas(ctx);
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
            self.canvas(ctx, &m, &iface, dt);
            self.toasts(ctx, &m);
            self.popups(ctx, dt);
            self.room_alert_overlay(ctx, &m);
            // (Neither the fullscreen board nor the one across the monitors is resized by hand.)
            if iface.frameless && !iface.fullscreen && !iface.span_all_monitors && !maximized {
                self.resize_grips(ctx);
            }
        }
        if m.offline {
            self.offline_overlay(ctx);
        }
        drop(m);
        self.after_body(ctx, &iface);
        settle_new_areas(ctx);
        self.last_frame = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ["THEME", "ULTRON", "PHONE QR", "CORE", "MODULES", "FEATURES", "HISTORY", "VIEW", "CLEAR VIEW", "CONSOLE", "REBOOT LLM", "CAMERA", "CAMERA SOURCE", "DEFAULT CAMERA", "SCREENS", "MIC: ON"]
        );
        assert!(items.iter().filter(|i| !i.boxed).map(|i| i.id).eq(["source_label"]), "only the source caption is unboxed");
        let on = |items: &[HeaderItem], id: &str| items.iter().find(|i| i.id == id).unwrap().on;
        assert!(on(&items, "camera") && !on(&items, "console"), "the camera box lights while the camera runs");
        let lit = header_items(&HeaderState { console: true, phone: true, core: true, features: true, ..HeaderState::default() }, "X", "X");
        assert!(["console", "phone", "core", "features"].iter().all(|id| on(&lit, id)), "a box lights while its panel is open");
        let muted = header_items(&HeaderState { mic_muted: true, ultron: true, ..HeaderState::default() }, "X", "X");
        assert!(muted.iter().any(|i| i.id == "mic" && i.text == "MIC: MUTED" && i.on) && on(&muted, "ultron"));
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
        assert!(row.hidden.starts_with(&["theme", "ultron"]), "{:?}", row.hidden);
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
            }
        }
        // Wide screens keep the familiar centred 820 px block.
        assert_eq!(comms_placement(Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1440.0)), 0.0), (820.0, 1280.0));
    }

    #[test]
    fn the_emblem_follows_jarvis_voice_and_never_sirs_microphone() {
        // Repro 2026-11-03: `mic.max(voice)` made the orb pulse while Sir talked.
        let sir = Reactor { mic: 0.8, voice: 0.0, ..Default::default() };
        let jarvis = Reactor { mic: 0.1, voice: 0.6, ..Default::default() };
        assert_eq!((sir.emblem_voice(), jarvis.emblem_voice()), (0.0, 0.6));
    }
}
