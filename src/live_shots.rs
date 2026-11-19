//! `JAI_SHOTS=<folder>` in a test session (`--silent --show`): every window of JAI's own
//! saves what it shows as a PNG every second — the real frames, 3D and globe included — so a
//! scripted session's UI can be looked at afterwards. Only JAI's windows, never the screen.

use crate::crash_guard::lock;
use eframe::egui::{self, ViewportCommand, ViewportId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_millis(1000);

fn folder() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(std::env::var_os("JAI_SHOTS")?);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_ref()
    .filter(|_| crate::platform::in_test_session())
}

static ASKED: LazyLock<Mutex<HashMap<ViewportId, Instant>>> = LazyLock::new(Default::default);

/// Called once per frame by every viewport: saves the shots that arrived, asks for the next.
pub fn tick(ctx: &egui::Context) {
    let Some(dir) = folder() else { return };
    let started = START.get_or_init(Instant::now);
    ctx.input(|i| {
        for event in &i.raw.events {
            if let egui::Event::Screenshot { viewport_id, image } = event {
                let name = format!("{:06}_{:016x}.png", started.elapsed().as_millis(), viewport_id.0.value());
                // Encoded and written off the GUI thread: the PNG of a whole screen takes several
                // hundred milliseconds, and done here it froze the very frames the session was
                // run to look at (the "0.9 s freeze when a block opens" of 2026-11-11 was this).
                let (image, path) = (std::sync::Arc::clone(image), dir.join(name));
                std::thread::spawn(move || {
                    let [w, h] = image.size;
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    save(path, &bytes, w as u32, h as u32);
                });
            }
        }
    });
    let viewport = ctx.viewport_id();
    let mut asked = lock(&ASKED);
    if asked.get(&viewport).is_none_or(|at| at.elapsed() >= EVERY) {
        asked.insert(viewport, Instant::now());
        if viewport == ViewportId::ROOT {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot);
        } else {
            read_back(ctx, dir.join(format!("{:06}_{:016x}.png", started.elapsed().as_millis(), viewport.0.value())));
        }
    }
    ctx.request_repaint_after(EVERY);
}

fn save(path: PathBuf, rgba: &[u8], w: u32, h: u32) {
    if let Err(e) = image::save_buffer(path, rgba, w, h, image::ExtendedColorType::Rgba8) {
        tracing::warn!("[SHOTS] Not saved: {e}");
    }
}

/// One of his other windows: egui hands no picture to a window drawn inside the board's frame
/// (every view of `gui::block`, the board's pieces, the console), so what the window has
/// painted is read back from its frame buffer, by a callback drawn last, over everything.
fn read_back(ctx: &egui::Context, path: PathBuf) {
    use eframe::glow::{self, HasContext};
    let callback = eframe::egui_glow::CallbackFn::new(move |info, painter| {
        let [w, h] = info.screen_size_px;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        // SAFETY: a read of the bound frame buffer into a buffer of exactly its size.
        unsafe { painter.gl().read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(&mut pixels)) };
        let path = path.clone();
        std::thread::spawn(move || {
            // (OpenGL's rows run from the bottom up; a see-through window is shown on black.)
            let mut rows: Vec<u8> = pixels.chunks_exact((w * 4) as usize).rev().flatten().copied().collect();
            rows.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
            save(path, &rows, w, h);
        });
    });
    ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::new("jai_shots"))).add(egui::PaintCallback { rect: ctx.screen_rect(), callback: std::sync::Arc::new(callback) });
}

static START: OnceLock<Instant> = OnceLock::new();
