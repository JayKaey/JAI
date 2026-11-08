//! `DISCORD // AUDIO ROUTING` (`[ MODULES ]`): where the copy of Jarvis' voice goes into Sir's
//! call (the injection target: a Wave Link channel in the Chat Mix or a virtual cable) and which
//! output he listens to (the call), a level meter of what the call hears, and
//! `[ TEST VOICE IN DISCORD ]`. Sir's own speaker keeps playing either way.

use super::{rgba, stroke, strip_font, STRIP_INK, STRIP_MUTED};
use crate::tools::ToolContext;
use eframe::egui::{self, Frame, Margin, RichText, Rounding, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

const ID: &str = "discord_audio";
const AUTO: &str = "Auto (Wave Link / Discord)";

static OPEN: AtomicBool = AtomicBool::new(false);
/// The outputs (listed off the HUD thread, at most every 5 s while the block is open).
static DEVICES: Mutex<(Option<Instant>, Vec<String>)> = Mutex::new((None, Vec::new()));
/// The last test's outcome.
static LAST: Mutex<String> = Mutex::new(String::new());

pub(super) fn open() {
    OPEN.store(true, Ordering::Release);
}

fn devices() -> Vec<String> {
    let mut d = DEVICES.lock().unwrap_or_else(|e| e.into_inner());
    if d.0.is_none_or(|at| at.elapsed() > Duration::from_secs(5)) {
        d.0 = Some(Instant::now());
        std::thread::spawn(|| {
            let list = crate::tts::list_output_devices();
            DEVICES.lock().unwrap_or_else(|e| e.into_inner()).1 = list;
            super::request_repaint();
        });
    }
    d.1.clone()
}

/// A device picker: `AUTO` or one of the outputs; the new choice when Sir picks one.
fn picker(ui: &mut Ui, id: &str, current: Option<&str>, devices: &[String]) -> Option<Option<String>> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(id).width(300.0).selected_text(current.unwrap_or(AUTO)).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_none(), AUTO).clicked() {
            picked = Some(None);
        }
        for d in devices {
            if ui.selectable_label(current == Some(d.as_str()), d).clicked() {
                picked = Some(Some(d.clone()));
            }
        }
    });
    picked
}

/// The level as `−18.4 dB` and a bar from −60 to 0 dB.
fn meter(ui: &mut Ui, level: f32) {
    let db = (20.0 * level.max(1e-5).log10()).clamp(-60.0, 0.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("CALL HEARS").font(strip_font()).color(STRIP_MUTED));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(220.0, 8.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_stroke(rect, Rounding::ZERO, stroke(1.0, crate::hud_board::border_colour()));
        let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * (db + 60.0) / 60.0, rect.height()));
        painter.rect_filled(fill, Rounding::ZERO, crate::hud_board::active_colour());
        ui.label(RichText::new(format!("{db:.1} dB")).font(strip_font()).color(STRIP_INK));
    });
}

fn body(ui: &mut Ui, tools: &ToolContext, rt: &Handle) {
    let devices = devices();
    let (target, source) = tools.config.read(|c| (c.discord_call_output.clone(), c.discord_listen_source.clone()));
    ui.label(RichText::new(format!("BRIDGE {} · VOICE INTO CALL: {}", crate::discord_bridge::mode().label().to_uppercase(), crate::discord_bridge::voice_device())).font(strip_font()).color(STRIP_INK));
    // Jarvis's own account (a bot of his: he joins calls as himself, no routing needed).
    if let Some(s) = crate::discord_bot::hud_status() {
        let call = match &s.call {
            Some(c) => format!(" · IN #{}{}{}", c.to_uppercase(), if s.encrypted { " (E2EE)" } else { "" }, if s.speaking_now { " · SPEAKING" } else if s.talking { " · MAY TALK" } else { " · LISTENING" }),
            None => String::new(),
        };
        let unread = if s.unread > 0 { format!(" · {} UNREAD", s.unread) } else { String::new() };
        ui.label(RichText::new(format!("HIS ACCOUNT: {}{call}{unread}", s.link.to_uppercase())).font(strip_font()).color(STRIP_INK));
    }
    ui.add_space(6.0);
    egui::Grid::new("discord_routing").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        ui.label(RichText::new("INJECTION TARGET").font(strip_font()).color(STRIP_INK));
        if let Some(next) = picker(ui, "discord_injection", target.as_deref(), &devices) {
            crate::discord_bridge::set_injection_target(&tools.config, next);
        }
        ui.end_row();
        ui.label(RichText::new("LISTEN SOURCE").font(strip_font()).color(STRIP_INK));
        if let Some(next) = picker(ui, "discord_listen", source.as_deref(), &devices) {
            crate::discord_bridge::set_listen_source(&tools.config, next);
        }
        ui.end_row();
    });
    ui.add_space(6.0);
    meter(ui, crate::tts::mirror_level());
    ui.add_space(6.0);
    let test = egui::Button::new(RichText::new("[ TEST VOICE IN DISCORD ]").font(strip_font()).color(STRIP_INK))
        .fill(rgba([3, 7, 12], 0.9))
        .stroke(stroke(1.0, crate::hud_board::border_colour()))
        .rounding(Rounding::ZERO);
    if ui.add(test).on_hover_text(format!("Says \"{}\" into the call and on your speaker", crate::discord_bridge::TEST_LINE)).clicked() {
        let tools = tools.clone();
        rt.spawn(async move {
            let o = tools.execute(crate::discord_bridge::TOOL, &serde_json::json!({ "action": "test" })).await;
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = format!("{} {}", if o.success { "PASS ·" } else { "FAIL ·" }, o.output.lines().next().unwrap_or(""));
            super::request_repaint();
        });
    }
    let last = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !last.is_empty() {
        ui.add(egui::Label::new(RichText::new(format!("→ {last}")).size(11.0).color(STRIP_INK)).wrap());
    }
    ui.add_space(6.0);
    ui.label(RichText::new("TIP: Wave Link — add a spare channel (SFX / Aux), turn it up in the Chat Mix and pick it as the injection target.").size(11.0).color(STRIP_MUTED));
    ui.label(RichText::new("No Wave Link: route a virtual cable into the microphone Discord uses (e.g. Voicemeeter) and pick the cable's input here.").size(11.0).color(STRIP_MUTED));
    ui.ctx().request_repaint_after(Duration::from_millis(100));
}

/// Registers the block while it is open: on the board, or its own window once detached.
pub(super) fn show(ctx: &egui::Context, tools: &ToolContext, rt: &Handle) {
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, ViewportId::from_hash_of("jarvis_discord_audio"), OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, "Discord // audio routing", [540.0, 330.0], |ui| body(ui, tools, rt));
        if action.close {
            OPEN.store(false, Ordering::Release);
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Discord Audio — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([560.0, 350.0]).with_min_inner_size([460.0, 300.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let (tools, rt) = (tools.clone(), rt.clone());
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jarvis_discord_audio"), builder, move |ctx, class| {
        crate::crash_guard::guard("discord audio", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx)) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Discord // audio routing").show(ctx, |ui| body(ui, &tools, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, "Discord // audio routing") {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui, &tools, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}
