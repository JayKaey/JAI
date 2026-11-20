//! The first start of J.A.I. on another PC (Sir, 2026-11-19: "When opening JAI on a PC for the
//! first time there should be an login screen like on the phone"): the address of Sir's PC, a
//! name and a password; or asking for an account there, with the key his screen shows. Until it
//! is logged in, a J.A.I. on another PC answers nobody (`fleet::locked`).

use super::{strip_ink, strip_muted};
use crate::config::ConfigStore;
use crate::holo::Face;
use crate::holo_ui;
use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::runtime::Handle;

const ID: &str = "login";
const TITLE: &str = "Log in // Sir's J.A.I.";
static OPEN: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Form {
    /// The address of Sir's PC.
    main: String,
    name: String,
    password: String,
    /// The key from Sir's screen, once an account was asked for.
    key: String,
    asked: bool,
    busy: bool,
    /// What the last try came to, and whether that is a fault.
    said: (String, bool),
    /// The installer's own lines (`installer`), when this is it.
    setup: Option<Setup>,
}

struct Setup {
    /// The graphics card's memory, and the brain it allows (or why J.A.I. cannot run here).
    card: String,
    brain: Result<&'static str, String>,
    into: String,
    /// This PC is logged in at Sir's PC already (J.A.I. is installed here): UPDATE needs no login.
    update: bool,
    finished: bool,
}

/// The installer's window opens with this form.
pub(super) fn installing(main: String, update: bool, card: String, brain: Result<&'static str, String>, into: &std::path::Path) {
    *lock() = Some(Form { main, setup: Some(Setup { card, brain, into: into.display().to_string(), update, finished: false }), ..Default::default() });
}

/// A step of the installer, for its window.
fn tell(line: String, fault: bool) {
    if let Some(form) = lock().as_mut() {
        form.said = (line, fault);
    }
    super::request_repaint();
}

static FORM: Mutex<Option<Form>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Form>> {
    FORM.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open() {
    OPEN.store(true, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

pub(super) fn field(ui: &mut Ui, title: &str, text: &mut String, hint: &str, password: bool) {
    ui.label(RichText::new(title.to_uppercase()).font(crate::holo::font(Face::Label, 8.5)).color(strip_ink()));
    // (A hint in the faint ink: in the field's own ink it read as a name already typed.)
    holo_ui::slot(ui, egui::TextEdit::singleline(text).hint_text(RichText::new(hint).color(crate::holo::theme().faint())).password(password).desired_width(f32::INFINITY));
    ui.add_space(6.0);
}

/// Logs in (or asks for an account, or finishes asking with the key) at Sir's PC.
fn send(rt: &Handle, config: &ConfigStore, form: &mut Form, ask: bool) {
    let (name, password) = (form.name.trim().to_string(), form.password.clone());
    let update = !ask && form.setup.as_ref().is_some_and(|s| s.update);
    // (An update logs in with the key this PC holds: no name, no password.)
    if let (Err(e), false) = (crate::accounts::check(&name, true).and_then(|_| crate::accounts::check(&password, false)), update) {
        form.said = (e, true);
        return;
    }
    form.busy = true;
    form.said = ("One moment…".into(), false);
    let (config, main, key, installing) = (config.clone(), form.main.clone(), form.asked.then(|| form.key.clone()), form.setup.is_some());
    rt.spawn(async move {
        let mut result = if update {
            Ok(String::new())
        } else if ask {
            crate::fleet::ask_for_account(&main, &name, &password).await
        } else {
            crate::fleet::log_in(&config, &main, &name, &password, key.as_deref()).await.map(|n| format!("Logged in as {n}."))
        };
        // The installer goes on: everything J.A.I. needs, from Sir's PC and from where his models come.
        if installing && !ask && result.is_ok() {
            result = crate::installer::install(&config, &main, |line| tell(line, false)).await.map(|()| "J.A.I. is installed and starts now.".into());
        }
        if let Some(form) = lock().as_mut() {
            form.busy = false;
            form.asked |= ask && result.is_ok();
            if result.is_ok() && !ask {
                OPEN.store(false, Ordering::Release);
                if let Some(setup) = form.setup.as_mut() {
                    setup.finished = true;
                }
            }
            // (Sir's PC may no longer take this PC's key: the login comes back.)
            if let (Some(setup), true) = (form.setup.as_mut(), update && result.is_err()) {
                setup.update = false;
            }
            form.said = result.map_or_else(|e| (e, true), |m| (m, false));
        }
        super::request_repaint();
    });
}

fn body(ui: &mut Ui, rt: &Handle, config: &ConfigStore) {
    let mut guard = lock();
    let form = guard.get_or_insert_with(|| Form { main: config.read(|c| c.main_pc.clone()), ..Default::default() });
    let update = form.setup.as_ref().is_some_and(|s| s.update);
    let about = if update {
        "J.A.I. is installed on this PC: UPDATE brings him to the version Sir's PC runs, with anything new he needs."
    } else if form.setup.is_some() {
        "Install J.A.I. on this PC: log in with your account at Sir's PC, or ask for one (a key appears on his screen). Everything he needs comes from there."
    } else {
        "This PC runs J.A.I. as one of Sir's. Log in with your account, or ask for one: a key appears on Sir's screen."
    };
    ui.label(RichText::new(about).font(crate::holo::font(Face::Body, 13.0)).color(strip_muted()));
    holo_ui::rule(ui);
    let mut runs_here = true;
    if let Some(setup) = &form.setup {
        let (line, colour) = match &setup.brain {
            Ok(brain) => (format!("{} · his brain: {brain}", setup.card), strip_ink()),
            Err(why) => (why.clone(), crate::holo::theme().alert()),
        };
        runs_here = setup.brain.is_ok();
        ui.label(RichText::new(line).font(crate::holo::font(Face::Mono, 12.0)).color(colour));
        ui.label(RichText::new(format!("Into {}", setup.into)).font(crate::holo::font(Face::Mono, 11.0)).color(strip_muted()));
        holo_ui::rule(ui);
    }
    if !runs_here {
        return;
    }
    field(ui, "Sir's PC", &mut form.main, "https://…", false);
    if !update {
        field(ui, "Name", &mut form.name, "3 to 20 characters", false);
        field(ui, "Password", &mut form.password, "3 to 20 characters", true);
    }
    if form.asked {
        field(ui, "The key on Sir's screen", &mut form.key, "six digits", false);
    }
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!form.busy, |ui| {
            if update {
                if holo_ui::key(ui, "UPDATE").clicked() {
                    send(rt, config, form, false);
                }
            } else if form.asked {
                if holo_ui::key(ui, "CREATE ACCOUNT").clicked() {
                    send(rt, config, form, false);
                }
            } else {
                if holo_ui::key(ui, if form.setup.is_some() { "INSTALL" } else { "LOG IN" }).clicked() {
                    send(rt, config, form, false);
                }
                if holo_ui::key(ui, "ASK FOR AN ACCOUNT").clicked() {
                    send(rt, config, form, true);
                }
            }
        });
    });
    let (said, fault) = &form.said;
    if !said.is_empty() {
        let colour = if *fault { crate::holo::theme().alert() } else { strip_ink() };
        ui.label(RichText::new(said).font(crate::holo::font(Face::Body, 13.0)).color(colour));
    }
}

/// The installer's whole window (`gui::run_installer`); it closes once J.A.I. is installed.
pub(super) fn installer(ctx: &egui::Context, rt: &Handle, config: &ConfigStore) {
    egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
        if super::detached_title_bar(ui, ctx, ID, "Install // J.A.I.") {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        body(ui, rt, config);
    });
    super::paint_window_border(ctx);
    if lock().as_ref().and_then(|f| f.setup.as_ref()).is_some_and(|s| s.finished) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

/// Registers the login while it is open (a window of its own; a block on the board in a session
/// without windows).
pub(super) fn show(ctx: &egui::Context, rt: &Handle, config: &ConfigStore) {
    let viewport = ViewportId::from_hash_of("jai_login");
    if !super::detached_keep(ctx, viewport, OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [460.0, 440.0], |ui| body(ui, rt, config)).close {
            OPEN.store(false, Ordering::Release);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Log in — J.A.I.").with_app_id("jai").with_inner_size([480.0, 460.0]).with_min_inner_size([380.0, 360.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    let (rt, config) = (rt.clone(), config.clone());
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("login", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx)) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Log in").show(ctx, |ui| body(ui, &rt, &config));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui, &rt, &config);
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

    /// The login after an account was asked for: every field, the key row, nothing cut off.
    #[test]
    fn the_login_of_another_pc_shows_every_field() {
        let _look = crate::holo_theme::look_for_pictures();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let config = ConfigStore::new(Default::default());
        *lock() = Some(Form { main: "https://jai.example.ts.net".into(), name: "Erika".into(), password: "secret".into(), asked: true, said: ("A key is on Sir's screen now.".into(), false), ..Default::default() });
        let mut stage = Stage::new(egui::vec2(480.0, 460.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("login").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(440.0, 420.0)).show(ctx, |ui| body(ui, rt.handle(), &config));
        });
        stage.raster(&frame).save("panel_login");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for shown in ["SIR'S PC", "NAME", "PASSWORD", "THE KEY ON SIR'S SCREEN", "CREATE ACCOUNT", "A key is on Sir's screen now."] {
            assert!(!frame.find_text(shown).is_empty(), "'{shown}' is not in the login");
        }
        assert!(frame.find_text("secret").is_empty(), "the password is never shown");
        // The installer: the card and the brain it allows, where he goes, INSTALL; a card too
        // small for him (no keys at all); and a PC that holds its key already (UPDATE, no login).
        let installer = [(Ok(crate::installer::SMALL_BRAIN), false, "panel_installer", "INSTALL"), (crate::installer::brain_for(Some(2048)), false, "panel_installer_refused", "2.0 GB"), (Ok(crate::installer::SMALL_BRAIN), true, "panel_installer_update", "UPDATE")];
        for (brain, update, picture, shown) in installer {
            installing("https://jai.example.ts.net".into(), update, "Graphics memory 6 GB".into(), brain, std::path::Path::new(r"C:\Users\Max\AppData\Local\Programs\JAI"));
            let frame = stage.frames(4, &mut |ctx| {
                egui::Window::new("login").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(440.0, 420.0)).show(ctx, |ui| body(ui, rt.handle(), &config));
            });
            stage.raster(&frame).save(picture);
            assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{picture}");
            assert!(!frame.find_text(shown).is_empty(), "'{shown}' is not in {picture}");
            assert_eq!(frame.find_text("ASK FOR AN ACCOUNT").is_empty(), picture != "panel_installer", "{picture}");
            assert_eq!(frame.find_text("PASSWORD").is_empty(), picture != "panel_installer", "{picture}");
        }
        // An update with the key this PC holds goes on without a name or a password.
        let mut form = Form { setup: Some(Setup { card: String::new(), brain: Ok(crate::installer::SMALL_BRAIN), into: String::new(), update: true, finished: false }), ..Default::default() };
        send(rt.handle(), &config, &mut form, false);
        assert!(form.busy && !form.said.1, "{:?}", form.said);
        // Without it, a login needs both.
        let mut form = Form { setup: form.setup.take().map(|s| Setup { update: false, ..s }), ..Default::default() };
        send(rt.handle(), &config, &mut form, false);
        assert!(!form.busy && form.said.1, "{:?}", form.said);
        *lock() = None;
    }
}
