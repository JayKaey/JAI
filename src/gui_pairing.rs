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
    if !crate::hud_dock::all_own() {
        OPEN.store(false, Ordering::Release);
    }
}

/// Registers the view with egui while it is open (each HUD frame): a window of its own (a block on
/// the board in a session without windows).
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
        .with_min_inner_size([440.0, 560.0])
        .with_min_inner_size([420.0, 560.0])
        .with_transparent(true)
        .with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(std::sync::Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    let host = host.clone();
    ctx.show_viewport_deferred(viewport_id(), builder, move |ctx, class| {
        crate::crash_guard::guard("pairing window", || frame(ctx, Some(class), &host));
    });
}

/// The QR code as a painted grid (white quiet zone, black modules).
fn paint_qr(ui: &mut egui::Ui, url: &str, size: f32) {
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

/// What the pairing view shows.
struct Shown {
    local: String,
    worldwide: Option<String>,
    tunnel: crate::worldwide_tunnel::TunnelStatus,
    ip: String,
    /// The paired phone's GPS line.
    gps: Option<String>,
}

/// A link as long as it is: wrapped inside the view (one unbroken line pushed the whole view
/// out past both edges of its window, 2026-11-19), copied by its key.
fn link(ui: &mut egui::Ui, url: &str, colour: Color32, copy: &str) {
    let shown = ui.add(egui::Label::new(RichText::new(url).font(face(Face::Mono, 11.0)).color(colour)).wrap().sense(Sense::click()));
    if shown.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
    if crate::holo_ui::key(ui, copy).clicked() {
        ui.output_mut(|o| o.copied_text = url.to_string());
    }
}

/// The view itself: the QR code and its link, the three steps, the link for home and the
/// phone's state. It scrolls where the window is shorter than it.
fn body(ui: &mut egui::Ui, p: &Palette, s: &Shown) {
    crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false, false]), |ui| {
        ui.vertical_centered(|ui| {
            muted(ui, "J.A.I. on your phone or iPad, anywhere in the world");
            ui.add_space(8.0);
            // (As large as the view is wide: a narrow one still shows the whole code.)
            let wide = ui.available_width();
            let side = |most: f32| most.min(wide);
            match &s.worldwide {
                Some(url) => {
                    if s.tunnel.tailnet_only {
                        // (This PC reaches its own Funnel name through Tailscale: its check proved
                        // nothing for a phone outside the tailnet, 2026-11-21.)
                        ui.label(RichText::new("TAILSCALE ONLINE · CHECKED INSIDE YOUR TAILNET ONLY").font(face(Face::Label, 9.0)).color(p.warn));
                    } else {
                        ui.label(RichText::new(format!("WORLDWIDE HTTPS ONLINE · {}", s.tunnel.provider.to_uppercase())).font(face(Face::Label, 9.0)).color(p.nominal));
                        // One address for good, or one that is new with every start.
                        let (stays, colour) = if s.tunnel.permanent { ("PERMANENT ADDRESS", p.nominal) } else { ("CHANGES WHEN THE TUNNEL RESTARTS", p.warn) };
                        ui.label(RichText::new(stays).font(face(Face::Label, 8.5)).color(colour));
                    }
                    ui.add_space(6.0);
                    paint_qr(ui, url, side(300.0));
                    ui.add_space(8.0);
                    link(ui, url, p.accent, "Copy link");
                    if s.tunnel.tailnet_only {
                        ui.add(egui::Label::new(RichText::new("Opens on a phone with Tailscale switched on. Without it, only while Funnel is on for this PC. At home, the Wi-Fi link below always works.").font(face(Face::Body, 12.0)).color(p.warn)).wrap());
                    }
                }
                None => {
                    ui.add(egui::Label::new(RichText::new(format!("WORLDWIDE TUNNEL {} · {}", s.tunnel.state.to_uppercase(), s.tunnel.detail)).font(face(Face::Label, 9.0)).color(p.warn)).wrap());
                    muted(ui, "Its QR code appears here the moment it is online. Until then, on this Wi-Fi only:");
                    ui.add_space(6.0);
                    paint_qr(ui, &s.local, side(220.0));
                    ui.add_space(6.0);
                    link(ui, &s.local, p.muted, "Copy local link");
                }
            }
        });
        ui.add_space(10.0);
        // (Checked inside the tailnet only, "anywhere" is promised for a phone with Tailscale.)
        let anywhere = if s.tunnel.tailnet_only {
            "With the Tailscale app switched on, the phone reaches it from anywhere, over 4G/5G mobile data or any Wi-Fi."
        } else {
            "It runs through a secure HTTPS tunnel, so it works from anywhere in the world over 4G/5G mobile data or any Wi-Fi; no same Wi-Fi needed."
        };
        for (n, step) in [
            "Scan the Worldwide HTTPS QR code with your phone's camera (or open the link on the phone).",
            anywhere,
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
        if s.worldwide.is_some() && !s.tunnel.permanent {
            muted(ui, "For one address that never changes: Integrations, Quick Connections, Mobile satellite, Permanent address.");
        }
        if s.worldwide.is_some() {
            ui.add_space(4.0);
            crate::holo_ui::fold(ui, "pairing_local", "At home: the local Wi-Fi link", s.tunnel.tailnet_only, |ui| {
                muted(ui, format!("Same Wi-Fi as this PC ({}); accept J.A.I.'s own certificate once.", s.ip));
                link(ui, &s.local, p.muted, "Copy local link");
            });
        }
        ui.add_space(8.0);
        let (colour, text) = match &s.gps {
            Some(line) => (p.nominal, line.as_str()),
            None => (p.muted, "Waiting for a phone…"),
        };
        ui.label(RichText::new(text).font(face(Face::Body, 12.0)).color(colour));
    });
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
    let shown = Shown {
        local: crate::satellite::pairing_url(host.app.config()),
        worldwide: crate::worldwide_tunnel::pairing_url(host.app.config()),
        tunnel: crate::worldwide_tunnel::snapshot(),
        ip: crate::satellite::lan_ip().to_string(),
        gps: host.app.tools().stark.location.phone_fix().map(|f| format!("GPS LOCK ACTIVE · ±{:.0} m{}", f.accuracy_m, f.address.as_deref().map(|a| format!(" · {a}")).unwrap_or_default())),
    };
    let body = |ui: &mut egui::Ui| body(ui, &p, &shown);
    if class.is_none() {
        let action = super::hud_block_window(ctx, "pairing", "Mobile Satellite // QR link", [480.0, 640.0], body);
        if action.close {
            OPEN.store(false, Ordering::Release);
        }
    } else if class == Some(ViewportClass::Embedded) {
        egui::Window::new("Mobile Satellite Pairing").default_size([480.0, 640.0]).show(ctx, body);
    } else {
        egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(16.0))).show(ctx, |ui| {
            if super::detached_title_bar(ui, ctx, "pairing", "Mobile satellite pairing") {
                OPEN.store(false, Ordering::Release);
            }
            body(ui);
        });
        super::paint_window_border(ctx);
    }
    ctx.request_repaint_after(Duration::from_millis(500));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    const LINK: &str = "https://example-words-for-a-tunnel.trycloudflare.com/satellite?token=0123456789abcdef0123456789abcdef";

    fn shown(worldwide: bool) -> Shown {
        let tunnel = crate::worldwide_tunnel::TunnelStatus { state: if worldwide { "online" } else { "connecting" }.into(), detail: "Waiting for the tunnel's address".into(), provider: "cloudflare".into(), ..Default::default() };
        Shown { local: "https://192.168.0.10:8443/satellite?token=0123456789abcdef0123456789abcdef".into(), worldwide: worldwide.then(|| LINK.to_string()), tunnel, ip: "192.168.0.10".into(), gps: None }
    }

    /// The window of 2026-11-19: its link, one unbroken line, was wider than the window, and
    /// the centred column grew with it, so every line was cut at both window edges. Now each
    /// row stays inside the view, at its first size and at its smallest, online or not.
    #[test]
    fn the_whole_view_stays_inside_its_window() {
        let _look = crate::holo_theme::look_for_pictures();
        let p = Palette::new(&crate::interface::InterfaceSettings::default());
        // Why it was cut: the kit's link is laid out on one line.
        let mut stage = Stage::new(vec2(520.0, 700.0));
        let frame = stage.frames(2, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(16.0))).show(ctx, |ui| crate::holo_ui::link_to(ui, LINK, LINK));
        });
        assert!(frame.find_text("trycloudflare").iter().any(|r| r.width() > 488.0), "the link on one line fits after all");
        for (size, worldwide) in [(vec2(520.0, 700.0), true), (vec2(420.0, 560.0), true), (vec2(520.0, 700.0), false)] {
            let shown = shown(worldwide);
            let mut stage = Stage::new(size);
            let frame = stage.frames(4, &mut |ctx| {
                egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(16.0))).show(ctx, |ui| body(ui, &p, &shown));
            });
            if size.x == 520.0 {
                stage.raster(&frame).save(if worldwide { "panel_pairing" } else { "panel_pairing_local" });
            }
            let rows = frame.find_text("");
            assert!(rows.len() > 8, "{size:?}: only {} rows", rows.len());
            for row in &rows {
                assert!(row.left() >= 15.0 && row.right() <= size.x - 15.0, "{size:?}: a row runs from {} to {}:\n{}", row.left(), row.right(), frame.all_text());
            }
            assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{size:?}");
            assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{size:?}");
            // The link is all there, over several rows; the window's bar has the one Close.
            let link: String = frame.all_text().replace('\n', "");
            assert!(link.contains(if worldwide { LINK } else { shown.local.as_str() }), "{size:?}: the link is not whole:\n{}", frame.all_text());
            assert!(frame.find_text("close").is_empty(), "{size:?}: a second Close");
            for said in ["COPY", "Scan the Worldwide"] {
                assert!(!frame.find_text(said).is_empty(), "{size:?}: {said} is not on the view:\n{}", frame.all_text());
            }
        }
        // Checked from this PC inside the tailnet only (the phone link of 2026-11-21): said so,
        // and the link for home stands open under it.
        let mut tailnet = shown(true);
        tailnet.tunnel.tailnet_only = true;
        let mut stage = Stage::new(vec2(520.0, 980.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(16.0))).show(ctx, |ui| body(ui, &p, &tailnet));
        });
        stage.raster(&frame).save("panel_pairing_tailnet");
        let flat: String = frame.all_text().split_whitespace().collect();
        for said in ["INSIDEYOURTAILNETONLY", "Tailscaleswitchedon", LINK, tailnet.local.as_str()] {
            assert!(flat.contains(said), "{said} is not on the view:\n{}", frame.all_text());
        }
        assert!(!flat.contains("WORLDWIDEHTTPSONLINE") && !flat.contains("itworksfromanywhere") && flat.contains("WiththeTailscaleappswitchedon"), "{}", frame.all_text());
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
    }
}
