//! The accounts list ("open the accounts list"): everyone besides Sir who may use J.A.I., with
//! their passwords; what each may do on their own PC and, apart, on Sir's (Sir, 2026-11-20: "the
//! window where i can give and take permissions to be more clearly laid out"): talk only, what
//! he chose, or everything, then every window and every tool by the Feature Lab's groups, in
//! words, each value of its options one by one (the Earth, but not its satellites); the topics
//! closed to them; a pause; their phones and PCs with what those PCs run; and what J.A.I. did for
//! them. Only Sir opens it (`accounts::run`). On another PC of his, in admin mode, it shows the
//! accounts at his main PC and changes them there (`admin::remote_accounts`).

use super::{strip_ink, strip_muted};
use crate::accounts::{self, Account, Grant, Switches, TOPICS};
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
    /// What can be allowed, by the Feature Lab's groups.
    groups: Vec<(&'static str, Vec<Grant>)>,
    /// The account unfolded.
    chosen: String,
    /// The features searched for: on their PC, on Sir's.
    search: [String; 2],
    /// Where CHOSEN was pressed while nothing is switched on yet: (account, on its own PC).
    choosing: Vec<(String, bool)>,
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
    let definitions = crate::tools::definitions_with(Vec::new());
    let groups = grouped(&definitions, accounts::grants(&definitions));
    let mut view = lock();
    let chosen = view.as_ref().filter(|v| v.db == db && v.remote == remote).map(|v| v.chosen.clone()).unwrap_or_default();
    *view = Some(View { db, remote, groups, chosen, ..Default::default() });
    OPEN.store(true, Ordering::Release);
}

/// The tools under the Feature Lab's groups (`features_view::catalogue`), the PC's power last.
fn grouped(definitions: &[serde_json::Value], grants: Vec<Grant>) -> Vec<(&'static str, Vec<Grant>)> {
    let catalogue = super::features_view::catalogue(definitions);
    let mut groups: Vec<(&'static str, Vec<Grant>)> = catalogue.iter().map(|(name, _)| (*name, Vec::new())).collect();
    groups.push(("Power and lock", Vec::new()));
    let last = groups.len() - 1;
    for g in grants {
        // (A tool without a schema of the model's stands under its category all the same: "System".)
        let named = || crate::tools::JAI_CAPABILITY_MANIFEST.iter().find(|c| c.tools.contains(&g.tool.as_str())).map(|c| c.name);
        let i = catalogue.iter().position(|(_, tools)| tools.iter().any(|(tool, _)| *tool == g.tool)).or_else(|| {
            let name = named()?;
            groups.iter().position(|(n, _)| *n == name).or_else(|| {
                groups.push((name, Vec::new()));
                Some(groups.len() - 1)
            })
        });
        let i = i.unwrap_or(last);
        groups[i].1.push(g);
    }
    groups.retain(|(_, g)| !g.is_empty());
    groups
}

/// One place's switches of an account: its own PC's, or Sir's PC's.
fn switches(a: &mut Account, own: bool) -> &mut Switches {
    if own {
        &mut a.own
    } else {
        &mut a.here
    }
}

/// A place's level in a word or two, for the list.
fn level_word(s: &Switches) -> String {
    if s.everything() {
        "everything".into()
    } else if s.allow.is_empty() {
        "talks only".into()
    } else {
        format!("{} chosen", s.allow.len())
    }
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
    let may = format!("their PC: {}  ·  yours: {}", level_word(&a.own), level_word(&a.here));
    let state = a.paused().map_or(may, |until| format!("paused until {until}"));
    let seen = a.devices.first().map(|d| format!("  ·  seen {}", ago(d.seen))).unwrap_or_default();
    // (Which J.A.I. their PC runs, at a glance: is it the newest?)
    let version = a.devices.iter().find(|d| d.kind == "pc" && !d.version.is_empty()).map(|d| format!("  ·  J.A.I. {}", d.version)).unwrap_or_default();
    format!("{}  ·  {state}{seen}{version}", a.name)
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
    ui.add_space(8.0);
    for own in [true, false] {
        place(ui, view, a, own, &change);
    }
    holo_ui::titled_plate(ui, &format!("Topics closed to {}", a.name), "03", |ui| {
        ui.label(muted("On their PC and on yours: a closed topic gets no answer, from the language model either."));
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
    holo_ui::titled_plate(ui, &format!("{}'s phones and PCs", a.name), "04", |ui| {
        if a.devices.is_empty() {
            ui.label(muted("None yet."));
        }
        for d in &a.devices {
            ui.label(label(&format!("{} · {}", d.kind, d.label), 8.5));
            let version = match d.version.as_str() {
                "" => String::new(),
                v if v == crate::fleet::release() => format!("  ·  J.A.I. {v}, as on this PC"),
                v => format!("  ·  J.A.I. {v}"),
            };
            let seen = if accounts::now() - d.seen < crate::fleet::ONLINE_WITHIN { "online now".to_string() } else { format!("seen {}", ago(d.seen)) };
            ui.label(muted(&format!("{seen}{version}")));
            if !d.stats.is_empty() {
                ui.label(RichText::new(&d.stats).font(crate::holo::font(Face::Mono, 11.0)).color(strip_ink()));
            }
        }
    });
    holo_ui::titled_plate(ui, &format!("What J.A.I. did for {} this week", a.name), "05", |ui| {
        let done = match remote {
            Some(r) => r.log.iter().filter(|d| d.name.eq_ignore_ascii_case(&a.name)).cloned().collect(),
            None => accounts::log(&db, Some(&a.name), accounts::now() - 7 * 86_400),
        };
        if done.is_empty() {
            ui.label(muted("Nothing this week."));
        }
        const SHOWN: usize = 12;
        for d in done.iter().rev().take(SHOWN) {
            let at = chrono::DateTime::from_timestamp(d.at, 0).map(|t| t.with_timezone(&chrono::Local).format("%d.%m. %H:%M").to_string()).unwrap_or_default();
            ui.label(label(&format!("{at} · {}", d.via), 8.0));
            ui.label(RichText::new(&d.asked).font(crate::holo::font(Face::Body, 13.0)).color(strip_ink()));
            ui.label(muted(&d.answered.chars().take(300).collect::<String>()));
            if !d.tools.is_empty() {
                ui.label(RichText::new(format!("ran {}", d.tools)).font(crate::holo::font(Face::Mono, 11.0)).color(strip_muted()));
            }
        }
        if done.len() > SHOWN {
            ui.label(muted(&format!("… and {} earlier this week (\"what did you do for {} this week\" lists them all).", done.len() - SHOWN, a.name)));
        }
    });
}

/// What an account may in one place, on a plate of its own: on its own PC (`own`; its windows
/// too) or on Sir's. TALK ONLY, CHOSEN or EVERYTHING, then the windows and the features by group,
/// each in words (its tool's name under the pointer).
fn place(ui: &mut Ui, view: &mut View, a: &Account, own: bool, change: &dyn Fn(&mut dyn FnMut(&mut Account))) {
    let s = if own { &a.own } else { &a.here };
    let pick = (a.name.to_lowercase(), own);
    let level = if s.everything() {
        2
    } else if !s.allow.is_empty() || view.choosing.contains(&pick) {
        1
    } else {
        0
    };
    let (title, index) = if own { (format!("On {}'s PC", a.name), "01") } else { ("On your PC".to_string(), "02") };
    holo_ui::titled_plate(ui, &title, index, |ui| {
        ui.horizontal(|ui| {
            for (i, text) in ["TALK ONLY", "CHOSEN", "EVERYTHING"].into_iter().enumerate() {
                if holo_ui::key_on(ui, text, level == i).clicked() && level != i {
                    view.choosing.retain(|p| *p != pick);
                    match i {
                        1 => view.choosing.push(pick.clone()),
                        2 => change(&mut |x| *switches(x, own) = Switches { allow: vec!["*".into()], deny: Vec::new() }),
                        _ => {}
                    }
                    if i == 0 || level == 2 {
                        change(&mut |x| *switches(x, own) = Switches::default());
                    }
                }
            }
        });
        let name = &a.name;
        ui.label(muted(&match (own, level) {
            (true, 0) => format!("On their own PC, {name} talks to J.A.I. and nothing more."),
            (true, 1) => format!("On their own PC, {name} may what is switched on below; any other window asks for your admin sign-in."),
            (true, _) => format!("Everything you can do, on {name}'s PC alone: every feature and window there, the settings, the Feature Lab and Discord too. Your admin window, the accounts and orders to other PCs stay yours."),
            (false, 0) => format!("Through your phone page or by voice at your PC, {name} talks to J.A.I. and nothing more."),
            (false, 1) => format!("Through your phone page or by voice at your PC, {name} may what is switched on below, and never your own windows."),
            (false, _) => format!("Everything on your PC but your own windows (the accounts, the settings, the labs, Discord) and your other PCs."),
        }));
        if level == 0 {
            return;
        }
        if own {
            holo_ui::head(ui, "Windows");
            let windows: Vec<&admin::Window> = admin::WINDOWS.iter().filter(|w| !admin::FLEET.contains(&w.id)).collect();
            ui.columns(2, |columns| {
                for (i, w) in windows.iter().enumerate() {
                    let ui = &mut columns[i % 2];
                    let key = admin::key(w.id);
                    let mut on = s.allows(&key) && (!w.sirs || s.everything());
                    ui.add_enabled_ui(!w.sirs || s.everything(), |ui| {
                        if holo_ui::toggle(ui, &mut on, w.title).on_hover_text(w.about).on_disabled_hover_text("Only with EVERYTHING on their PC.").changed() {
                            change(&mut |x| switches(x, own).set(&key, on));
                        }
                    });
                }
            });
        }
        holo_ui::head(ui, "Features");
        let searched = &mut view.search[usize::from(!own)];
        holo_ui::slot(ui, egui::TextEdit::singleline(searched).hint_text("search the features").desired_width(f32::INFINITY));
        let search = searched.trim().to_lowercase();
        for (group, grants) in &view.groups {
            let found: Vec<&Grant> = grants.iter().filter(|g| search.is_empty() || g.tool.contains(&search) || g.about.to_lowercase().contains(&search) || g.values.iter().any(|v| v.contains(&search))).collect();
            if found.is_empty() {
                continue;
            }
            // (A tool counts when it, or one value of its options, is allowed.)
            let on = grants.iter().filter(|g| s.allows(&g.tool) || g.values.iter().any(|v| s.allows(&format!("{}:{v}", g.tool)))).count();
            // (Open at first where something was switched one by one: allowed, or taken back.)
            let set = |tool: &str| s.allow.iter().chain(&s.deny).any(|k| k.split(':').next() == Some(tool));
            holo_ui::fold(ui, ("features", own, &a.name, *group), &format!("{group}  ·  {on} of {}", grants.len()), !search.is_empty() || grants.iter().any(|g| set(&g.tool)), |ui| {
                for g in found {
                    let mut on = s.allows(&g.tool);
                    let said = if g.about.is_empty() { g.tool.replace('_', " ") } else { g.about.clone() };
                    if holo_ui::toggle(ui, &mut on, said).on_hover_text(&g.tool).changed() {
                        change(&mut |x| switches(x, own).set(&g.tool, on));
                    }
                    if g.values.is_empty() {
                        continue;
                    }
                    ui.indent(("values", own, &g.tool), |ui| {
                        let one_by_one = s.allow.iter().chain(&s.deny).any(|k| k.starts_with(&format!("{}:", g.tool)));
                        holo_ui::fold(ui, ("values", own, &a.name, &g.tool), &format!("{} options", g.values.len()), one_by_one || !search.is_empty(), |ui| {
                            for value in &g.values {
                                let key = format!("{}:{value}", g.tool);
                                let mut on = s.allows(&key);
                                // In words ("view=disk_map" is "View: disk map"); the key itself on hover.
                                let words = crate::pim::capitalise(&value.replace('=', ": ").replace('_', " "));
                                if holo_ui::toggle(ui, &mut on, words).on_hover_text(value).changed() {
                                    change(&mut |x| switches(x, own).set(&key, on));
                                }
                            }
                        });
                    });
                }
            });
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

    /// The list with one account unfolded: its password key, its pause and REMOVE keys, its two
    /// places apart (everything but Discord on its own PC; on Sir's, the Earth without its
    /// satellites, the options one by one), nothing cut off or drawn over.
    #[test]
    fn the_accounts_list_shows_every_switch() {
        let _look = crate::holo_theme::look_for_pictures();
        let dir = std::env::temp_dir().join(format!("jai_accounts_view_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("jai_memory.db").to_string_lossy().to_string();
        let mut max = accounts::for_voice(&db, "Max", "").unwrap();
        max.own.allow = vec!["*".into()];
        max.own.set(&admin::key("discord"), false);
        max.here = Switches::default();
        max.here.set("open_jai_view:view=earth", true);
        max.here.set("open_jai_view:action=show_satellites", false);
        max.topics.push("maths".into());
        max.devices.push(accounts::Device { id: "a1b2c3d4".into(), kind: "pc".into(), label: "Musterstadt-PC (Windows 11)".into(), version: "3.1.0".into(), stats: "CPU 12 % · RAM 7.9 / 32 GB · RTX 4060 8 GB".into(), seen: accounts::now() - 120 });
        accounts::save(&db, &max).unwrap();
        accounts::make_admin(&db, &crate::vault::Vault::in_memory(), "Mustermann", "geheim123").unwrap();
        let definitions = vec![json!({"type": "function", "function": {"name": "open_jai_view", "description": "Opens one of his views. More.", "parameters": {"type": "object", "properties": {"view": {"type": "string", "enum": ["earth", "disk_map"]}, "action": {"type": "string", "description": "earth: show, show_satellites"}}}}})];
        *lock() = Some(View { db: db.clone(), groups: grouped(&definitions, accounts::grants(&definitions)), chosen: "Max".into(), ..Default::default() });
        let mut stage = Stage::new(egui::vec2(640.0, 1500.0));
        let frame = stage.frames(4, &mut |ctx| {
            egui::Window::new("accounts").title_bar(false).fixed_pos(egui::pos2(10.0, 10.0)).fixed_size(egui::vec2(600.0, 1460.0)).show(ctx, body);
        });
        stage.raster(&frame).save("panel_accounts");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
        assert_eq!(visual::text_over_text(&frame), Vec::<String>::new());
        let row = "Max  ·  their PC: everything  ·  yours: 1 chosen";
        for shown in [row,"PASSWORD", "SHOW", "PAUSE 1 H", "REMOVE", "ON MAX'S PC", "ON YOUR PC", "TALK ONLY", "CHOSEN", "EVERYTHING", "Feature Lab", "Discord", "Opens one of his views", "View: earth", "Action: show satellites", "TOPICS CLOSED TO MAX", "Musterstadt-PC", "Mustermann  ·  your admin account"] {
            assert!(!frame.find_text(shown).is_empty(), "'{shown}' is not in the list");
        }
        *lock() = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
