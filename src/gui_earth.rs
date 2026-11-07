//! The EARTH block (`crate::earth`): on the board, or in its own window once detached.
//! (Scaffold: the Earth's build draws the globe, its layers and its cards.)

use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::Arc;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "earth";
const TITLE: &str = "Earth";

fn body(ui: &mut Ui, _rt: &Handle) {
    ui.label(RichText::new("ACQUIRING ORBIT..").font(super::strip_font()).color(super::STRIP_MUTED));
}

/// Registers the Earth while it is open (a block on the board, or its own window once detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    if !crate::earth::is_open() {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, [1180.0, 720.0], |ui| body(ui, rt));
        if action.close {
            crate::earth::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Earth — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([1200.0, 740.0]).with_min_inner_size([700.0, 460.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jarvis_earth"), builder, move |ctx, class| {
        crate::crash_guard::guard("earth", || {
            if ctx.input(|i| i.viewport().close_requested()) || !crate::earth::is_open() {
                crate::earth::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        crate::earth::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}
