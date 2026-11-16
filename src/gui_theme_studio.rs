//! The Theme Studio (`[ + CREATE THEME ]` in the THEME menu, or `[ TRY ]` on the interface
//! features): Sir picks Core Primary, Core Secondary, Window Border and Background Tint (an RGB /
//! HSV picker, or a hex code), names the theme, and `[ SAVE THEME ]` stores it
//! (`config/custom_themes.json` and SQLite) and applies it.

use super::{rgba, stroke, strip_font, STRIP_INK, STRIP_MUTED};
use crate::interface::InterfaceSettings;
use crate::theme_engine::CustomTheme;
use eframe::egui::{self, Frame, Margin, RichText, Rounding, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// The block's id on the board.
const ID: &str = "theme_studio";
/// The width its colour rows need (label, picker, hex field, HSV read-out).
const MIN_WIDTH: f32 = 440.0;
/// The hex field: "#00f0ff" in the monospace face, with room for the cursor.
const HEX_WIDTH: f32 = 76.0;

static OPEN: AtomicBool = AtomicBool::new(false);
/// What Sir is designing (from the active theme when the studio opens).
static DRAFT: Mutex<Option<CustomTheme>> = Mutex::new(None);
/// A theme `[ SAVE THEME ]` handed over; the HUD saves and applies it on its frame.
static SAVED: Mutex<Option<CustomTheme>> = Mutex::new(None);

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open() {
    *lock(&DRAFT) = None;
    OPEN.store(true, Ordering::Release);
}

/// Closes the studio without saving.
pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// One colour: its picker (RGB / HSV), its hex code (applied once it parses) and its HSV.
fn colour_row(ui: &mut Ui, label: &str, rgb: &mut [u8; 3]) {
    ui.label(RichText::new(label).font(strip_font()).color(STRIP_INK));
    egui::color_picker::color_edit_button_srgb(ui, rgb);
    let id = ui.id().with(("theme_hex", label));
    let mut hex = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| crate::interface::hex(*rgb));
    // (Its full width, whatever the grid offers: a grid gives a cell the width its column had in
    // the frame before, 40 px in the first, and a field that takes what it is offered stayed
    // 40 px wide for good, with the hex code cut off.)
    let field = ui.add_sized(egui::vec2(HEX_WIDTH, ui.spacing().interact_size.y), egui::TextEdit::singleline(&mut hex).font(egui::TextStyle::Monospace));
    if field.changed() {
        if let Some(c) = crate::interface::parse_color(hex.trim()).filter(|_| hex.trim().len() >= 7) {
            *rgb = c;
        }
    }
    if !field.has_focus() {
        hex = crate::interface::hex(*rgb);
    }
    ui.data_mut(|d| d.insert_temp(id, hex));
    let (h, s, v) = crate::theme_engine::to_hsv(*rgb);
    ui.label(RichText::new(format!("H {h:.0}°  S {:.0}%  V {:.0}%", s * 100.0, v * 100.0)).font(strip_font()).color(STRIP_MUTED));
    ui.end_row();
}

fn body(ui: &mut Ui) {
    let mut draft = lock(&DRAFT);
    let Some(t) = draft.as_mut() else { return };
    ui.horizontal(|ui| {
        ui.label(RichText::new("NAME").font(strip_font()).color(STRIP_INK));
        ui.add(egui::TextEdit::singleline(&mut t.name).desired_width(220.0).hint_text("Cyberpunk"));
    });
    ui.add_space(6.0);
    egui::Grid::new("theme_studio_colours").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
        colour_row(ui, "CORE PRIMARY", &mut t.core_primary);
        colour_row(ui, "CORE SECONDARY", &mut t.core_secondary);
        colour_row(ui, "WINDOW BORDER", &mut t.window_border);
        colour_row(ui, "BACKGROUND TINT", &mut t.background_tint);
    });
    // A preview: the tint, a border and the core's two colours.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(360.0), 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, Rounding::ZERO, super::rgb(t.background_tint));
    painter.rect_stroke(rect.shrink(0.5), Rounding::ZERO, stroke(1.0, super::rgb(t.window_border)));
    painter.circle_filled(rect.left_center() + egui::vec2(20.0, 0.0), 10.0, super::rgb(t.core_primary));
    painter.circle_stroke(rect.left_center() + egui::vec2(20.0, 0.0), 14.0, stroke(1.5, super::rgb(t.core_secondary)));
    ui.add_space(8.0);
    let save = egui::Button::new(RichText::new("[ SAVE THEME ]").font(strip_font()).color(STRIP_INK))
        .fill(rgba([3, 7, 12], 0.9))
        .stroke(stroke(1.0, crate::hud_board::border_colour()))
        .rounding(Rounding::ZERO);
    if ui.add_enabled(!t.name.trim().is_empty(), save).on_hover_text("Saves it under its name and applies it").clicked() {
        t.name = t.name.trim().to_string();
        *lock(&SAVED) = Some(t.clone());
        super::request_repaint();
    }
}

/// Registers the studio while it is open (a block on the board, or its own window once
/// detached); the theme Sir saved this frame, if any.
pub(super) fn show(ctx: &egui::Context, iface: &InterfaceSettings) -> Option<CustomTheme> {
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, ViewportId::from_hash_of("jai_theme_studio"), OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget(ID);
        return lock(&SAVED).take();
    }
    lock(&DRAFT).get_or_insert_with(|| CustomTheme::from_palette("Custom", &crate::theme_engine::current(), iface.background_rgb()));
    if !crate::hud_dock::adopt(ID) {
        // (Its rows keep their width: in a block the dock made narrower, the studio scrolls
        // sideways instead of squeezing its hex fields shut.)
        let action = super::hud_block_window(ctx, ID, "Theme Studio // create theme", [560.0, 300.0], |ui| {
            crate::hud_style::faded(ui, egui::ScrollArea::both().auto_shrink([false; 2]), |ui| {
                ui.set_min_width(MIN_WIDTH);
                body(ui);
            });
        });
        if action.close {
            OPEN.store(false, Ordering::Release);
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return lock(&SAVED).take();
    }
    let mut builder = ViewportBuilder::default().with_title("Theme Studio — J.A.I.").with_app_id("jai").with_inner_size([580.0, 330.0]).with_min_inner_size([460.0, 280.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jai_theme_studio"), builder, move |ctx, class| {
        crate::crash_guard::guard("theme studio", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx)) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Theme Studio").show(ctx, body);
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, "Theme Studio // create theme") {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui);
                });
                super::paint_window_border(ctx);
            }
        });
    });
    lock(&SAVED).take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    /// The exhaustive run of 2026-11-06 17:41: every hex code in the studio was cut off after
    /// its fourth character, in every frame.
    #[test]
    fn every_hex_code_in_the_studio_is_shown_whole() {
        *lock(&DRAFT) = Some(CustomTheme::from_palette("Custom", &crate::theme_engine::current(), [3, 5, 9]));
        let mut stage = Stage::new(egui::vec2(900.0, 600.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("studio").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(560.0, 300.0)).show(ctx, |ui| {
                crate::hud_style::faded(ui, egui::ScrollArea::both().auto_shrink([false; 2]), |ui| {
                    ui.set_min_width(MIN_WIDTH);
                    body(ui);
                });
            });
        });
        *lock(&DRAFT) = None;
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        let codes = frame.find_text("#");
        assert_eq!(codes.len(), 4, "four colours, four hex codes: {}", frame.all_text());
        assert!(codes.iter().all(|r| r.width() > 40.0), "each seven characters wide: {codes:?}");
    }
}
