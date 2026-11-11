//! The HUD without a window: the real `HudApp` drawn into a private `egui::Context` that no
//! display ever sees.
//!
//! `JAI.exe --live-test --headless` and `--exhaustive` run the whole interface this way. The
//! same `HudApp::frame` the window runs is stepped here, at any resolution, with a simulated
//! pointer and keyboard; what it paints comes back as shapes (`visual_ui_verifier::Frame`)
//! instead of pixels on a monitor. Tools that open windows work as they do with the real HUD
//! (the offscreen context is the HUD's context for as long as it runs), detached OS windows
//! are embedded as in-app windows by egui, and nothing here ever touches a native window:
//! the HUD is built without a window handle.
//!
//! One thread owns the HUD. It draws an idle frame about thirty times a second, as the window
//! does at rest, so everything that waits for the HUD to draw (a new block publishing its rect,
//! a fade finishing) proceeds; in between it runs the jobs the verifier sends it, each with
//! the HUD to itself.

use super::*;
use crate::visual_ui_verifier::Frame as Shapes;

/// A piece of work for the offscreen HUD's thread.
type Job = Box<dyn FnOnce(&mut VirtualHud) + Send>;

/// The idle rate while no job runs (the window's 30 FPS at rest).
const IDLE_FRAME: Duration = Duration::from_millis(33);

static JOBS: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

/// The HUD drawn offscreen.
pub(crate) struct VirtualHud {
    hud: HudApp,
    ctx: egui::Context,
    size: Vec2,
    /// The virtual desktop position of the window (monitor regions are measured against it).
    origin: Pos2,
    /// Seconds since the first frame, as egui sees them.
    clock: f64,
    last: Instant,
    pointer: Option<Pos2>,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    /// How long the HUD's own work took in the last frame (layout and painting into shapes,
    /// without the wait before it).
    cost: Duration,
}

impl VirtualHud {
    fn new(app: Jai, rt: Handle, size: Vec2) -> Self {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        // Its blocks are measured where they rest (the verifier's own row plays the entrance and
        // the exit: `set_still`).
        set_still(&ctx, true);
        configure_style(&ctx, app.config().read(|c| c.interface.font_scale));
        crate::theme_engine::attach_database(&app.config().read(|c| c.memory_db_path.clone()));
        let model = Arc::new(Mutex::new(HudModel::default()));
        // The offscreen context is the HUD's context: repaint requests and the "is a HUD up"
        // question reach it exactly as they reach the window's.
        *lock(&SHARED.ctx) = Some(ctx.clone());
        feed_model(&app, &rt, &model);
        SHARED.running.store(true, Ordering::Release);
        let hud = HudApp::build(app, rt, model, 0);
        info!("[HUD] Offscreen HUD up at {}x{} (no window)", size.x, size.y);
        Self { hud, ctx, size, origin: Pos2::ZERO, clock: 0.0, last: Instant::now(), pointer: None, events: Vec::new(), modifiers: egui::Modifiers::NONE, cost: Duration::ZERO }
    }

    pub(crate) fn size(&self) -> Vec2 {
        self.size
    }

    pub(crate) fn screen(&self) -> Rect {
        Rect::from_min_size(Pos2::ZERO, self.size)
    }

    /// Blocks drawn at rest (`true`, the default here) or with their entrance and exit, as in
    /// the window.
    pub(crate) fn animate_windows(&mut self, on: bool) {
        set_still(&self.ctx, !on);
    }

    /// Whether one of the HUD's own surfaces is open: "console", "core", "history", "screens",
    /// "find", "features".
    pub(crate) fn surface_open(&self, name: &str) -> bool {
        match name {
            "console" => self.hud.console_open,
            "core" => self.hud.core_open,
            "history" => self.hud.history_open,
            "screens" => self.hud.screens_open,
            "find" => self.hud.find_open,
            "features" => features_view::is_open(),
            _ => false,
        }
    }

    /// Another resolution (and where the window sits on the virtual desktop).
    pub(crate) fn resize(&mut self, size: Vec2, origin: Pos2) {
        self.size = size;
        self.origin = origin;
    }

    /// The emblem's centre and radius on screen right now.
    pub(crate) fn emblem(&self) -> (Pos2, f32) {
        let frame = self.hud.emblem.frame(self.home());
        (frame.centre, frame.radius)
    }

    /// The HUD's home screen as of its last frame: the window, or the primary monitor's part of
    /// a board that spans several.
    pub(crate) fn home(&self) -> Rect {
        home_screen(&self.ctx)
    }

    /// Where in-app blocks may stand: one region, or one per monitor of a spanned board.
    pub(crate) fn block_regions(&self) -> Vec<Rect> {
        block_regions(&self.ctx)
    }

    /// Each monitor's part of the window while it reaches onto several (else nothing).
    pub(crate) fn monitor_parts(&self) -> Vec<Rect> {
        monitor_regions(&self.ctx)
    }

    /// The emblem's state name ("AwakeFrontEmblem", "ThinkingGyroscope", "CornerDocked" …).
    pub(crate) fn emblem_state(&self) -> String {
        format!("{:?}", self.hud.emblem.state())
    }

    /// Opens or closes one of the HUD's own surfaces by name, as its header button would:
    /// "console", "core", "history", "screens", "find", "features".
    pub(crate) fn set_surface(&mut self, name: &str, open: bool) {
        match name {
            "console" => {
                self.hud.console_open = open;
                self.hud.console_focus = open;
            }
            "core" => self.hud.core_open = open,
            "history" => self.hud.history_open = open,
            "screens" => self.hud.screens_open = open,
            "find" => {
                self.hud.find_open = open;
                if !open {
                    self.hud.find_text.clear();
                    crate::hud_dock::set_search("");
                }
            }
            "features" => {
                if features_view::is_open() != open {
                    features_view::toggle();
                }
            }
            _ => {}
        }
    }

    /// Text into the console's input line (the completions follow it).
    pub(crate) fn type_in_console(&mut self, text: &str) {
        self.hud.console_input = text.to_string();
    }

    /// What stands in the console's input line.
    pub(crate) fn console_line(&self) -> String {
        self.hud.console_input.clone()
    }

    /// The Feature Lab's `[ TRY ]` for a feature a HUD control drives: opens that control.
    pub(crate) fn open_control(&mut self, id: &str) {
        open_control(id);
    }

    /// The Feature Lab's `[ TRY ]` for a feature that waits for Sir's words: the example request
    /// in the console's input line.
    pub(crate) fn stage_in_console(&mut self, text: &str) {
        prefill_console(text.to_string());
    }

    /// Closes the HUD's own controls again (the SCREENS mover, the Theme Studio, the console).
    pub(crate) fn close_controls(&mut self) {
        self.hud.screens_open = false;
        self.hud.console_open = false;
        self.hud.console_input.clear();
        theme_studio::close();
        mixer_view::close();
    }

    /// Whether the control `id` is open ("screens", "theme_studio", "mixer").
    pub(crate) fn control_open(&self, id: &str) -> bool {
        match id {
            "screens" => self.hud.screens_open,
            "theme_studio" => theme_studio::is_open(),
            "mixer" => mixer_view::is_open(),
            _ => false,
        }
    }

    /// Where in-app blocks live on the home screen.
    pub(crate) fn block_bounds(&self) -> Rect {
        block_bounds(self.home())
    }

    /// What lies under `at`, for a finding's message: the topmost layer there.
    pub(crate) fn layer_at(&self, at: Pos2) -> String {
        self.ctx.layer_id_at(at).map_or_else(|| "nothing".to_string(), |layer| format!("{layer:?}"))
    }

    /// The board's area as the HUD laid it out in its last frame (right of the widgets column
    /// when Sir has widgets there, left of the console while it is out).
    pub(crate) fn board_area(&self) -> Rect {
        if self.hud.board_area_now.is_positive() { self.hud.board_area_now } else { board_area(self.home(), 0.0, self.hud.console_reserve()) }
    }

    /// The console's completions for its current line.
    pub(crate) fn completions(&self) -> Vec<String> {
        self.hud.completions.1.clone()
    }

    // -------------------------------------------------------------------------- input ----

    pub(crate) fn move_pointer(&mut self, to: Pos2) {
        self.pointer = Some(to);
        self.events.push(egui::Event::PointerMoved(to));
    }

    pub(crate) fn press(&mut self, button: egui::PointerButton, down: bool) {
        if let Some(pos) = self.pointer {
            self.events.push(egui::Event::PointerButton { pos, button, pressed: down, modifiers: self.modifiers });
        }
    }

    pub(crate) fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        for pressed in [true, false] {
            self.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        }
    }

    pub(crate) fn pointer_gone(&mut self) {
        self.pointer = None;
        self.events.push(egui::Event::PointerGone);
    }

    // ------------------------------------------------------------------------- frames ----

    /// One frame of the real HUD, `dt` seconds after the last one (waited in real time too: the
    /// board's cascade and the window fades run on the wall clock). What it painted.
    pub(crate) fn frame(&mut self, dt: f32) -> Shapes {
        let wait = Duration::from_secs_f32(dt.clamp(0.0, 0.25)).saturating_sub(self.last.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        let step = self.last.elapsed().as_secs_f64();
        self.last = Instant::now();
        self.clock += step;
        let screen = self.screen();
        let mut input = egui::RawInput { screen_rect: Some(screen), time: Some(self.clock), predicted_dt: step.max(1.0 / 240.0) as f32, events: std::mem::take(&mut self.events), modifiers: self.modifiers, focused: true, ..Default::default() };
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.inner_rect = Some(Rect::from_min_size(self.origin, self.size));
        viewport.outer_rect = viewport.inner_rect;
        viewport.focused = Some(true);
        viewport.fullscreen = Some(true);
        let hud = &mut self.hud;
        let working = Instant::now();
        let output = self.ctx.run(input, |ctx| {
            // A panic inside a frame costs that frame, as in the window.
            if crate::crash_guard::guard("offscreen HUD frame", || hud.frame(ctx)).is_none() {
                hud.last_frame = Instant::now();
            }
        });
        self.cost = working.elapsed();
        // (The same frame clock as the window's: the self-test's frame-time probe reads it.)
        frame_stats(self.cost);
        crate::visual_ui_verifier::capture(output, screen)
    }

    /// The HUD's own work in the last frame, in milliseconds.
    pub(crate) fn frame_cost_ms(&self) -> f64 {
        self.cost.as_secs_f64() * 1000.0
    }

    /// `count` frames at `fps`; the last one's shapes.
    pub(crate) fn frames_at(&mut self, count: usize, fps: f32) -> Shapes {
        let mut last = self.frame(1.0 / fps);
        for _ in 1..count {
            last = self.frame(1.0 / fps);
        }
        last
    }

    /// A click at `at` (pointer there, button down, button up, one more frame for the result).
    pub(crate) fn click(&mut self, at: Pos2, button: egui::PointerButton) -> Shapes {
        self.move_pointer(at);
        self.frame(1.0 / 60.0);
        self.press(button, true);
        self.frame(1.0 / 60.0);
        self.press(button, false);
        self.frame(1.0 / 60.0);
        self.frame(1.0 / 60.0)
    }

    /// A drag with the primary button from `from` by `delta`, in `steps` frames.
    pub(crate) fn drag(&mut self, from: Pos2, delta: Vec2, steps: usize) -> Shapes {
        self.move_pointer(from);
        self.frame(1.0 / 60.0);
        self.press(egui::PointerButton::Primary, true);
        self.frame(1.0 / 60.0);
        let steps = steps.max(2);
        for n in 1..=steps {
            self.move_pointer(from + delta * (n as f32 / steps as f32));
            self.frame(1.0 / 60.0);
        }
        self.press(egui::PointerButton::Primary, false);
        self.frame(1.0 / 60.0);
        self.frame(1.0 / 60.0)
    }
}

/// Starts the offscreen HUD on its own thread (once). `size`: its resolution in points.
pub(crate) fn start(app: Jai, rt: Handle, size: Vec2) {
    let mut jobs = lock(&JOBS);
    if jobs.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<Job>();
    *jobs = Some(tx);
    drop(jobs);
    let spawned = std::thread::Builder::new().name("offscreen-hud".into()).stack_size(16 * 1024 * 1024).spawn(move || {
        let mut hud = VirtualHud::new(app, rt, size);
        loop {
            match rx.recv_timeout(IDLE_FRAME) {
                Ok(job) => job(&mut hud),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    hud.frame(0.0);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if SHARED.quitting.load(Ordering::Acquire) {
                break;
            }
        }
        SHARED.running.store(false, Ordering::Release);
        lock(&SHARED.ctx).take();
    });
    if let Err(e) = spawned {
        warn!("[HUD] The offscreen HUD could not start: {e}");
        lock(&JOBS).take();
    }
}

/// Whether the HUD runs offscreen in this process.
pub(crate) fn is_running() -> bool {
    lock(&JOBS).is_some() && SHARED.running.load(Ordering::Acquire)
}

/// Runs `job` on the offscreen HUD's thread with the HUD to itself; its result, or None when no
/// offscreen HUD runs (or the job panicked).
pub(crate) async fn with<R: Send + 'static>(job: impl FnOnce(&mut VirtualHud) -> R + Send + 'static) -> Option<R> {
    let sender = lock(&JOBS).clone()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let boxed: Job = Box::new(move |hud| {
        if let Some(result) = crate::crash_guard::guard("offscreen HUD job", || job(hud)) {
            let _ = tx.send(result);
        }
    });
    sender.send(boxed).ok()?;
    rx.await.ok()
}
