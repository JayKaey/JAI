//! Sir's admin window ("open the admin window", "activate admin mode"): every window J.A.I.
//! opens, in groups, his own first, each opened with a click (`admin::WINDOWS`). And the pop-up
//! for his admin sign-in: on his main PC once, to make his admin account; on another PC of his
//! whenever a window that is not that PC's is opened (`admin::gate`).

use super::{strip_ink, strip_muted};
use crate::admin::{self, Window, GROUPS, WINDOWS};
use crate::app::Jai;
use crate::config::ConfigStore;
use crate::holo::{self, Face};
use crate::{holo_frame, holo_kit, holo_ui};
use eframe::egui::{self, pos2, vec2, Color32, Frame, Margin, RichText, Sense, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use tokio::runtime::Handle;

const ID: &str = "admin";
const TITLE: &str = "Admin // every window";
static OPEN: AtomicBool = AtomicBool::new(false);

const TILE_H: f32 = 66.0;
const MIN_TILE: f32 = 200.0;
const GAP: f32 = 8.0;

pub(super) fn set_open(open: bool) {
    OPEN.store(open, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// One window of the list: its name and what it is, OPEN while it is up.
fn tile(ui: &mut Ui, w: &'static Window, up: bool, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, TILE_H), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        admin::open(w);
    }
    if !ui.is_rect_visible(rect) {
        return;
    }
    let t = holo::theme();
    let lit = ui.ctx().animate_bool_with_time(response.id.with("lit"), response.hovered(), 0.12);
    let p = ui.painter();
    holo_frame::card(p, rect, lit, 1.0);
    // (What is live, in the accent.)
    let mut right = rect.right() - 8.0;
    if up {
        let width = holo::galley(p, Face::Label, "OPEN", 8.0, Color32::BLACK).size().x + 12.0;
        right = holo_kit::chip(p, pos2(right - width, rect.top() + 8.0), "OPEN", t.accent(), true, 1.0).left() - 6.0;
    }
    let name = holo::galley_within(p, Face::Label, w.title, 9.0, t.ink(), (right - rect.left() - 18.0).max(20.0));
    p.galley(pos2(rect.left() + 10.0, holo::snap(rect.top() + 15.0 - name.size().y / 2.0, holo::ppp(p))), name, t.ink());
    holo::wrapped(p, pos2(rect.left() + 10.0, rect.top() + 28.0), rect.width() - 20.0, Face::Body, w.about, 12.0, t.muted(), 2);
}

fn body(ui: &mut Ui, config: &ConfigStore) {
    let up = admin::up();
    let about = if crate::fleet::is_client(&config.read(|c| c.main_pc.clone())) {
        "Admin mode: everything on this PC is yours until this window closes. The account it is logged in with stays as it is."
    } else {
        "Your main PC: everything here is yours. Your own windows come first; on another PC they open only with your admin sign-in."
    };
    ui.label(RichText::new(about).font(holo::font(Face::Body, 13.0)).color(strip_muted()));
    ui.add_space(4.0);
    crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
        for (g, group) in GROUPS.iter().enumerate() {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            holo_kit::section(ui.painter(), row.left_top() + vec2(0.0, 8.0), row.width(), &format!("{:02}", g + 1), group, 1.0);
            // (The admin window is this one.)
            let shown: Vec<&'static Window> = WINDOWS.iter().filter(|w| w.group == g && w.id != ID).collect();
            let columns = ((ui.available_width() + GAP) / (MIN_TILE + GAP)).floor().max(1.0) as usize;
            let width = ((ui.available_width() - GAP * (columns - 1) as f32) / columns as f32).floor();
            for line in shown.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for w in line {
                        tile(ui, w, up.contains(&w.id), width);
                    }
                });
                ui.add_space(GAP - ui.spacing().item_spacing.y);
            }
        }
    });
}

/// Registers the admin window while it is open (a window of its own; a block on the board in a
/// session without windows). On his main PC without his admin account yet, the pop-up to make
/// it opens with the first frame.
pub(super) fn show(ctx: &egui::Context, app: &Jai, rt: &Handle) {
    static FIRST: Once = Once::new();
    FIRST.call_once(|| {
        if admin::needs_setup(app.config()) {
            ask();
        }
    });
    // (The call that asked for his sign-in, carried out now that it may be.)
    for (tool, args) in std::mem::take(&mut *THEN.lock().unwrap_or_else(|e| e.into_inner())) {
        let tools = app.tools().clone();
        rt.spawn(async move {
            let done = tools.execute(&tool, &args).await;
            tracing::info!("[ADMIN] {tool} after Sir's sign-in: {}", if done.success { "carried out" } else { "failed" });
        });
    }
    show_sign_in(ctx, app.config(), rt);
    let viewport = ViewportId::from_hash_of("jai_admin");
    if !super::detached_keep(ctx, viewport, OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [700.0, 640.0], |ui| body(ui, app.config())).close {
            OPEN.store(false, Ordering::Release);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Admin — J.A.I.").with_app_id("jai").with_inner_size([720.0, 680.0]).with_min_inner_size([460.0, 360.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    let config = app.config().clone();
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("admin", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx)) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Admin").show(ctx, |ui| body(ui, &config));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui, &config);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

// ------------------------------------------------------------------------ the pop-up ----

const ASK_ID: &str = "admin_sign_in";

#[derive(Default)]
struct Form {
    /// His main PC without his admin account: it is made here.
    setup: bool,
    name: String,
    password: String,
    again: String,
    busy: bool,
    /// What the last try came to, and whether that is a fault.
    said: (String, bool),
}

/// The pop-up's form, while it is open.
static FORM: Mutex<Option<Form>> = Mutex::new(None);
/// Asked for since the last frame.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The call that asked for his sign-in, once he is signed in (carried out by `show`).
static THEN: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());

fn form() -> MutexGuard<'static, Option<Form>> {
    FORM.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn ask() {
    ASKED.store(true, Ordering::Release);
}

/// The pop-up closes without a sign-in.
fn cancel() {
    *form() = None;
    admin::cancel();
}

/// Makes his admin account (the pop-up closes), or signs him in at his main PC; then what asked
/// for it goes on.
fn send(rt: &Handle, config: &ConfigStore, f: &mut Form) {
    let (name, password) = (f.name.trim().to_string(), std::mem::take(&mut f.password));
    if f.setup {
        let again = std::mem::take(&mut f.again);
        let made = if password == again { admin::set_up(config, &name, &password) } else { Err("The two passwords are not the same.".into()) };
        match made {
            Ok(()) => {
                tracing::info!("[ADMIN] Sir's admin account is made");
                f.setup = false;
            }
            Err(e) => f.said = (e, true),
        }
        return;
    }
    // (Not sent half typed: every wrong try counts against the name at his main PC.)
    if name.is_empty() || password.is_empty() {
        f.said = ("Type your name and your password.".into(), true);
        return;
    }
    f.busy = true;
    f.said = ("One moment…".into(), false);
    let config = config.clone();
    rt.spawn(async move {
        match admin::sign_in(&config, &name, &password).await {
            Ok(then) => {
                *form() = None;
                THEN.lock().unwrap_or_else(|e| e.into_inner()).extend(then);
            }
            Err(e) => {
                if let Some(f) = form().as_mut() {
                    (f.busy, f.said) = (false, (e, true));
                }
            }
        }
        super::request_repaint();
    });
}

fn sign_in_body(ui: &mut Ui, config: &ConfigStore, rt: &Handle) {
    let mut guard = form();
    let Some(f) = guard.as_mut() else { return };
    let asked = admin::asking();
    let about = if f.setup {
        "This is your main PC: it keeps the accounts, and J.A.I. answers nobody until your admin account is made. With it you sign in at the admin window of any other PC; nobody logs in with it.".to_string()
    } else if asked.is_some_and(|w| w.id == ID) {
        "Admin mode makes everything on this PC yours until the admin window closes. The account this PC is logged in with stays as it is.".to_string()
    } else {
        format!("{} is yours alone. Your admin sign-in opens it on this PC until it closes; the account this PC is logged in with stays as it is.", asked.map_or("This window", |w| w.title))
    };
    ui.label(RichText::new(about).font(holo::font(Face::Body, 13.0)).color(strip_muted()));
    holo_ui::rule(ui);
    super::login_view::field(ui, "Name", &mut f.name, if f.setup { "3 to 20 characters" } else { "your admin account" }, false);
    super::login_view::field(ui, "Password", &mut f.password, "3 to 20 characters", true);
    if f.setup {
        super::login_view::field(ui, "The password again", &mut f.again, "the same once more", true);
    }
    let enter = !f.busy && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut cancelled = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!f.busy, |ui| {
            if holo_ui::key(ui, if f.setup { "CREATE" } else { "SIGN IN" }).clicked() || enter {
                send(rt, config, f);
            }
            cancelled = !f.setup && holo_ui::key(ui, "CANCEL").clicked();
        });
    });
    let (said, fault) = &f.said;
    if !said.is_empty() {
        ui.label(RichText::new(said).font(holo::font(Face::Body, 13.0)).color(if *fault { holo::theme().alert() } else { strip_ink() }));
    }
    // (Made, cancelled, or nothing waits for his sign-in any more: the pop-up closes.)
    if !f.setup && (cancelled || (asked.is_none() && !f.busy)) {
        drop(guard);
        cancel();
    }
}

/// Registers the pop-up while it is open.
fn show_sign_in(ctx: &egui::Context, config: &ConfigStore, rt: &Handle) {
    if ASKED.swap(false, Ordering::AcqRel) {
        form().get_or_insert_with(|| Form { setup: admin::needs_setup(config), ..Default::default() });
    }
    let viewport = ViewportId::from_hash_of("jai_admin_sign_in");
    let title = if form().as_ref().is_some_and(|f| f.setup) { "Admin // your account" } else { "Admin // Sir's sign-in" };
    if !super::detached_keep(ctx, viewport, form().is_some()) {
        crate::hud_dock::forget(ASK_ID);
        return;
    }
    if !crate::hud_dock::adopt(ASK_ID) {
        if super::hud_block_window(ctx, ASK_ID, title, [460.0, 400.0], |ui| sign_in_body(ui, config, rt)).close {
            cancel();
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Admin sign-in — J.A.I.").with_app_id("jai").with_inner_size([480.0, 420.0]).with_min_inner_size([380.0, 340.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    let (config, rt) = (config.clone(), rt.clone());
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("admin sign-in", || {
            if ctx.input(|i| i.viewport().close_requested()) {
                cancel();
            }
            if form().is_none() && !super::detached_leaving(ctx) {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Admin sign-in").show(ctx, |ui| sign_in_body(ui, &config, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ASK_ID, title) {
                        cancel();
                    }
                    sign_in_body(ui, &config, &rt);
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
    use serde_json::json;

    fn pc(main_pc: &str) -> ConfigStore {
        ConfigStore::new(JaiConfig { main_pc: main_pc.into(), ..Default::default() })
    }

    /// The admin window on his main PC: every window of the list but itself, in its groups, his
    /// own first and marked, nothing cut off or drawn over.
    #[test]
    fn the_admin_window_lists_every_window() {
        let _look = crate::holo_theme::look_for_pictures();
        let main = pc("");
        let mut stage = Stage::new(egui::vec2(720.0, 1120.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("admin").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(680.0, 1080.0)).show(ctx, |ui| body(ui, &main));
        });
        stage.raster(&frame).save("panel_admin");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for w in WINDOWS.iter().filter(|w| w.id != ID) {
            assert!(!frame.find_text(&w.title.to_uppercase()).is_empty(), "'{}' is not in the admin window", w.title);
        }
        for group in GROUPS {
            assert!(!frame.find_text(&group.to_uppercase()).is_empty(), "the group '{group}'");
        }
        assert!(frame.find_text("Your main PC").len() == 1);
        assert!(frame.find_exact("OPEN").is_empty(), "nothing is open");
        // What is up says so.
        let _tests = crate::admin::TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert!(crate::admin::gate(&main, rt.handle(), &["earth", "settings"]).is_empty(), "his main PC closes nothing");
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("admin").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(680.0, 1080.0)).show(ctx, |ui| body(ui, &main));
        });
        stage.raster(&frame).save("panel_admin_open");
        assert_eq!(frame.find_exact("OPEN").len(), 2);
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        crate::admin::gate(&main, rt.handle(), &[]);
    }

    /// The pop-up: on his main PC, the making of his admin account; on another PC, his sign-in
    /// for the window that asked, with CANCEL.
    #[test]
    fn the_pop_up_makes_his_account_or_signs_him_in() {
        let _look = crate::holo_theme::look_for_pictures();
        let _tests = crate::admin::TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let mut stage = Stage::new(egui::vec2(480.0, 420.0));
        let main = pc("");
        *form() = Some(Form { setup: true, name: "Mustermann".into(), password: "geheim".into(), said: ("The two passwords are not the same.".into(), true), ..Default::default() });
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("ask").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(440.0, 380.0)).show(ctx, |ui| sign_in_body(ui, &main, rt.handle()));
        });
        stage.raster(&frame).save("panel_admin_setup");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for shown in ["NAME", "PASSWORD", "THE PASSWORD AGAIN", "CREATE", "This is your main PC", "The two passwords are not the same."] {
            assert!(!frame.find_text(shown).is_empty(), "'{shown}' is not in the setup");
        }
        assert!(frame.find_text("geheim").is_empty() && frame.find_text("CANCEL").is_empty(), "no password shown, and no way round it");
        // Another PC: the settings were asked for.
        let other = pc("https://main.example");
        assert_eq!(crate::admin::refusal(&other, "configure_interface", &json!({"settings_panel": true})).as_deref(), Some(crate::admin::SIGN_IN));
        *form() = Some(Form::default());
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("ask").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(440.0, 380.0)).show(ctx, |ui| sign_in_body(ui, &other, rt.handle()));
        });
        stage.raster(&frame).save("panel_admin_sign_in");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for shown in ["Settings is yours alone", "SIGN IN", "CANCEL"] {
            assert!(!frame.find_text(shown).is_empty(), "'{shown}' is not in the sign-in");
        }
        assert!(frame.find_text("THE PASSWORD AGAIN").is_empty());
        // Half typed, nothing goes to his main PC (each wrong try counts there); two passwords
        // that differ make no account; nothing typed is kept either way.
        let mut half = Form { name: "Mustermann".into(), ..Default::default() };
        send(rt.handle(), &other, &mut half);
        assert!(!half.busy && half.said.1, "{:?}", half.said);
        let mut setup = Form { setup: true, name: "Mustermann".into(), password: "geheim123".into(), again: "geheim124".into(), ..Default::default() };
        send(rt.handle(), &main, &mut setup);
        assert_eq!(setup.said, ("The two passwords are not the same.".to_string(), true));
        assert!(setup.setup && setup.password.is_empty() && setup.again.is_empty());
        // Nothing waits for his sign-in any more: it closes.
        crate::admin::cancel();
        stage.frames(1, &mut |ctx| {
            egui::Window::new("ask").show(ctx, |ui| sign_in_body(ui, &other, rt.handle()));
        });
        assert!(form().is_none());
    }
}
