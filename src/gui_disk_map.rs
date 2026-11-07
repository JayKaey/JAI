//! The DISK MAP block (`crate::disk_map`): on the board, or in its own window once detached.
//! (Scaffold: the disk map's build draws the treemap, the usage ring and the lists.)

use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::Arc;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "disk_map";
const TITLE: &str = "Disk map";

fn body(ui: &mut Ui, _rt: &Handle) {
    ui.label(RichText::new("READING..").font(super::strip_font()).color(super::STRIP_MUTED));
}

/// Registers the disk map while it is open (a block on the board, or its own window once
/// detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    if !crate::disk_map::is_open() {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, TITLE, [1100.0, 680.0], |ui| body(ui, rt));
        if action.close {
            crate::disk_map::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Disk map — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([1120.0, 700.0]).with_min_inner_size([680.0, 440.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jarvis_disk_map"), builder, move |ctx, class| {
        crate::crash_guard::guard("disk map", || {
            if ctx.input(|i| i.viewport().close_requested()) || !crate::disk_map::is_open() {
                crate::disk_map::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        crate::disk_map::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}
