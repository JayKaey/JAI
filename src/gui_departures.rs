//! The DEPARTURES block (C1, "when's my next train?", "departures from Karlsruhe Hbf"): the live
//! board of a station, refreshed every 30 s while it is open. A click picks a row ("where is
//! that train?" then means it); SHOW ON EARTH follows it on the globe. The data comes from
//! `crate::transit`; this file only draws it.

use super::{bold, stroke, block_slate, strip_ink, strip_muted};
use crate::transit::{self, Departure};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Frame, Margin, Rect, Rounding, Sense, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "departures";
const TITLE: &str = "Departures";
const BLOCK_SIZE: [f32; 2] = [560.0, 470.0];
const WINDOW_SIZE: [f32; 2] = [580.0, 520.0];
const WINDOW_MIN: [f32; 2] = [420.0, 300.0];
const VIEWPORT: &str = "jai_departures";
const ROW_H: f32 = 30.0;

fn mono(size: f32) -> FontId {
    FontId::monospace(super::ts(size))
}

/// A line's pill colour: its own (bright enough), else by mode.
fn pill_colour(d: &Departure) -> Color32 {
    if let Some([r, g, b]) = d.colour {
        if 0.3 * r as f32 + 0.59 * g as f32 + 0.11 * b as f32 > 50.0 {
            return Color32::from_rgb(r, g, b);
        }
    }
    match d.mode.as_str() {
        "HIGHSPEED_RAIL" | "LONG_DISTANCE" | "NIGHT_RAIL" => Color32::from_rgb(0xE8, 0xEE, 0xF2),
        "SUBURBAN" | "METRO" => Color32::from_rgb(0x2E, 0x9E, 0x4F),
        "SUBWAY" => Color32::from_rgb(0x2F, 0x6F, 0xD6),
        "TRAM" => Color32::from_rgb(0xD9, 0xA4, 0x1E),
        "BUS" | "COACH" => Color32::from_rgb(0x8E, 0x4F, 0xC9),
        _ => Color32::from_rgb(0xC8, 0x3A, 0x3A),
    }
}

use crate::holo::ink_on;

/// What Sir clicked this frame.
#[derive(Default)]
struct Actions {
    select: Option<usize>,
    show_on_earth: bool,
    refresh: bool,
}

/// The board's content (the block's and the detached window's).
fn body(ui: &mut Ui, rt: &Handle) {
    transit::schedule(rt);
    let board = transit::board().clone();
    let tz = transit::local_offset();
    let now = chrono::Utc::now();
    let accent = crate::hud_board::active_colour();
    let mut actions = Actions::default();

    // The header: the station, when it was updated, LIVE or DEMO.
    ui.horizontal(|ui| {
        let name = board.stop.as_ref().map(|s| s.name.clone()).or_else(|| board.asked.clone()).unwrap_or_else(|| "Finding your station…".into());
        ui.label(egui::RichText::new(name.to_uppercase()).font(bold(15.0)).color(strip_ink()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (tag, colour) = if board.demo { ("DEMO", strip_muted()) } else if board.loading { ("…", strip_muted()) } else { ("LIVE", crate::holo::theme().alert()) };
            ui.label(egui::RichText::new(tag).font(mono(11.0)).color(colour));
            if !board.updated.is_empty() {
                ui.label(egui::RichText::new(format!("UPDATED {}", board.updated)).font(mono(11.0)).color(strip_muted()));
            }
        });
    });
    ui.add_space(4.0);
    if let Some(e) = &board.error {
        ui.label(egui::RichText::new(e).font(mono(11.0)).color(crate::holo::theme().warn()));
    }
    if board.loading && board.rows.is_empty() && board.error.is_none() {
        ui.label(egui::RichText::new("Asking Transitous for the departures…").font(mono(11.0)).color(strip_muted()));
    }

    // The columns.
    let full = ui.available_width();
    let cols = [0.0, 0.14, 0.30, 0.72, 0.84].map(|f| f * full);
    let head = ui.allocate_exact_size(vec2(full, 18.0), Sense::hover()).0;
    let p = ui.painter();
    for (x, h) in cols.iter().zip(["TIME", "LINE", "DESTINATION", "PLATF.", "STATUS"]) {
        p.text(pos2(head.left() + x, head.center().y), Align2::LEFT_CENTER, h, mono(11.0), strip_muted());
    }
    p.line_segment([head.left_bottom(), head.right_bottom()], stroke(1.0, accent.gamma_multiply(0.35)));

    let footer = 34.0;
    crate::holo_block::faded(ui, egui::ScrollArea::vertical().max_height((ui.available_height() - footer).max(60.0)).auto_shrink([false, false]), |ui| {
        for (i, d) in board.rows.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(full, ROW_H), Sense::click());
            let p = ui.painter();
            let selected = board.selected == Some(i);
            let gone = d.expected < now - chrono::Duration::seconds(30);
            if selected {
                p.rect_filled(rect, Rounding::ZERO, accent.gamma_multiply(0.14));
                p.line_segment([rect.left_top(), rect.left_bottom()], stroke(2.0, accent));
            } else if resp.hovered() {
                p.rect_filled(rect, Rounding::ZERO, Color32::from_white_alpha(8));
            }
            let fade = if gone { 0.45 } else { 1.0 };
            let y = rect.center().y;
            // The time: planned, with the expected one beside it when late.
            let planned = d.planned.with_timezone(&tz).format("%H:%M").to_string();
            p.text(pos2(rect.left() + cols[0] + 4.0, y), Align2::LEFT_CENTER, &planned, mono(13.0), strip_ink().gamma_multiply(fade));
            // The line pill.
            let pill_c = pill_colour(d).gamma_multiply(fade);
            let galley = p.layout_no_wrap(d.line.clone(), bold(11.0), ink_on(pill_c));
            let pill = Rect::from_min_size(pos2(rect.left() + cols[1], y - 9.0), vec2((galley.size().x + 12.0).min(cols[2] - cols[1] - 6.0), 18.0));
            p.rect_filled(pill, Rounding::ZERO, pill_c);
            p.with_clip_rect(pill).galley(pos2(pill.left() + 6.0, y - galley.size().y / 2.0), galley, ink_on(pill_c));
            // The destination.
            let dest_w = cols[3] - cols[2] - 8.0;
            let dest = p.layout(d.headsign.clone(), FontId::proportional(super::ts(13.0)), if d.cancelled { strip_muted() } else { strip_ink().gamma_multiply(fade) }, f32::INFINITY);
            let dest_rect = Rect::from_min_size(pos2(rect.left() + cols[2], y - dest.size().y / 2.0), vec2(dest_w, dest.size().y));
            p.with_clip_rect(dest_rect).galley(dest_rect.min, dest, strip_ink());
            if d.cancelled {
                p.line_segment([pos2(dest_rect.left(), y), pos2(dest_rect.left() + dest_w.min(200.0), y)], stroke(1.0, crate::holo::theme().alert().gamma_multiply(0.8)));
            }
            // The platform (amber when it changed).
            if let Some(track) = &d.track {
                let c = if d.platform_changed() { crate::holo::theme().warn() } else { strip_ink().gamma_multiply(fade) };
                p.text(pos2(rect.left() + cols[3], y), Align2::LEFT_CENTER, track, mono(12.0), c);
            }
            // The status.
            let (status, c) = if d.cancelled {
                ("cancelled".to_string(), crate::holo::theme().alert())
            } else if !d.realtime {
                ("timetable".to_string(), strip_muted())
            } else if d.delay_min() >= 1 {
                (format!("{} +{}", d.expected.with_timezone(&tz).format("%H:%M"), d.delay_min()), crate::holo::theme().warn())
            } else {
                ("on time".to_string(), crate::holo::theme().ink())
            };
            p.text(pos2(rect.left() + cols[4], y), Align2::LEFT_CENTER, status, mono(11.0), c.gamma_multiply(fade));
            p.line_segment([rect.left_bottom(), rect.right_bottom()], stroke(1.0, Color32::from_white_alpha(10)));
            if resp.clicked() {
                actions.select = Some(i);
            }
            resp.on_hover_cursor(CursorIcon::PointingHand);
        }
        if board.rows.is_empty() && !board.loading && board.error.is_none() && board.stop.is_some() {
            ui.label(egui::RichText::new("Nothing leaves in the next while.").font(mono(11.0)).color(strip_muted()));
        }
    });

    // The footer: the buttons and the source.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let button = |ui: &mut Ui, text: &str, enabled: bool| {
            ui.add_enabled_ui(enabled, |ui| crate::holo_ui::key(ui, text)).inner
        };
        if button(ui, "SHOW ON EARTH", transit::focused().is_some()).on_hover_text("Follow the selected train on the Earth").clicked() {
            actions.show_on_earth = true;
        }
        if button(ui, "REFRESH", !board.loading && board.stop.is_some()).clicked() {
            actions.refresh = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("Transitous \u{b7} open data").font(mono(11.0)).color(strip_muted()));
        });
    });

    if let Some(i) = actions.select {
        transit::select(i);
        crate::gui::refresh();
    }
    if actions.refresh {
        transit::refresh_now();
        crate::gui::refresh();
    }
    if actions.show_on_earth {
        rt.spawn(async {
            if let Err(e) = crate::earth::run(&transit::show_train_args()).await {
                tracing::debug!("[DEPARTURES] Show on Earth: {e}");
            }
        });
    }
    // The board's clock moves (gone rows fade): a frame every few seconds is plenty.
    ui.ctx().request_repaint_after(Duration::from_secs(if board.loading { 1 } else { 5 }));
}

/// Registers the board while it is open (a block, or its own window once detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    if !super::detached_keep(ctx, ViewportId::from_hash_of(VIEWPORT), transit::is_open()) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, BLOCK_SIZE, |ui| body(ui, rt));
        if action.close {
            transit::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Departures — J.A.I.").with_app_id("jai").with_inner_size(WINDOW_SIZE).with_min_inner_size(WINDOW_MIN).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of(VIEWPORT), builder, move |ctx, class| {
        crate::crash_guard::guard("departures", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!transit::is_open() && !super::detached_leaving(ctx)) {
                transit::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        transit::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}
