//! The accounts list ("open the accounts list"): everyone besides Sir who may use J.A.I., with
//! their passwords; what each may do, every tool and every value of its options one by one (the
//! Earth, but not its satellites); the topics closed to them; a pause; their phones and PCs with
//! what those PCs run; and what J.A.I. did for them. Only Sir opens it (`accounts::run`). On
//! another PC of his, in admin mode, it shows the accounts at his main PC and changes them there
//! (`admin::remote_accounts`).

use super::{strip_ink, strip_muted};
use crate::accounts::{self, Account, Grant, TOPICS};
use crate::admin::{self, Remote};
use crate::holo::Face;
use crate::holo_ui;
use eframe::egui::{self, Frame, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const ID: &str = "accounts";
const TITLE: &str = "Accounts // who may use J.A.I.";
static OPEN: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct View {
    /// The memory database the accounts are kept in.
    db: String,
    /// The accounts at Sir's main PC, from another PC of his in admin mode.
    remote: bool,
    grants: Vec<Grant>,
    /// The account unfolded.
    chosen: String,
    search: String,
    /// A topic of Sir's own words being typed.
    topic: String,
    /// The password shown: whose, and what it is.
    shown: Option<(String, String)>,
    /// REMOVE was pressed once for this one: pressed again it removes.
    removing: String,
}

static VIEW: Mutex<Option<View>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<View>> {
    VIEW.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn open(db: String, remote: bool) {
    let grants = accounts::grants(&crate::tools::definitions_with(Vec::new()));
    let mut view = lock();
    let chosen = view.as_ref().filter(|v| v.db == db && v.remote == remote).map(|v| v.chosen.clone()).unwrap_or_default();
    *view = Some(View { db, remote, grants, chosen, ..Default::default() });
    OPEN.store(true, Ordering::Release);
}

pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

fn label(text: &str, size: f32) -> RichText {
    RichText::new(text.to_uppercase()).font(crate::holo::font(Face::Label, size)).color(strip_ink())
}

fn muted(text: &str) -> RichText {
    RichText::new(text).font(crate::holo::font(Face::Body, 12.5)).color(strip_muted())
}

fn ago(at: i64) -> String {
    match (accounts::now() - at).max(0) {
        s @ 0..=59 => format!("{s} s ago"),
        s @ 60..=3_599 => format!("{} min ago", s / 60),
        s @ 3_600..=86_399 => format!("{} h ago", s / 3_600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// What an account may do, in a few words.
fn summary(a: &Account) -> String {
    if a.admin {
        return format!("{}  ·  your admin account", a.name);
    }
    let may = if a.allow.iter().any(|k| k == "*") {
        "may do everything".to_string()
    } else if a.allow.is_empty() {
        "asks only".to_string()
    } else {
        format!("{} allowed", a.allow.len())
    };
    let state = a.paused().map_or(may, |until| format!("paused until {until}"));
    let seen = a.devices.first().map(|d| format!("  ·  seen {}", ago(d.seen))).unwrap_or_default();
    format!("{}  ·  {state}{seen}", a.name)
}

fn body(ui: &mut Ui) {
    let mut guard = lock();
    let Some(view) = guard.as_mut() else { return };
    let remote = view.remote.then(|| admin::remote_accounts(&view.chosen));
    let (all, waiting) = match &remote {
        Some(r) => (r.accounts.clone(), r.waiting.clone()),
        None => (accounts::all(&view.db), accounts::waiting()),
    };
    if !waiting.is_empty() || remote.is_some() {
        // (A key's minutes run down; his main PC's list is looked at again.)
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
    crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
        if let Some(r) = remote.as_ref().filter(|r| !r.error.is_empty()) {
            ui.label(RichText::new(&r.error).font(crate::holo::font(Face::Body, 13.0)).color(crate::holo::theme().alert()));
        }
        if !waiting.is_empty() {
            holo_ui::head(ui, "Asking for an account");
            for (name, code, left) in &waiting {
                ui.horizontal(|ui| {
                    ui.label(label(name, 9.0));
                    ui.label(RichText::new(format!("{} {}", &code[..3], &code[3..])).font(crate::holo::font(Face::Figure, 18.0)).color(crate::holo::theme().accent()));
                    ui.label(muted(&format!("holds {}:{:02}", left / 60, left % 60)));
                    if holo_ui::key(ui, "REFUSE").clicked() {
                        match remote {
                            Some(_) => admin::remote_change(json!({ "action": "refuse", "name": name })),
                            None => accounts::refuse(name),
                        }
                    }
                });
            }
            holo_ui::rule(ui);
        }
        if all.is_empty() {
            ui.label(muted("No accounts yet. Anyone can ask for one on the phone page: the key they need appears on this screen, and a new account may ask questions and nothing more until you allow it."));
            return;
        }
        for a in &all {
            let chosen = view.chosen.eq_ignore_ascii_case(&a.name);
            if holo_ui::item(ui, summary(a), chosen).clicked() {
                view.chosen = if chosen { String::new() } else { a.name.clone() };
                view.removing.clear();
            }
        }
        if let Some(a) = all.iter().find(|a| a.name.eq_ignore_ascii_case(&view.chosen)) {
            holo_ui::rule(ui);
            detail(ui, view, a, remote.as_ref());
        }
    });
}

/// One account unfolded (`remote`: one at Sir's main PC, changed there).
fn detail(ui: &mut Ui, view: &mut View, a: &Account, remote: Option<&Remote>) {
    let db = view.db.clone();
    let change = |f: &mut dyn FnMut(&mut Account)| {
        if remote.is_some() {
            let mut changed = a.clone();
            f(&mut changed);
            admin::remote_change(json!({ "action": "save", "name": a.name, "account": changed }));
        } else if let Err(e) = accounts::update(&db, &a.name, |x| f(x)) {
            tracing::warn!("[ACCOUNTS] {}'s account could not be changed: {e}", a.name);
        }
    };
    ui.label(label(&a.name, 12.0));
    if a.admin {
        ui.label(muted("Your admin account: it signs you in at the admin window of another PC, and nowhere else. Nobody logs in with it, and it changes on your main PC alone."));
    }
    // (His own admin password is shown on his main PC only.)
    if !(a.admin && remote.is_some()) {
        ui.horizontal(|ui| {
            ui.label(label("Password", 8.0));
            let shown = remote.map_or(view.shown.clone(), |r| r.shown.clone()).filter(|(n, _)| n.eq_ignore_ascii_case(&a.name)).map(|(_, p)| p);
            ui.label(RichText::new(shown.clone().unwrap_or_else(|| "••••••••".into())).font(crate::holo::font(Face::Mono, 13.0)).color(strip_ink()));
            if holo_ui::key(ui, if shown.is_some() { "HIDE" } else { "SHOW" }).clicked() {
                match (remote, shown) {
                    (Some(_), Some(_)) => admin::hide_password(),
                    (Some(_), None) => admin::remote_change(json!({ "action": "password", "name": a.name })),
                    (None, Some(_)) => view.shown = None,
                    (None, None) => view.shown = crate::vault::Vault::open(&db).ok().and_then(|v| accounts::password(&v, &a.name)).map(|p| (a.name.clone(), p)),
                }
            }
        });
    }
    if a.admin {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        match a.paused() {
            Some(until) => {
                ui.label(muted(&format!("Paused until {until}.")));
                if holo_ui::key(ui, "RESUME").clicked() {
                    change(&mut |x| x.until = 0);
                }
            }
            None => {
                for (text, minutes) in [("PAUSE 1 H", 60i64), ("PAUSE 1 DAY", 1_440), ("PAUSE 1 WEEK", 10_080)] {
                    if holo_ui::key(ui, text).clicked() {
                        change(&mut |x| x.until = accounts::now() + minutes * 60);
                    }
                }
            }
        }
        let arming = view.removing.eq_ignore_ascii_case(&a.name);
        if holo_ui::key_alert(ui, if arming { "REMOVE FOR GOOD?" } else { "REMOVE" }).clicked() {
            if arming {
                if remote.is_some() {
                    admin::remote_change(json!({ "action": "remove", "name": a.name }));
                } else if let Ok(vault) = crate::vault::Vault::open(&db) {
                    accounts::remove(&db, &vault, &a.name);
                }
                view.chosen.clear();
                view.removing.clear();
            } else {
                view.removing = a.name.clone();
            }
        }
    });
    holo_ui::fold(ui, ("allow", &a.name), &format!("What {} may do", a.name), true, |ui| {
        let mut everything = a.allow.iter().any(|k| k == "*");
        if holo_ui::toggle(ui, &mut everything, "Everything, as Sir may (this PC included)").changed() {
            change(&mut |x| {
                x.allow.retain(|k| k != "*");
                if everything {
                    x.allow.push("*".into());
                }
            });
        }
        ui.label(muted("All off: the language model alone. A tool on allows its options but those switched off below it; one option on allows just that."));
        holo_ui::slot(ui, egui::TextEdit::singleline(&mut view.search).hint_text("search the tools").desired_width(f32::INFINITY));
        let search = view.search.trim().to_lowercase();
        for g in view.grants.iter().filter(|g| search.is_empty() || g.tool.contains(&search) || g.about.to_lowercase().contains(&search) || g.values.iter().any(|v| v.contains(&search))) {
            let mut on = a.allows(&g.tool);
            if holo_ui::toggle(ui, &mut on, format!("{}: {}", g.tool, g.about)).changed() {
                change(&mut |x| x.set(&g.tool, on));
            }
            if g.values.is_empty() {
                continue;
            }
            ui.indent(("values", &g.tool), |ui| {
                holo_ui::fold(ui, ("values", &a.name, &g.tool), &format!("{} options", g.values.len()), !search.is_empty(), |ui| {
                    for value in &g.values {
                        let key = format!("{}:{value}", g.tool);
                        let mut on = a.allows(&key);
                        if holo_ui::toggle(ui, &mut on, value).changed() {
                            change(&mut |x| x.set(&key, on));
                        }
                    }
                });
            });
        }
    });
    holo_ui::fold(ui, ("windows", &a.name), &format!("Windows on {}'s PC", a.name), false, |ui| {
        ui.label(muted("On their PC a window opens when it is on here; any other asks for your admin sign-in. Yours (the accounts, the settings, the labs, Discord) are never theirs."));
        for w in admin::WINDOWS.iter().filter(|w| !w.sirs) {
            let key = admin::key(w.id);
            let mut on = a.allows(&key);
            if holo_ui::toggle(ui, &mut on, format!("{}: {}", w.title, w.about)).changed() {
                change(&mut |x| x.set(&key, on));
            }
        }
    });
    holo_ui::fold(ui, ("topics", &a.name), &format!("Topics closed to {}", a.name), false, |ui| {
        ui.label(muted("A closed topic gets no answer, from the language model either."));
        for (id, title) in TOPICS {
            let mut closed = a.topics.iter().any(|t| t == id);
            if holo_ui::toggle(ui, &mut closed, title).changed() {
                change(&mut |x| {
                    x.topics.retain(|t| t != id);
                    if closed {
                        x.topics.push(id.to_string());
                    }
                });
            }
        }
        for own in a.topics.iter().filter(|t| !TOPICS.iter().any(|(id, _)| id == *t)) {
            ui.horizontal(|ui| {
                ui.label(muted(&format!("\"{own}\"")));
                if holo_ui::key(ui, "OPEN AGAIN").clicked() {
                    change(&mut |x| x.topics.retain(|t| t != own));
                }
            });
        }
        ui.horizontal(|ui| {
            holo_ui::slot(ui, egui::TextEdit::singleline(&mut view.topic).hint_text("a word of your own").desired_width(200.0));
            let word = view.topic.trim().to_lowercase();
            if holo_ui::key(ui, "CLOSE IT").clicked() && !word.is_empty() {
                change(&mut |x| {
                    if !x.topics.contains(&word) {
                        x.topics.push(word.clone());
                    }
                });
                view.topic.clear();
            }
        });
    });
    holo_ui::fold(ui, ("devices", &a.name), &format!("{}'s phones and PCs", a.name), false, |ui| {
        if a.devices.is_empty() {
            ui.label(muted("None yet."));
        }
        for d in &a.devices {
            ui.label(label(&format!("{} · {}", d.kind, d.label), 8.5));
            let version = match d.version.as_str() {
                "" => String::new(),
                v if v == crate::fleet::BUILD => format!("  ·  J.A.I. {v}, as on this PC"),
                v => format!("  ·  J.A.I. {v}"),
            };
            let seen = if accounts::now() - d.seen < crate::fleet::ONLINE_WITHIN { "online now".to_string() } else { format!("seen {}", ago(d.seen)) };
            ui.label(muted(&format!("{seen}{version}")));
            if !d.stats.is_empty() {
                ui.label(RichText::new(&d.stats).font(crate::holo::font(Face::Mono, 11.0)).color(strip_ink()));
            }
        }
    });
    holo_ui::fold(ui, ("log", &a.name), &format!("What J.A.I. did for {} this week", a.name), false, |ui| {
        let done = match remote {
            Some(r) => r.log.iter().filter(|d| d.name.eq_ignore_ascii_case(&a.name)).cloned().collect(),
            None => accounts::log(&db, Some(&a.name), accounts::now() - 7 * 86_400),
        };
        if done.is_empty() {
            ui.label(muted("Nothing this week."));
        }
        for d in done.iter().rev().take(40) {
            let at = chrono::DateTime::from_timestamp(d.at, 0).map(|t| t.with_timezone(&chrono::Local).format("%d.%m. %H:%M").to_string()).unwrap_or_default();
            ui.label(label(&format!("{at} · {}", d.via), 8.0));
            ui.label(RichText::new(&d.asked).font(crate::holo::font(Face::Body, 13.0)).color(strip_ink()));
            ui.label(muted(&d.answered.chars().take(300).collect::<String>()));
            if !d.tools.is_empty() {
                ui.label(RichText::new(format!("ran {}", d.tools)).font(crate::holo::font(Face::Mono, 11.0)).color(strip_muted()));
            }
        }
    });
}

/// Registers the list while it is open (a window of its own; a block on the board in a session
/// without windows).
pub(super) fn show(ctx: &egui::Context) {
    let viewport = ViewportId::from_hash_of("jai_accounts");
    if !super::detached_keep(ctx, viewport, OPEN.load(Ordering::Acquire)) {
        crate::hud_dock::forget(ID);
        return;
    }
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [620.0, 640.0], body).close {
            OPEN.store(false, Ordering::Release);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Accounts — J.A.I.").with_app_id("jai").with_inner_size([640.0, 700.0]).with_min_inner_size([460.0, 320.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("accounts", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!OPEN.load(Ordering::Acquire) && !super::detached_leaving(ctx)) {
                OPEN.store(false, Ordering::Release);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new("Accounts").show(ctx, body);
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        OPEN.store(false, Ordering::Release);
                    }
                    body(ui);
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
    use serde_json::json;

    /// The list with one account unfolded: its password key, its pause and REMOVE keys, every
    /// tool with its options one by one, nothing cut off or drawn over.
    #[test]
    fn the_accounts_list_shows_every_switch() {
        let _look = crate::holo_theme::look_for_pictures();
        let dir = std::env::temp_dir().join(format!("jai_accounts_view_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("jai_memory.db").to_string_lossy().to_string();
        let mut max = accounts::for_voice(&db, "Max", "").unwrap();
        max.set("open_jai_view:view=earth", true);
        max.set("open_jai_view:action=show_satellites", false);
        max.topics.push("maths".into());
        max.devices.push(accounts::Device { id: "a1b2c3d4".into(), kind: "pc".into(), label: "Musterstadt-PC (Windows 11)".into(), version: "3.1.0".into(), stats: "CPU 12 % · RAM 7.9 / 32 GB · RTX 4060 8 GB".into(), seen: accounts::now() - 120 });
        accounts::save(&db, &max).unwrap();
        accounts::make_admin(&db, &crate::vault::Vault::in_memory(), "Mustermann", "geheim123").unwrap();
        let definitions = vec![json!({"type": "function", "function": {"name": "open_jai_view", "description": "Opens one of his views. More.", "parameters": {"type": "object", "properties": {"view": {"type": "string", "enum": ["earth", "disk_map"]}, "action": {"type": "string", "description": "earth: show, show_satellites"}}}}})];
        *lock() = Some(View { db: db.clone(), grants: accounts::grants(&definitions), chosen: "Max".into(), ..Default::default() });
        let mut stage = Stage::new(egui::vec2(640.0, 900.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("accounts").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(600.0, 860.0)).show(ctx, body);
        });
        stage.raster(&frame).save("panel_accounts");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        for shown in ["MAX", "PASSWORD", "SHOW", "PAUSE 1 H", "REMOVE", "open_jai_view: Opens one of his views", "Everything, as Sir may (this PC included)", "pc_power: Shut down, restart, sleep or lock this PC", "WINDOWS ON MAX'S PC", "Mustermann  ·  your admin account"] {
            assert!(!frame.find_text(shown).is_empty(), "'{shown}' is not in the list");
        }
        *lock() = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
