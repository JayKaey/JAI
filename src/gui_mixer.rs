//! The Mixer block ("open the mixer"): a fader and a mute key for the master volume and for every
//! app with a channel, read off the real mixer twice a second while the block is open. A fader
//! goes through `audio_mixer::slide` (a drag is a hundred changes a second), a mute key through
//! `control_audio_mixer`, the way a spoken request goes.

use super::{rgba, stroke, strip_font, STRIP_INK, STRIP_MUTED};
use crate::audio_mixer::Channel;
use eframe::egui::{self, Frame, Margin, RichText, Rounding, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tracing::warn;

/// The block's id on the board.
const ID: &str = "audio_mixer";
const TITLE: &str = "Mixer // every channel";
/// How often the channels are read while the block is open.
const READ_EVERY: Duration = Duration::from_millis(500);
/// A fader stays where his hand left it this long (the device takes a moment to say so itself).
const HELD_FOR: Duration = Duration::from_millis(900);
const FADER_WIDTH: f32 = 240.0;
/// The mute key: "[ MUTED ]" in the strip's font.
const KEY_SIZE: [f32; 2] = [74.0, 22.0];

static OPEN: AtomicBool = AtomicBool::new(false);
/// A reading is under way.
static READING: AtomicBool = AtomicBool::new(false);
/// What the last reading found (and his hand changed since).
static CHANNELS: Mutex<Vec<Channel>> = Mutex::new(Vec::new());
static LAST_READ: Mutex<Option<Instant>> = Mutex::new(None);
static LAST_TOUCH: Mutex<Option<Instant>> = Mutex::new(None);

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open() {
    // (Read at once: a fader from the last time it was open is not where the sound is now.)
    *lock(&LAST_READ) = None;
    *lock(&LAST_TOUCH) = None;
    OPEN.store(true, Ordering::Release);
}

/// Closes the block.
pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// The channels are read again when that is due and no reading is under way.
fn refresh(rt: &Handle) {
    if lock(&LAST_READ).is_some_and(|at| at.elapsed() < READ_EVERY) || READING.swap(true, Ordering::AcqRel) {
        return;
    }
    *lock(&LAST_READ) = Some(Instant::now());
    rt.spawn(async {
        match crate::audio_mixer::channels().await {
            Ok(channels) if !lock(&LAST_TOUCH).is_some_and(|at| at.elapsed() < HELD_FOR) => *lock(&CHANNELS) = channels,
            Ok(_) => {}
            Err(e) => warn!("[MIXER] The block could not read the channels: {e}"),
        }
        READING.store(false, Ordering::Release);
        super::request_repaint();
    });
}

/// One channel: its name, its fader, its level and its mute key.
fn row(ui: &mut Ui, channel: &mut Channel, rt: &Handle) {
    let touched = || *lock(&LAST_TOUCH) = Some(Instant::now());
    ui.label(RichText::new(channel.label.to_uppercase()).font(strip_font()).color(STRIP_INK));
    ui.spacing_mut().slider_width = FADER_WIDTH;
    let mut level = f32::from(channel.percent);
    if ui.add(egui::Slider::new(&mut level, 0.0..=100.0).show_value(false)).changed() {
        channel.percent = level.round() as u8;
        touched();
        let (target, percent) = (channel.target.clone(), channel.percent);
        rt.spawn(async move { crate::audio_mixer::slide(&target, percent).await });
    }
    ui.label(RichText::new(format!("{:>3} %", channel.percent)).font(strip_font()).color(STRIP_MUTED));
    let (text, ink) = if channel.muted { ("[ MUTED ]", crate::hud_board::active_colour()) } else { ("[ MUTE ]", STRIP_INK) };
    let key = egui::Button::new(RichText::new(text).font(strip_font()).color(ink)).fill(rgba([3, 7, 12], 0.9)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO);
    if ui.add_sized(KEY_SIZE, key).clicked() {
        channel.muted = !channel.muted;
        touched();
        let args = if channel.is_master() {
            json!({ "action": if channel.muted { "mute_master" } else { "unmute_master" } })
        } else {
            json!({ "action": if channel.muted { "mute_app" } else { "unmute_app" }, "app": channel.target })
        };
        rt.spawn(async move {
            if let Err(e) = crate::audio_mixer::tool(&args).await {
                warn!("[MIXER] The mute key failed: {e}");
            }
        });
    }
    ui.end_row();
}

fn body(ui: &mut Ui, rt: &Handle) {
    refresh(rt);
    ui.ctx().request_repaint_after(READ_EVERY);
    let mut channels = lock(&CHANNELS);
    if channels.is_empty() {
        ui.label(RichText::new("READING THE MIXER").font(strip_font()).color(STRIP_MUTED));
        return;
    }
    // (A block the dock made narrower scrolls sideways instead of squeezing its faders.)
    egui::ScrollArea::both().auto_shrink([false; 2]).show(ui, |ui| rows(ui, &mut channels, rt));
}

fn rows(ui: &mut Ui, channels: &mut [Channel], rt: &Handle) {
    egui::Grid::new("mixer_channels").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
        for channel in channels.iter_mut() {
            row(ui, channel, rt);
        }
    });
}

/// Registers the mixer while it is open (a block on the board, or its own window once detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    if !OPEN.load(Ordering::Acquire) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, [560.0, 320.0], |ui| body(ui, rt));
        if action.close {
            OPEN.store(false, Ordering::Release);
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Mixer — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([580.0, 340.0]).with_min_inner_size([480.0, 200.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jarvis_audio_mixer"), builder, move |ctx, class| {
        crate::crash_guard::guard("mixer", || {
            if ctx.input(|i| i.viewport().close_requested()) || !OPEN.load(Ordering::Acquire) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Mixer").show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    fn channel(target: &str, label: &str, percent: u8, muted: bool) -> Channel {
        Channel { target: target.to_string(), label: label.to_string(), percent, muted }
    }

    #[test]
    fn every_channel_is_shown_whole() {
        let runtime = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
        let mut channels = vec![
            channel("master", "Master", 100, false),
            channel("discord", "Discord", 5, true),
            channel("applicationframehost", "Application Frame Host", 40, false),
        ];
        let mut stage = Stage::new(egui::vec2(760.0, 420.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("mixer").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(640.0, 320.0)).show(ctx, |ui| rows(ui, &mut channels, runtime.handle()));
        });
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for shown in ["MASTER", "DISCORD", "100 %", "5 %", "[ MUTED ]", "[ MUTE ]"] {
            assert!(!frame.find_text(shown).is_empty(), "{shown} is not on the block");
        }
    }
}
