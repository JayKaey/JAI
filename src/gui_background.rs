//! The background window ("open the background window", "change my background", the settings
//! panel's row, the admin window's tile): his own pictures, videos and slideshows on the board's
//! black (`bg_media`), one for every monitor or one for each, chosen with the system's open
//! dialog instead of said aloud; and how they stand: behind the board or the whole background,
//! with their sound or without, how far they are dimmed, how fast a slideshow turns.

use super::{strip_ink, strip_muted};
use crate::bg_media::{self, ALL};
use crate::config::ConfigStore;
use crate::holo::{self, Face};
use crate::holo_ui;
use crate::interface::InterfaceSettings;
use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// The window's id among his views.
const ID: &str = "background";
const TITLE: &str = "Background // pictures and videos behind me";
static OPEN: AtomicBool = AtomicBool::new(false);
/// The open dialog's answer, from its own thread: the monitor it is for, and what was chosen.
static CHOSEN: Mutex<Option<(String, Result<Option<PathBuf>, String>)>> = Mutex::new(None);
/// Whether a dialog is open, and the line under the controls (and whether it is a fault).
static STATE: Mutex<(bool, String, bool)> = Mutex::new((false, String::new(), false));
/// The width of a source's name.
const NAME_W: f32 = 130.0;

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open() {
    OPEN.store(true, Ordering::Release);
}

pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Changes the background as `configure_interface` would (`bg_media::apply`): what came of it
/// stands under the controls.
fn change(config: &ConfigStore, args: Value) {
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    config.update(|c| bg_media::apply(&mut c.interface, &args, &mut done, &mut failed));
    let line = |text: &str| crate::kit_voice::capitalised(text.trim_end_matches('.')) + ".";
    let mut state = lock(&STATE);
    (state.1, state.2) = match failed.first() {
        Some(fault) => (line(fault), true),
        None => (line(&done.join(", ")), false),
    };
}

/// Opens the system's dialog for monitor `key`'s source (`folder`: a slideshow's) on a thread of
/// its own; its answer is taken in by the next frame.
fn ask(key: &str, folder: bool) {
    let mut state = lock(&STATE);
    if state.0 {
        return;
    }
    state.0 = true;
    let key = key.to_string();
    std::thread::spawn(move || {
        let chosen = bg_media::choose(folder);
        *lock(&CHOSEN) = Some((key, chosen));
        lock(&STATE).0 = false;
        super::request_repaint();
    });
}

/// What monitor `key` shows, in a few words.
fn source_words(s: &InterfaceSettings, key: &str) -> String {
    match s.bg_media.get(key) {
        Some(path) => Path::new(path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned()),
        None if key == ALL => "black".into(),
        None => "as every monitor".into(),
    }
}

fn label(text: &str) -> RichText {
    RichText::new(text).font(holo::font(Face::Body, 13.0)).color(strip_ink())
}

/// The window's inside (`monitors`: each monitor's key and name, every monitor first).
fn body(ui: &mut Ui, config: &ConfigStore, monitors: &[(String, String)]) {
    if let Some((key, chosen)) = lock(&CHOSEN).take() {
        match chosen {
            Ok(Some(path)) => change(config, json!({ "background_media": path.display().to_string(), "background_monitor": key })),
            Ok(None) => {}
            Err(fault) => *lock(&STATE) = (false, crate::kit_voice::capitalised(&fault) + ".", true),
        }
    }
    let s = config.read(|c| c.interface.clone());
    let (asking, said, fault) = lock(&STATE).clone();
    ui.label(RichText::new("Your own pictures and videos on my black, one for every monitor or one for each. Local files only; a video plays as H.264 in MP4, a folder of pictures is a slideshow.").font(holo::font(Face::Body, 12.0)).color(strip_muted()));
    ui.add_space(4.0);
    holo_ui::head(ui, "WHAT");
    for (key, name) in monitors {
        ui.horizontal(|ui| {
            ui.add_sized([NAME_W, 22.0], egui::Label::new(label(name)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if s.bg_media.contains_key(key) && holo_ui::key_alert(ui, "NONE").clicked() {
                    change(config, json!({ "background_media": "none", "background_monitor": key }));
                }
                if holo_ui::key(ui, "FOLDER").clicked() && !asking {
                    ask(key, true);
                }
                if holo_ui::key(ui, "FILE").clicked() && !asking {
                    ask(key, false);
                }
                let shown = source_words(&s, key);
                let colour = if s.bg_media.contains_key(key) { holo::theme().accent() } else { strip_muted() };
                ui.add(egui::Label::new(RichText::new(shown).font(holo::font(Face::Body, 13.0)).color(colour)).truncate());
            });
        });
    }
    ui.add_space(6.0);
    holo_ui::head(ui, "HOW");
    ui.horizontal(|ui| {
        let whole = s.bg_mode == "whole";
        if holo_ui::key_on(ui, "BEHIND THE BOARD", !whole).clicked() && whole {
            change(config, json!({ "background_mode": "behind" }));
        }
        if holo_ui::key_on(ui, "THE WHOLE BACKGROUND", whole).clicked() && !whole {
            change(config, json!({ "background_mode": "whole" }));
        }
    });
    ui.label(RichText::new(if s.bg_mode == "whole" { "The media alone; the board comes when you speak to me or say \"show the board\"." } else { "The board stands over the media, which is dimmed so the board stays readable." }).font(holo::font(Face::Body, 12.0)).color(strip_muted()));
    ui.add_space(4.0);
    let mut sound = s.bg_sound;
    if holo_ui::toggle(ui, &mut sound, "Videos with their sound").changed() {
        change(config, json!({ "background_sound": sound }));
    }
    ui.horizontal(|ui| {
        ui.add_sized([NAME_W, 22.0], egui::Label::new(label("Dimmed")));
        let mut dim = (s.bg_dim * 100.0).round();
        if ui.add(holo_ui::Slider::new(&mut dim, 0.0..=90.0).step_by(1.0).suffix(" %")).changed() {
            change(config, json!({ "background_dim": dim }));
        }
    });
    ui.horizontal(|ui| {
        ui.add_sized([NAME_W, 22.0], egui::Label::new(label("Slideshow")));
        let mut every = s.bg_interval;
        if ui.add(holo_ui::Number::new(&mut every).range(3..=3600).suffix(" s a picture")).changed() {
            change(config, json!({ "background_interval": every }));
        }
    });
    ui.add_space(6.0);
    let (line, colour) = if asking { ("The dialog is open: choose there.".to_string(), strip_ink()) } else { (said, if fault { holo::theme().alert() } else { strip_ink() }) };
    if !line.is_empty() {
        ui.label(RichText::new(line).font(holo::font(Face::Body, 13.0)).color(colour));
    }
}

/// Every monitor first, then each by its number (as `bg_media` keys them).
fn monitors() -> Vec<(String, String)> {
    let mut all = vec![(ALL.to_string(), "Every monitor".to_string())];
    all.extend(crate::monitors::list().into_iter().map(|m| (m.index.to_string(), format!("Monitor {}{}", m.index, if m.primary { " · main" } else { "" }))));
    all
}

/// The background window while it is open (a window of its own; a block on the board in a
/// session without windows).
pub(super) fn show(ctx: &egui::Context, config: &ConfigStore) {
    let viewport = ViewportId::from_hash_of("jai_background_window");
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, viewport, is_open()) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [620.0, 470.0], |ui| body(ui, config, &monitors())).close {
            close();
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Background — J.A.I.").with_app_id("jai").with_inner_size([640.0, 500.0]).with_min_inner_size([540.0, 420.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    let config = config.clone();
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("background", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!is_open() && !super::detached_leaving(ctx)) {
                close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            let monitors = monitors();
            if class == ViewportClass::Embedded {
                egui::Window::new("Background").show(ctx, |ui| body(ui, &config, &monitors));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        close();
                    }
                    body(ui, &config, &monitors);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::JaiConfig;
    use crate::visual_ui_verifier::{self as visual, Stage};

    /// A picture for every monitor, a slideshow on the second, the board over them: every line
    /// whole; and what the window changes is what `configure_interface` would.
    #[test]
    fn the_background_window_shows_each_monitor_and_changes_it() {
        let _look = crate::holo_theme::look_for_pictures();
        let dir = std::env::temp_dir().join(format!("jai_bg_window_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let picture = dir.join("sunset.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 120, 40, 255])).save(&picture).unwrap();
        let config = ConfigStore::new(JaiConfig::default());
        let monitors = vec![(ALL.to_string(), "Every monitor".to_string()), ("1".to_string(), "Monitor 1 · main".to_string()), ("2".to_string(), "Monitor 2".to_string())];
        change(&config, json!({ "background_media": picture.display().to_string(), "background_monitor": ALL }));
        assert_eq!(config.read(|c| c.interface.bg_media.get(ALL).cloned()), Some(picture.display().to_string()));
        assert!(!lock(&STATE).2, "{}", lock(&STATE).1);
        change(&config, json!({ "background_media": dir.display().to_string(), "background_monitor": "2" }));
        assert!(config.read(|c| c.interface.bg_media.contains_key("2")), "a folder is a slideshow");
        change(&config, json!({ "background_dim": 40.0, "background_mode": "whole" }));
        assert_eq!(config.read(|c| (c.interface.bg_mode.clone(), (c.interface.bg_dim * 100.0).round())), ("whole".to_string(), 40.0));
        change(&config, json!({ "background_media": dir.join("nothing.png").display().to_string(), "background_monitor": "1" }));
        assert!(lock(&STATE).2, "a file that is not there is said so");
        *lock(&STATE) = (false, "Background the picture sunset.png on every monitor.".into(), false);
        let mut stage = Stage::new(egui::vec2(680.0, 540.0));
        let frame = stage.frames(4, &mut |ctx| {
            // Not "background": that is egui's own background layer's id.
            egui::Window::new("background window").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(620.0, 470.0)).show(ctx, |ui| body(ui, &config, &monitors));
        });
        stage.raster(&frame).save("panel_background");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for text in ["WHAT", "HOW", "Every monitor", "Monitor 2", "sunset.png", "as every monitor", "FILE", "FOLDER", "NONE", "THE WHOLE BACKGROUND", "Videos with their sound", "Dimmed", "Slideshow"] {
            assert!(!frame.find_text(text).is_empty(), "{text} is not in the background window");
        }
        *lock(&STATE) = (false, String::new(), false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
