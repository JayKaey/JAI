//! The Hotkeys window ("open the hotkeys window", the settings panel's HOTKEYS, the admin
//! window's tile): every key of his that works in any program and in games. SET, then the new
//! key, changes one (Esc, or another window in front, gives up); CLEAR takes it away; a key
//! another program holds shows as taken. `hotkeys.rs` keeps and registers them.

use super::{strip_ink, strip_muted};
use crate::config::ConfigStore;
use crate::holo::Face;
use crate::hotkeys::{self, Act};
use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The window's id among his views.
const ID: &str = "hotkeys";
const TITLE: &str = "Hotkeys // every key of his";
static OPEN: AtomicBool = AtomicBool::new(false);
/// The key that waits for a new one (SET pressed), and the line under the keys.
static STATE: Mutex<(Option<Act>, String)> = Mutex::new((None, String::new()));

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open() {
    OPEN.store(true, Ordering::Release);
}

/// Closes the window (a key that waited for a new one keeps the one it had).
pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
    wait_for(None);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Waits for a new key for `act` (None: for none); his keys are let go meanwhile, so one of them
/// can be pressed here.
fn wait_for(act: Option<Act>) {
    lock(&STATE).0 = act;
    #[cfg(windows)]
    if act.is_some() {
        // (What is held now is no new key.)
        let _ = newly_down();
    }
    hotkeys::pause(act.is_some());
}

/// The key that went down since the last look, with the modifiers held: read off the keyboard
/// itself, so keys egui has no name for count too (Pause, Scroll Lock, the number pad). A key
/// held at the first look counts once it was let go and pressed again.
#[cfg(windows)]
fn newly_down() -> Option<(u32, u32)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    static DOWN: Mutex<[bool; 256]> = Mutex::new([false; 256]);
    // SAFETY: reads a key's state; nothing is handed over.
    let down = |vk: u32| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
    // (Shift, Ctrl, Alt, the Windows keys, their left and right twins, Caps and Num Lock.)
    let modifier = |vk: u32| matches!(vk, 0x10..=0x12 | 0x14 | 0x5B | 0x5C | 0x90 | 0xA0..=0xA5);
    let mut before = lock(&DOWN);
    let mut found = None;
    for vk in 0x08..=0xFE {
        let now = down(vk);
        if now && !before[vk as usize] && !modifier(vk) && found.is_none() {
            found = Some(vk);
        }
        before[vk as usize] = now;
    }
    let mods = [(0x11, hotkeys::CTRL), (0x12, hotkeys::ALT), (0x10, hotkeys::SHIFT), (0x5B, hotkeys::WIN), (0x5C, hotkeys::WIN)].into_iter().filter(|(vk, _)| down(*vk)).fold(0, |m, (_, bit)| m | bit);
    found.map(|vk| (mods, vk))
}

/// The key pressed in the window this frame: (modifiers, virtual key; 0 for one he cannot have).
fn pressed_now(ui: &Ui) -> Option<(u32, u32)> {
    #[cfg(windows)]
    let pressed = {
        let _ = ui;
        newly_down()
    };
    #[cfg(not(windows))]
    let pressed = ui.input(|i| {
        i.events.iter().find_map(|event| match event {
            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => Some((0, hotkeys::ESCAPE)),
            egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
                let mods = [(modifiers.ctrl, hotkeys::CTRL), (modifiers.alt, hotkeys::ALT), (modifiers.shift, hotkeys::SHIFT)].into_iter().filter(|(on, _)| *on).fold(0, |m, (_, bit)| m | bit);
                Some((mods, hotkeys::vk_of(key.name()).unwrap_or(0)))
            }
            _ => None,
        })
    });
    pressed
}

/// While a key waits for a new one: the key pressed is taken, or the line says why not. Esc, or
/// another window in front, ends the wait.
fn capture(ui: &Ui, config: &ConfigStore, act: Act) {
    ui.ctx().request_repaint();
    if ui.ctx().input(|i| i.viewport().focused) == Some(false) {
        wait_for(None);
        return;
    }
    let Some((mods, vk)) = pressed_now(ui) else { return };
    if vk == hotkeys::ESCAPE {
        lock(&STATE).1.clear();
        wait_for(None);
        return;
    }
    match config.read(|c| hotkeys::check(c, act, mods, vk)) {
        Ok(key) => {
            hotkeys::set(config, act, &key);
            wait_for(None);
            lock(&STATE).1 = format!("{key}: {}.", act.name().to_lowercase());
            // (Whether another program holds it is known once it was registered.)
            ui.ctx().request_repaint_after(Duration::from_millis(150));
        }
        Err(why) => lock(&STATE).1 = why,
    }
}

/// The hint, a row for each key (what it does, its key, SET and CLEAR) and the line under them.
/// What was pressed: (its key, SET rather than CLEAR).
fn table(ui: &mut Ui, keys: &[Vec<String>; 4], waiting: Option<Act>, taken: &[Act], note: &str) -> Option<(Act, bool)> {
    let theme = crate::holo::theme();
    ui.label(RichText::new("These keys work in every program and in games. SET, then press the new key.").font(crate::holo::font(Face::Body, 13.0)).color(strip_muted()));
    ui.add_space(12.0);
    let mut pressed = None;
    egui::Grid::new("hotkeys").num_columns(4).spacing([16.0, 12.0]).show(ui, |ui| {
        for (act, keys) in Act::ALL.into_iter().zip(keys) {
            ui.label(RichText::new(act.name().to_uppercase()).font(crate::holo::font(Face::Label, 9.0)).color(strip_ink()));
            let (shown, colour) = match (waiting == Some(act), keys.is_empty(), taken.contains(&act)) {
                (true, ..) => ("PRESS THE NEW KEY".to_string(), theme.accent()),
                (_, true, _) => ("NONE".to_string(), strip_muted()),
                (_, _, true) => (format!("{} · TAKEN", keys.join(", ").to_uppercase()), theme.alert()),
                _ => (keys.join(", ").to_uppercase(), strip_ink()),
            };
            ui.label(RichText::new(shown).font(crate::holo::font(Face::Figure, 13.0)).color(colour));
            if crate::holo_ui::key_on(ui, "SET", waiting == Some(act)).clicked() {
                pressed = Some((act, true));
            }
            if !keys.is_empty() && crate::holo_ui::key(ui, "CLEAR").clicked() {
                pressed = Some((act, false));
            }
            ui.end_row();
        }
    });
    if !note.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new(note).font(crate::holo::font(Face::Body, 13.0)).color(strip_ink()));
    }
    pressed
}

fn body(ui: &mut Ui, config: &ConfigStore) {
    let waiting = lock(&STATE).0;
    if let Some(act) = waiting {
        capture(ui, config, act);
    }
    let (waiting, note) = lock(&STATE).clone();
    let keys = config.read(|c| Act::ALL.map(|act| hotkeys::keys_of(c, act)));
    match table(ui, &keys, waiting, &hotkeys::taken(), &note) {
        // (SET again: it keeps its key.)
        Some((act, true)) => {
            lock(&STATE).1.clear();
            wait_for((waiting != Some(act)).then_some(act));
        }
        Some((act, false)) => {
            wait_for(None);
            hotkeys::set(config, act, "");
            lock(&STATE).1 = format!("No key for {} now.", act.name().to_lowercase());
        }
        None => {}
    }
}

/// The Hotkeys window while it is open (a window of its own; a block on the board in a session
/// without windows).
pub(super) fn show(ctx: &egui::Context, config: &ConfigStore) {
    let viewport = ViewportId::from_hash_of("jai_hotkeys");
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, viewport, OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [620.0, 330.0], |ui| body(ui, config)).close {
            close();
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Hotkeys — J.A.I.").with_app_id("jai").with_inner_size([640.0, 350.0]).with_min_inner_size([560.0, 300.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    let config = config.clone();
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("hotkeys", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx)) {
                close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Hotkeys").show(ctx, |ui| body(ui, &config));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        close();
                    }
                    body(ui, &config);
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

    /// Every key whole, as it starts, while one waits for a new key, and with one taken.
    #[test]
    fn every_key_of_his_is_shown_whole() {
        let mut c = JaiConfig::default();
        let _look = crate::holo_theme::look_for_pictures();
        for (picture, waiting, taken, note) in [
            ("panel_hotkeys", None, vec![], ""),
            ("panel_hotkeys_waiting", Some(Act::Mute), vec![], "Ctrl+Alt+J is already his key for \"His windows to the front\"."),
            ("panel_hotkeys_taken", None, vec![Act::Mute], "Ctrl+Shift+M: microphone on / off."),
        ] {
            if picture == "panel_hotkeys_taken" {
                c.hotkeys.mute = "Ctrl+Shift+M".into();
            }
            let keys = Act::ALL.map(|act| hotkeys::keys_of(&c, act));
            let mut stage = Stage::new(egui::vec2(720.0, 400.0));
            let frame = stage.frames(4, &mut |ctx| {
                egui::Window::new("hotkeys").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(620.0, 330.0)).show(ctx, |ui| {
                    table(ui, &keys, waiting, &taken, note);
                });
            });
            stage.raster(&frame).save(picture);
            assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{picture}");
            assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{picture}");
            let mut shown = vec!["MICROPHONE ON / OFF", "HIS WINDOWS TO THE FRONT", "THE CONSOLE TO TYPE TO HIM", "WAKE HIM FROM STANDBY", "CTRL+ALT+J", "CTRL+ALT+K", "ALT+J", "SET", "CLEAR"];
            shown.push(match picture {
                "panel_hotkeys" => "NONE",
                "panel_hotkeys_waiting" => "PRESS THE NEW KEY",
                _ => "CTRL+SHIFT+M · TAKEN",
            });
            for text in shown {
                assert!(!frame.find_text(text).is_empty(), "{text} is not in {picture}");
            }
        }
    }
}
