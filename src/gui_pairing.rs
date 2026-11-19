//! The Mobile Satellite pairing window: a native window with the worldwide HTTPS QR code
//! (rendered here from the `qrcode` matrix; the local Wi-Fi one while the tunnel connects), the
//! clickable link and the three steps, opened by "how can I use you on my phone?". Child module
//! of `gui`.

use super::widget_view::WidgetHost;
use super::{muted, paint_background, Palette};
use crate::holo::{font as face, Face};
use eframe::egui::{self, vec2, Color32, Frame, Margin, RichText, Rounding, Sense, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

static OPEN: AtomicBool = AtomicBool::new(false);
/// (url, side, modules) of the QR code drawn last.
static QR: Mutex<Option<(String, usize, Vec<bool>)>> = Mutex::new(None);

/// Opens the pairing window (from any thread).
pub(super) fn request() {
    OPEN.store(true, Ordering::Release);
}

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("jai_satellite_pairing")
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// `[ PHONE QR ]`: opens the QR block, or closes it (a detached window closes too).
pub(super) fn toggle() {
    OPEN.fetch_xor(true, Ordering::AcqRel);
}

/// Closes the pairing view when it sits inside the HUD (the HOME tab).
pub(super) fn close_in_app() {
    if !crate::hud_dock::is_detached("pairing") {
        OPEN.store(false, Ordering::Release);
    }
}

/// Registers the view with egui while it is open (each HUD frame): a ROSE block inside the HUD,
/// or its own window once detached.
pub(super) fn show(ctx: &egui::Context, host: &WidgetHost) {
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, viewport_id(), OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget("pairing");
        return;
    }
    if !crate::hud_dock::adopt("pairing") {
        frame(ctx, None, host);
        return;
    }
    let mut builder = ViewportBuilder::default()
        .with_title("Mobile Satellite Pairing — J.A.I.")
        .with_app_id("jai")
        .with_inner_size([520.0, 700.0])
        .with_min_inner_size([420.0, 560.0])
        .with_transparent(true)
        .with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(std::sync::Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, "pairing") {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let host = host.clone();
    ctx.show_viewport_deferred(viewport_id(), builder, move |ctx, class| {
        crate::crash_guard::guard("pairing window", || frame(ctx, Some(class), &host));
    });
}

/// The QR code as a painted grid (white quiet zone, black modules).
pub(super) fn paint_qr(ui: &mut egui::Ui, url: &str, size: f32) {
    let matrix = {
        let mut cache = QR.lock().unwrap_or_else(|e| e.into_inner());
        if cache.as_ref().is_none_or(|(u, _, _)| u != url) {
            *cache = crate::satellite::qr_matrix(url).map(|(side, m)| (url.to_string(), side, m));
        }
        cache.clone()
    };
    let Some((_, side, modules)) = matrix else {
        muted(ui, "(The link is too long for a QR code.)");
        return;
    };
    let quiet = 4.0;
    let cell = (size / (side as f32 + 2.0 * quiet)).floor().max(2.0);
    let total = cell * (side as f32 + 2.0 * quiet);
    let (rect, _) = ui.allocate_exact_size(vec2(total, total), Sense::hover());
    ui.painter().rect_filled(rect, Rounding::ZERO, Color32::WHITE);
    for (i, dark) in modules.iter().enumerate() {
        if *dark {
            let (x, y) = ((i % side) as f32, (i / side) as f32);
            let min = rect.min + vec2((quiet + x) * cell, (quiet + y) * cell);
            ui.painter().rect_filled(egui::Rect::from_min_size(min, vec2(cell, cell)), Rounding::ZERO, Color32::BLACK);
        }
    }
}

/// One frame of the pairing view: `class` None = the block inside the HUD.
fn frame(ctx: &egui::Context, class: Option<ViewportClass>, host: &WidgetHost) {
    if class.is_some() && (ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx))) {
        OPEN.store(false, Ordering::Release);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    }
    let p = Palette::new(&host.app.config().read(|c| c.interface.clone()));
    if class.is_some() {
        paint_background(ctx, &p);
    }
    let local = crate::satellite::pairing_url(host.app.config());
    let worldwide = crate::worldwide_tunnel::pairing_url(host.app.config());
    let tunnel = crate::worldwide_tunnel::snapshot();
    let ip = crate::satellite::lan_ip();
    let fix = host.app.tools().stark.location.phone_fix();
    let body = |ui: &mut egui::Ui| {
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("MOBILE SATELLITE").font(face(Face::Label, 14.0)).color(p.text).extra_letter_spacing(3.0));
            muted(ui, "J.A.I. on your phone or iPad — anywhere in the world");
            ui.add_space(8.0);
            match &worldwide {
                Some(url) => {
                    ui.label(RichText::new(format!("WORLDWIDE HTTPS ONLINE · {}", tunnel.provider.to_uppercase())).font(face(Face::Label, 9.0)).color(p.nominal));
                    // One address for good, or one that is new with every start.
                    let (stays, colour) = if tunnel.permanent { ("PERMANENT ADDRESS", p.nominal) } else { ("CHANGES WHEN THE TUNNEL RESTARTS", p.warn) };
                    ui.label(RichText::new(stays).font(face(Face::Label, 8.5)).color(colour));
                    ui.add_space(6.0);
                    paint_qr(ui, url, 300.0);
                    ui.add_space(8.0);
                    crate::holo_ui::link_to(ui, RichText::new(url).font(face(Face::Mono, 11.0)).color(p.accent), url);
                    ui.horizontal(|ui| {
                        if crate::holo_ui::key(ui, "Copy link").clicked() {
                            ui.output_mut(|o| o.copied_text = url.clone());
                        }
                        if crate::holo_ui::key(ui, "Close").clicked() {
                            OPEN.store(false, Ordering::Release);
                        }
                    });
                }
                None => {
                    ui.label(RichText::new(format!("WORLDWIDE TUNNEL {} — {}", tunnel.state.to_uppercase(), tunnel.detail)).font(face(Face::Label, 9.0)).color(p.warn));
                    muted(ui, "Its QR code appears here the moment it is online. Until then, on this Wi-Fi only:");
                    ui.add_space(6.0);
                    paint_qr(ui, &local, 220.0);
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(RichText::new(&local).font(face(Face::Mono, 11.0)).color(p.muted)).wrap());
                    ui.horizontal(|ui| {
                        if crate::holo_ui::key(ui, "Copy local link").clicked() {
                            ui.output_mut(|o| o.copied_text = local.clone());
                        }
                        if crate::holo_ui::key(ui, "Close").clicked() {
                            OPEN.store(false, Ordering::Release);
                        }
                    });
                }
            }
        });
        ui.add_space(10.0);
        for (n, step) in [
            "Scan the Worldwide HTTPS QR code with your phone's camera (or open the link on the phone).",
            "It runs through a secure HTTPS tunnel, so it works from anywhere in the world over 4G/5G mobile data or any Wi-Fi — no same Wi-Fi needed.",
            "In Safari or Chrome tap Share → \"Add to Home Screen\", open it from there and allow Microphone and Location (GPS).",
        ]
        .iter()
        .enumerate()
        {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{}.", n + 1)).font(face(Face::Head, 13.0)).color(p.accent));
                ui.label(RichText::new(*step).font(face(Face::Body, 12.5)).color(p.text));
            });
        }
        if worldwide.is_some() && !tunnel.permanent {
            muted(ui, "For one address that never changes: Integrations, Quick Connections, Mobile satellite, Permanent address.");
        }
        if worldwide.is_some() {
            ui.add_space(4.0);
            crate::holo_ui::fold(ui, "pairing_local", "At home: the local Wi-Fi link", false, |ui| {
                muted(ui, format!("Same Wi-Fi as this PC ({ip}); accept J.A.I.' own certificate once."));
                ui.add(egui::Label::new(RichText::new(&local).font(face(Face::Mono, 11.0)).color(p.muted)).wrap());
                if crate::holo_ui::key(ui, "Copy local link").clicked() {
                    ui.output_mut(|o| o.copied_text = local.clone());
                }
            });
        }
        ui.add_space(8.0);
        let (colour, text) = match &fix {
            Some(f) => (p.nominal, format!("GPS LOCK ACTIVE · ±{:.0} m{}", f.accuracy_m, f.address.as_deref().map(|a| format!(" · {a}")).unwrap_or_default())),
            None => (p.muted, "Waiting for a phone…".to_string()),
        };
        ui.label(RichText::new(text).font(face(Face::Body, 12.0)).color(colour));
    };
    if class.is_none() {
        let action = super::hud_block_window(ctx, "pairing", "Mobile Satellite // QR link", [480.0, 640.0], body);
        if action.close {
            OPEN.store(false, Ordering::Release);
        }
        if action.detach {
            crate::hud_dock::detach("pairing");
        }
    } else if class == Some(ViewportClass::Embedded) {
        egui::Window::new("Mobile Satellite Pairing").default_size([480.0, 640.0]).show(ctx, body);
    } else {
        egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(16.0))).show(ctx, |ui| {
            if super::detached_title_bar(ui, ctx, "Mobile satellite pairing") {
                OPEN.store(false, Ordering::Release);
            }
            body(ui);
        });
        super::paint_window_border(ctx);
    }
    ctx.request_repaint_after(Duration::from_millis(500));
}
