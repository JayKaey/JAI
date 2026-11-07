//! The NETWORK RADAR block (`crate::network_radar`): on the board, or in its own window once
//! detached. (Scaffold: the radar's build draws the sweep, the devices and the chart.)

use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::Arc;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "network_radar";
const TITLE: &str = "Network radar";

fn body(ui: &mut Ui, _rt: &Handle) {
    ui.label(RichText::new("LOCATING THE NETWORK..").font(super::strip_font()).color(super::STRIP_MUTED));
}

/// Registers the radar while it is open (a block on the board, or its own window once detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    if !crate::network_radar::is_open() {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, [1040.0, 640.0], |ui| body(ui, rt));
        if action.close {
            crate::network_radar::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Network radar — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([1060.0, 660.0]).with_min_inner_size([640.0, 420.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jarvis_network_radar"), builder, move |ctx, class| {
        crate::crash_guard::guard("network radar", || {
            if ctx.input(|i| i.viewport().close_requested()) || !crate::network_radar::is_open() {
                crate::network_radar::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        crate::network_radar::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}
