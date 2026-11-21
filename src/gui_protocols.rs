//! The protocol window ("open the protocol window", the admin window's tile): Sir's protocols,
//! each with what he says for it (SAY), when it runs by itself (WHEN: a program, the clock, a
//! PC of his or a friend's) and what J.A.I. does then (DO: one order a line, in his words),
//! set up and changed by hand. `protocols` keeps and runs them; `kit_notes` watches the WHENs.

use super::{strip_ink, strip_muted};
use crate::holo::{self, Face};
use crate::holo_ui;
use crate::protocols::{self, Protocol, ProtocolStore, Step};
use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// The window's id among his views.
const ID: &str = "protocols";
const TITLE: &str = "Protocols // say it, or when it happens";
static OPEN: AtomicBool = AtomicBool::new(false);
/// The list's width.
const LIST_W: f32 = 190.0;

/// The protocol on the form, as it is typed.
#[derive(Default)]
struct Form {
    /// Its name as it is kept (empty: a new one).
    chosen: String,
    name: String,
    say: String,
    when: String,
    steps: String,
    /// What the last SAVE, RUN or DELETE came to, and whether that is a fault.
    said: (String, bool),
}

static FORM: Mutex<Form> = Mutex::new(Form { chosen: String::new(), name: String::new(), say: String::new(), when: String::new(), steps: String::new(), said: (String::new(), false) });

fn form() -> MutexGuard<'static, Form> {
    FORM.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open() {
    OPEN.store(true, Ordering::Release);
}

pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

impl Form {
    /// The form for `p` (None: an empty one for a new protocol).
    fn load(&mut self, p: Option<&Protocol>) {
        *self = p.map_or_else(Form::default, |p| Form {
            chosen: p.name.clone(),
            name: p.name.clone(),
            say: p.trigger_phrases.join("\n"),
            when: p.when.join("\n"),
            steps: p.steps.iter().map(Step::line).collect::<Vec<_>>().join("\n"),
            said: (String::new(), false),
        });
    }
}

/// Keeps the form as a protocol (one named anew replaces the one it was).
fn save(f: &mut Form, store: &ProtocolStore) -> Result<Protocol, String> {
    let lines = |text: &str| text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect::<Vec<_>>();
    let when = lines(&f.when);
    if let Some(bad) = when.iter().find(|line| crate::kit_notes::when_of(line).is_none()) {
        return Err(format!("\"{bad}\" is no WHEN I know. Say it like \"Max comes online\", \"Minecraft starts\", \"every day at 7:30\" or \"I start a game\"."));
    }
    if let Some(other) = store.get(&f.name).filter(|p| p.name != f.chosen) {
        return Err(format!("There is a {} already: choose it on the left to change it.", protocols::display_name(&other.name)));
    }
    let description = store.get(&f.chosen).map(|p| p.description).unwrap_or_default();
    let steps = lines(&f.steps).iter().filter_map(|line| Step::of_line(line)).collect();
    let saved = store.save(Protocol { name: f.name.clone(), description, trigger_phrases: lines(&f.say), steps, when })?;
    if !f.chosen.is_empty() && f.chosen != saved.name {
        store.delete(&f.chosen);
    }
    f.chosen = saved.name.clone();
    f.name = saved.name.clone();
    Ok(saved)
}

/// One part of the form: its name on a rule, what goes in it, the lines to type.
fn field(ui: &mut Ui, head: &str, about: &str, text: &mut String, rows: usize, hint: &str) {
    holo_ui::head(ui, head);
    ui.label(RichText::new(about).font(holo::font(Face::Body, 12.0)).color(strip_muted()));
    holo_ui::slot(ui, egui::TextEdit::multiline(text).desired_rows(rows).desired_width(f32::INFINITY).hint_text(hint));
    ui.add_space(6.0);
}

fn editor(ui: &mut Ui, f: &mut Form, store: &ProtocolStore, run: &dyn Fn(String)) {
    holo_ui::head(ui, "NAME");
    holo_ui::slot(ui, egui::TextEdit::singleline(&mut f.name).hint_text("wake up").desired_width(f32::INFINITY));
    ui.add_space(6.0);
    field(ui, "SAY", "What you say for it, a phrase a line. \"Engage the … protocol\" always works.", &mut f.say, 2, "wake up daddys home");
    field(ui, "WHEN", "When it runs by itself, one a line: a program, the clock, a PC of yours or a friend's.", &mut f.when, 2, "Max comes online\nI start a game");
    field(ui, "DO", "What I do then, an order a line, as you would say it. \"Tell Max's PC to …\" does it on his PC.", &mut f.steps, 6, "open Spotify\nplay my rock playlist\ntell Max's PC to open Minecraft");
    ui.horizontal(|ui| {
        if holo_ui::key(ui, "SAVE").clicked() {
            f.said = match save(f, store) {
                Ok(p) => (format!("Saved. Say \"engage the {} protocol\"{}.", p.name, p.trigger_phrases.first().map(|t| format!(" or \"{t}\"")).unwrap_or_default()), false),
                Err(e) => (e, true),
            };
        }
        if holo_ui::key(ui, "RUN").clicked() {
            f.said = match save(f, store) {
                Ok(p) => {
                    run(p.name.clone());
                    (format!("The {} is engaged.", protocols::display_name(&p.name)), false)
                }
                Err(e) => (e, true),
            };
        }
        if !f.chosen.is_empty() && holo_ui::key_alert(ui, "DELETE").clicked() {
            let gone = std::mem::take(&mut f.chosen);
            store.delete(&gone);
            f.load(None);
            f.said = (format!("The {} is gone.", protocols::display_name(&gone)), false);
        }
    });
    if !f.said.0.is_empty() {
        ui.label(RichText::new(&f.said.0).font(holo::font(Face::Body, 13.0)).color(if f.said.1 { holo::theme().alert() } else { strip_ink() }));
    }
}

/// The window's inside: his protocols on the left, the one chosen on the form. (`run` engages
/// one by its name.)
fn body(ui: &mut Ui, store: &ProtocolStore, run: &dyn Fn(String)) {
    let mut f = form();
    let all = store.list();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_W);
            if holo_ui::key(ui, "NEW").clicked() {
                f.load(None);
            }
            ui.add_space(4.0);
            crate::holo_block::faded(ui, egui::ScrollArea::vertical().id_salt("protocol_list").auto_shrink([false; 2]), |ui| {
                for p in &all {
                    if holo_ui::item(ui, protocols::display_name(&p.name), p.name == f.chosen).clicked() {
                        f.load(Some(p));
                    }
                }
            });
        });
        ui.add_space(12.0);
        ui.vertical(|ui| editor(ui, &mut f, store, run));
    });
}

/// The protocol window while it is open (a window of its own; a block on the board in a session
/// without windows).
pub(super) fn show(ctx: &egui::Context, app: &crate::app::Jai, rt: &tokio::runtime::Handle) {
    let viewport = ViewportId::from_hash_of("jai_protocols");
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, viewport, is_open()) {
        crate::hud_dock::forget(ID);
        return;
    }
    let store = app.tools().protocols.clone();
    let (tools, rt) = (app.tools().clone(), rt.clone());
    let run = move |name: String| {
        let tools = tools.clone();
        rt.spawn(async move {
            let _ = tools.execute("manage_stark_protocol", &serde_json::json!({ "action": "execute_protocol", "protocol_name": name })).await;
        });
    };
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [760.0, 560.0], |ui| body(ui, &store, &run)).close {
            close();
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Protocols — J.A.I.").with_app_id("jai").with_inner_size([780.0, 600.0]).with_min_inner_size([620.0, 480.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("protocols", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!is_open() && !super::detached_leaving(ctx)) {
                close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Protocols").show(ctx, |ui| body(ui, &store, &run));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        close();
                    }
                    body(ui, &store, &run);
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

    /// SAVE keeps what he typed (a WHEN it does not know is refused), a new name replaces the
    /// old one, and another protocol's name is not taken over.
    #[test]
    fn the_form_saves_a_protocol_as_he_typed_it() {
        let store = ProtocolStore::in_memory();
        // (A phrase no other test says: the table of phrases is the whole process's.)
        let mut f = Form { name: "Wake up".into(), say: "lights up max is here\n".into(), when: "Max comes online\nbanana".into(), steps: "open Spotify\n\nplay my rock playlist".into(), ..Form::default() };
        assert!(save(&mut f, &store).unwrap_err().contains("\"banana\""));
        f.when = "Max comes online\nevery day at 7:30".into();
        let p = save(&mut f, &store).unwrap();
        assert_eq!((p.name.as_str(), p.trigger_phrases.as_slice(), p.when.len(), p.steps.len()), ("wake up", ["lights up max is here".to_string()].as_slice(), 2, 2));
        assert!(p.steps.iter().all(|s| s.tool == protocols::ORDER));
        f.name = "welcome".into();
        save(&mut f, &store).unwrap();
        assert!(store.get("wake up").is_none() && store.get("welcome").is_some(), "renamed, not copied");
        f.name = "gaming".into();
        assert!(save(&mut f, &store).unwrap_err().contains("Gaming Protocol already"));
        let mut loaded = Form::default();
        loaded.load(store.get("welcome").as_ref());
        assert_eq!((loaded.say.as_str(), loaded.when.as_str(), loaded.steps.as_str()), ("lights up max is here", "Max comes online\nevery day at 7:30", "open Spotify\nplay my rock playlist"));
        assert!(store.delete("welcome"));
    }

    /// The list and the form whole, with a protocol of his on it.
    #[test]
    fn the_protocol_window_shows_a_protocol_whole() {
        let _look = crate::holo_theme::look_for_pictures();
        let store = ProtocolStore::in_memory();
        form().load(Some(&Protocol {
            name: "wake up".into(),
            description: String::new(),
            trigger_phrases: vec!["wake up daddys home".into()],
            when: vec!["Max comes online".into(), "every day at 7:30".into()],
            steps: ["open Spotify", "play my rock playlist", "tell Max's PC to open Minecraft"].iter().filter_map(|l| Step::of_line(l)).collect(),
        }));
        form().said = ("Saved. Say \"engage the wake up protocol\" or \"wake up daddys home\".".into(), false);
        let mut stage = Stage::new(egui::vec2(800.0, 600.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("protocols").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(760.0, 540.0)).show(ctx, |ui| body(ui, &store, &|_| {}));
        });
        stage.raster(&frame).save("panel_protocols");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for text in ["NEW", "NAME", "SAY", "WHEN", "DO", "SAVE", "RUN", "DELETE", "Gaming Protocol", "Max comes online", "tell Max's PC to open Minecraft"] {
            assert!(!frame.find_text(text).is_empty(), "{text} is not in the protocol window");
        }
        form().load(None);
    }
}
