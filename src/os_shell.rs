//! J.A.I.OS's desktop as `jai --shell` draws it: the taskbar (the [J] key, the open windows by
//! program, the tray, his lamp, the clock), the [J] menu (a search line that also takes an
//! order for him, favourites, the programs by group, the power keys) and the desktop (the
//! drafting table with the icons of the Desktop folder). What is where and what a click means
//! is decided here, in points, and painted with his own kit; `os_wayland` gives each piece a
//! surface, and the tests draw them offscreen.

use crate::holo::{self, a, lit, Face};
use crate::holo_block::{self, Lamp};
use crate::os_apps::{self, App};
use crate::{holo_frame, holo_kit};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The pictures of programs, tray items and files, by the key each is asked for with.
pub type Icons = HashMap<String, Arc<egui::ColorImage>>;

/// The picture `key` as a texture of this context (each surface has its own context, and a
/// texture belongs to the one that loaded it).
fn icon_texture(ctx: &egui::Context, icons: &Icons, key: &str) -> Option<egui::TextureId> {
    let image = icons.get(key)?;
    let id = egui::Id::new(("os-icon", key, Arc::as_ptr(image) as usize));
    if let Some(texture) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(texture.id());
    }
    let texture = ctx.load_texture(key, egui::ImageData::Color(image.clone()), egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    Some(texture.id())
}

/// The picture `key` in `r`; without one, the first letter of `name` in a small frame.
pub(crate) fn paint_icon(p: &egui::Painter, icons: &Icons, key: &str, name: &str, r: Rect, alpha: f32) {
    let t = holo::theme();
    match icon_texture(p.ctx(), icons, key) {
        Some(texture) => drop(p.image(texture, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), a(Color32::WHITE, alpha))),
        None => {
            let r = r.shrink(r.width() * 0.08);
            holo::outline(p, &holo::plate(r, [0.0, 0.0, r.width() * 0.25, 0.0]), a(t.ink(), 0.4 * alpha));
            let letter = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            holo::text(p, r.center(), Align2::CENTER_CENTER, Face::Head, &letter, r.height() * 0.58, a(t.ink(), 0.85 * alpha));
        }
    }
}

/// A tray item's picture as it comes over the bus (`IconPixmap`: ARGB, the highest byte first).
pub fn pixmap_image(width: i32, height: i32, argb: &[u8]) -> Option<egui::ColorImage> {
    let (w, h) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    if w == 0 || h == 0 || w > 1024 || h > 1024 || argb.len() != w * h * 4 {
        return None;
    }
    let rgba: Vec<u8> = argb.chunks_exact(4).flat_map(|p| [p[1], p[2], p[3], p[0]]).collect();
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

// ------------------------------------------------------------------------------- taskbar ----

/// The taskbar's height in points.
pub const BAR_H: f32 = 44.0;
const TASK_MIN: f32 = 46.0;
const TASK_MAX: f32 = 190.0;
const TRAY_CELL: f32 = 28.0;

/// A surface's buffer for its size in points at a scale in 120ths (the fractional-scale
/// protocol's unit: 150 is 125 %), rounded half up as that protocol says.
pub fn buffer_size(points: (u32, u32), scale120: u32) -> (u32, u32) {
    let px = |v: u32| ((v * scale120 + 60) / 120).max(1);
    (px(points.0), px(points.1))
}

/// One open window, as the taskbar needs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Task {
    pub uuid: String,
    pub title: String,
    /// What groups it: its program's menu entry, else the id KWin names it by.
    pub app: String,
    /// The program's name, where the menu knows it.
    pub name: String,
    pub active: bool,
    pub minimised: bool,
}

/// The windows of one program: one key of the taskbar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    pub app: String,
    pub name: String,
    pub windows: Vec<Task>,
}

/// The windows by program. `order` keeps each program where it first stood (KWin lists the
/// windows from the front one back, which changes with every click).
pub fn groups(tasks: &[Task], order: &mut Vec<String>) -> Vec<Group> {
    order.retain(|app| tasks.iter().any(|t| t.app == *app));
    let mut fresh: Vec<&Task> = tasks.iter().filter(|t| !order.contains(&t.app)).collect();
    fresh.sort_by(|x, y| x.uuid.cmp(&y.uuid));
    for task in fresh {
        if !order.contains(&task.app) {
            order.push(task.app.clone());
        }
    }
    order
        .iter()
        .map(|app| {
            let mut windows: Vec<Task> = tasks.iter().filter(|t| t.app == *app).cloned().collect();
            windows.sort_by(|x, y| x.uuid.cmp(&y.uuid));
            let name = windows.iter().map(|w| w.name.as_str()).find(|n| !n.is_empty()).unwrap_or(&windows[0].title).to_string();
            Group { app: app.clone(), name, windows }
        })
        .collect()
}

/// What the taskbar asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum BarAction {
    Menu,
    Activate(String),
    Minimise(String),
    /// Another window of the program (a middle click on its key).
    Launch(String),
    Tray(usize, TrayClick),
    /// His lamp: the HUD comes up.
    Lamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayClick {
    Activate,
    Secondary,
    Menu,
}

/// What a click on a program's key does: its one window comes to the front, or steps back
/// when it is the front one; of several, the one after the front one is next.
pub fn click(group: &Group) -> BarAction {
    let front = group.windows.iter().position(|w| w.active);
    match (group.windows.len(), front) {
        (1, Some(_)) => BarAction::Minimise(group.windows[0].uuid.clone()),
        (n, Some(k)) => BarAction::Activate(group.windows[(k + 1) % n].uuid.clone()),
        _ => BarAction::Activate(group.windows[0].uuid.clone()),
    }
}

/// One item of the tray.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrayItem {
    /// Names its picture in the icons.
    pub key: String,
    pub title: String,
    /// It asks to be looked at.
    pub attention: bool,
}

/// Where the taskbar's parts stand, in points from its top left corner.
#[derive(Debug, Clone, PartialEq)]
pub struct BarLayout {
    pub j: Rect,
    pub tasks: Vec<Rect>,
    /// Where the count of the keys that found no room stands (and how many those are).
    pub more: Option<(Rect, usize)>,
    pub tray: Vec<Rect>,
    pub lamp: Rect,
    pub clock: Rect,
}

/// The taskbar of a monitor `width` points wide with `tasks` program keys and `tray` items.
/// The keys share the room between the [J] key and the tray; when they would get narrower
/// than an icon, the last ones give way to a count.
pub fn bar_layout(width: f32, tasks: usize, tray: usize) -> BarLayout {
    let j = Rect::from_min_size(pos2(10.0, BAR_H / 2.0 - 14.0), vec2(52.0, 28.0));
    let clock = Rect::from_min_max(pos2(width - 88.0, 0.0), pos2(width - 10.0, BAR_H));
    let lamp = Rect::from_min_max(pos2(clock.left() - 124.0, 0.0), pos2(clock.left() - 8.0, BAR_H));
    let tray_left = lamp.left() - 8.0 - tray as f32 * TRAY_CELL;
    let tray = (0..tray).map(|i| Rect::from_min_size(pos2(tray_left + i as f32 * TRAY_CELL, BAR_H / 2.0 - TRAY_CELL / 2.0), Vec2::splat(TRAY_CELL))).collect();
    let (left, right) = (j.right() + 14.0, tray_left - 12.0);
    let room = (right - left).max(0.0);
    let fit = (room / TASK_MIN).floor() as usize;
    let shown = if tasks <= fit { tasks } else { fit.saturating_sub(1) };
    let key = if shown == 0 { 0.0 } else { ((room - if shown < tasks { TASK_MIN } else { 0.0 }) / shown as f32).min(TASK_MAX) };
    let at = |i: usize, w: f32| Rect::from_min_size(pos2(left + i as f32 * key, 6.0), vec2(w - 4.0, BAR_H - 12.0));
    let more = (shown < tasks && fit > 0).then(|| (at(shown, TASK_MIN), tasks - shown));
    BarLayout { j, tasks: (0..shown).map(|i| at(i, key)).collect(), more, tray, lamp, clock }
}

/// What the taskbar shows.
pub struct Bar<'a> {
    pub groups: &'a [Group],
    pub tray: &'a [TrayItem],
    pub icons: &'a Icons,
    /// What he is doing ("idle", "listening", "thinking", "speaking"); empty while he is not there.
    pub state: &'a str,
    /// The time ("14:05") and the day ("WED 25 NOV").
    pub clock: (&'a str, &'a str),
    pub menu_open: bool,
}

/// His lamp and its word for a state of his.
pub fn lamp_of(state: &str) -> (Lamp, &'static str) {
    match state {
        "" => (Lamp::Rest, "OFFLINE"),
        "listening" => (Lamp::Busy, "LISTENING"),
        "thinking" => (Lamp::Busy, "THINKING"),
        "speaking" => (Lamp::Busy, "SPEAKING"),
        _ => (Lamp::Good, "READY"),
    }
}

/// Draws the taskbar over the whole context and says what was clicked.
pub fn paint_bar(ctx: &egui::Context, bar: &Bar) -> Option<BarAction> {
    let t = holo::theme();
    let mut action = None;
    egui::CentralPanel::default().frame(egui::Frame::none().fill(t.void())).show(ctx, |ui| {
        let screen = ui.max_rect();
        let layout = bar_layout(screen.width(), bar.groups.len(), bar.tray.len());
        let p = ui.painter().clone();
        // Black glass a breath lighter at its top edge, under one hairline.
        p.add(egui::Shape::mesh({
            let mut glass = egui::Mesh::default();
            glass.colored_vertex(screen.left_top(), a(t.ink(), 0.06));
            glass.colored_vertex(screen.right_top(), a(t.ink(), 0.06));
            glass.colored_vertex(screen.right_bottom(), a(t.ink(), 0.015));
            glass.colored_vertex(screen.left_bottom(), a(t.ink(), 0.015));
            glass.add_triangle(0, 1, 2);
            glass.add_triangle(0, 2, 3);
            glass
        }));
        holo::rule(&p, screen.left_top(), screen.right_top(), a(t.ink(), 0.30));

        let j = ui.interact(layout.j, ui.id().with("j"), Sense::click());
        holo_kit::key(&p, layout.j, "J", j.hovered() as u8 as f32, j.is_pointer_button_down_on(), bar.menu_open, 1.0);
        if j.clicked() {
            action = Some(BarAction::Menu);
        }

        for (i, (group, r)) in bar.groups.iter().zip(&layout.tasks).enumerate() {
            let key = ui.interact(*r, ui.id().with(("task", i)), Sense::click());
            let active = group.windows.iter().any(|w| w.active);
            let resting = group.windows.iter().all(|w| w.minimised);
            if active {
                // The front program: a faint pane under a lit filament along its top.
                p.rect_filled(*r, 0.0, a(t.accent(), 0.10));
                let mut light = holo::Light::new(&p);
                light.neon(&[r.left_top(), r.right_top()], false, 1.4, lit(t.accent(), 0.9), 5.0);
                light.paint(&p);
            } else if key.hovered() {
                p.rect_filled(*r, 0.0, a(t.ink(), 0.07));
            }
            let icon = Rect::from_center_size(pos2(r.left() + 19.0, r.center().y - 1.0), Vec2::splat(22.0));
            paint_icon(&p, bar.icons, &group.app, &group.name, icon, if resting { 0.55 } else { 1.0 });
            let room = r.right() - 8.0 - (icon.right() + 8.0);
            if room > 28.0 {
                let title = if group.windows.len() == 1 && !group.windows[0].title.is_empty() { &group.windows[0].title } else { &group.name };
                let colour = if active { t.ink() } else { holo::mix(t.muted(), t.ink(), if resting { 0.1 } else { 0.55 }) };
                let g = holo::galley_within(&p, Face::Body, title, 12.5, colour, room);
                p.galley(pos2(icon.right() + 8.0, holo::snap(r.center().y - 1.0 - g.size().y / 2.0, holo::ppp(&p))), g, colour);
            }
            // One tick per window along the bottom: the front one in the accent.
            for (k, w) in group.windows.iter().take(6).enumerate() {
                let x = r.left() + 8.0 + k as f32 * 9.0;
                let colour = if w.active { t.accent() } else { a(t.ink(), if w.minimised { 0.25 } else { 0.6 }) };
                holo::rule(&p, pos2(x, r.bottom() - 1.0), pos2(x + 6.0, r.bottom() - 1.0), colour);
            }
            if key.clicked() {
                action = Some(click(group));
            } else if key.clicked_by(egui::PointerButton::Middle) {
                action = Some(BarAction::Launch(group.app.clone()));
            }
        }
        if let Some((r, count)) = layout.more {
            holo::text(&p, r.center(), Align2::CENTER_CENTER, Face::Figure, &format!("+{count}"), 14.0, t.muted());
        }

        for (i, (item, r)) in bar.tray.iter().zip(&layout.tray).enumerate() {
            let cell = ui.interact(*r, ui.id().with(("tray", i)), Sense::click());
            if cell.hovered() {
                p.rect_filled(*r, 0.0, a(t.ink(), 0.07));
            }
            paint_icon(&p, bar.icons, &item.key, &item.title, Rect::from_center_size(r.center(), Vec2::splat(20.0)), 1.0);
            if item.attention {
                holo::rule(&p, pos2(r.left() + 5.0, r.bottom() + 2.0), pos2(r.right() - 5.0, r.bottom() + 2.0), t.accent());
            }
            for (button, click) in [(egui::PointerButton::Primary, TrayClick::Activate), (egui::PointerButton::Middle, TrayClick::Secondary), (egui::PointerButton::Secondary, TrayClick::Menu)] {
                if cell.clicked_by(button) {
                    action = Some(BarAction::Tray(i, click));
                }
            }
        }

        // His lamp and what he is doing, in a word.
        let (lamp, word) = lamp_of(bar.state);
        let lamp_key = ui.interact(layout.lamp, ui.id().with("lamp"), Sense::click());
        if lamp_key.hovered() {
            p.rect_filled(layout.lamp.shrink2(vec2(0.0, 6.0)), 0.0, a(t.ink(), 0.07));
        }
        holo::rule(&p, pos2(layout.lamp.left(), 12.0), pos2(layout.lamp.left(), BAR_H - 12.0), a(t.ink(), 0.16));
        let moving = holo_block::paint_lamp(&p, pos2(layout.lamp.left() + 16.0, layout.lamp.center().y), lamp, ui.input(|i| i.time), 1.0);
        let colour = match lamp {
            Lamp::Busy => t.accent(),
            Lamp::Rest => t.faint(),
            _ => t.muted(),
        };
        holo::text(&p, pos2(layout.lamp.left() + 30.0, layout.lamp.center().y), Align2::LEFT_CENTER, Face::Label, word, 8.5, colour);
        if moving {
            ctx.request_repaint_after(std::time::Duration::from_millis(110));
        }
        if lamp_key.clicked() {
            action = Some(BarAction::Lamp);
        }

        holo::rule(&p, pos2(layout.clock.left(), 12.0), pos2(layout.clock.left(), BAR_H - 12.0), a(t.ink(), 0.16));
        holo::text(&p, pos2(layout.clock.right(), 8.0), Align2::RIGHT_TOP, Face::Figure, bar.clock.0, 16.0, t.ink());
        holo::text(&p, pos2(layout.clock.right(), BAR_H - 8.0), Align2::RIGHT_BOTTOM, Face::Label, bar.clock.1, 8.0, t.muted());
    });
    action
}

// ------------------------------------------------------------------------------ [J] menu ----

/// The menu's size in points.
pub const MENU_SIZE: Vec2 = vec2(620.0, 600.0);
/// The programs the menu pins while Sir has pinned none.
pub const FAVOURITES: [&str; 5] = ["firefox", "steam", "org.kde.dolphin", "org.kde.konsole", "org.kde.discover"];
const ROW: f32 = 34.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Lock,
    LogOut,
    Sleep,
    Restart,
    ShutDown,
}

impl Power {
    pub const ALL: [(Power, &'static str); 5] = [(Power::Lock, "LOCK"), (Power::LogOut, "LOG OUT"), (Power::Sleep, "SLEEP"), (Power::Restart, "RESTART"), (Power::ShutDown, "SHUT DOWN")];

    /// Ends Sir's session: its key is pressed twice.
    pub fn ends_session(self) -> bool {
        matches!(self, Power::LogOut | Power::Restart | Power::ShutDown)
    }
}

/// What the menu asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    Launch(String),
    /// What was typed is an order for J.A.I.
    Order(String),
    Power(Power),
    /// Pin (or unpin) a program among the favourites.
    Pin(String, bool),
    Close,
}

/// The open menu.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Menu {
    pub query: String,
    /// 0 the favourites, then the groups that hold a program.
    pub tab: usize,
    /// The row Enter takes.
    pub cursor: usize,
    /// The power key pressed once (it ends the session: the second press does it).
    pub armed: Option<Power>,
}

/// The favourites as programs: Sir's pins (the stock ones while he has none), those the PC has.
pub fn favourites<'a>(apps: &'a [App], pinned: &[String]) -> Vec<&'a App> {
    let stock: Vec<String> = FAVOURITES.iter().map(|id| id.to_string()).collect();
    let ids = if pinned.is_empty() { &stock[..] } else { pinned };
    ids.iter().filter_map(|id| os_apps::of_window(apps, id, "")).collect()
}

/// The pins after `id` was pinned or unpinned (the stock ones are written out by the first change).
pub fn pinned_after(apps: &[App], pinned: &[String], id: &str, on: bool) -> Vec<String> {
    let mut now: Vec<String> = favourites(apps, pinned).iter().map(|a| a.id.clone()).collect();
    now.retain(|p| p != id);
    if on {
        now.push(id.to_string());
    }
    now
}

/// What Enter does on the menu's rows: the program under the cursor, or, past the last one
/// (and whenever nothing matches), what was typed as an order for J.A.I.
pub fn enter(menu: &Menu, rows: &[&App]) -> Option<MenuAction> {
    match rows.get(menu.cursor) {
        Some(app) => Some(MenuAction::Launch(app.id.clone())),
        None if !menu.query.trim().is_empty() => Some(MenuAction::Order(menu.query.trim().to_string())),
        None => None,
    }
}

/// Draws the menu over the whole context and says what was chosen.
pub fn paint_menu(ctx: &egui::Context, menu: &mut Menu, apps: &[App], pinned: &[String], icons: &Icons) -> Option<MenuAction> {
    let t = holo::theme();
    let mut action = None;
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let p = ui.painter().clone();
        let laid = holo_frame::plate(&p, ui.max_rect().shrink(1.0), &holo_frame::Plate { index: "J", title: "PROGRAMS", focus: 1.0, clock: 10.0, ..Default::default() });
        let body = laid.body;
        let search = Rect::from_min_size(body.min, vec2(body.width(), 30.0));
        let keys = Rect::from_min_max(pos2(body.left(), body.bottom() - holo_kit::KEY_H), body.max);
        let middle = Rect::from_min_max(pos2(body.left(), search.bottom() + 12.0), pos2(body.right(), keys.top() - 12.0));

        // The search line: it has the keyboard for as long as the menu is open.
        let before = menu.query.clone();
        let mut line = ui.new_child(egui::UiBuilder::new().max_rect(search));
        let edit = egui::TextEdit::singleline(&mut menu.query).hint_text("Search, or tell J.A.I. what to do").desired_width(search.width() - 12.0).font(holo::font(Face::Body, 15.0));
        crate::holo_ui::slot(&mut line, edit).request_focus();
        if menu.query != before {
            menu.cursor = 0;
        }
        let (enter_key, escape, down, up) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp)));

        let searching = !menu.query.trim().is_empty();
        let groups = os_apps::grouped(apps);
        menu.tab = menu.tab.min(groups.len());
        let pins = favourites(apps, pinned);
        let rows: Vec<&App> = if searching {
            os_apps::search(apps, &menu.query)
        } else if menu.tab == 0 {
            pins.clone()
        } else {
            groups[menu.tab - 1].1.clone()
        };
        // (While he searches, the row after the last program is the order for J.A.I.)
        let count = rows.len() + searching as usize;
        if down {
            menu.cursor = (menu.cursor + 1).min(count.saturating_sub(1));
        }
        if up {
            menu.cursor = menu.cursor.saturating_sub(1);
        }
        menu.cursor = menu.cursor.min(count.saturating_sub(1));

        // Left: the favourites and the groups. Right: the programs.
        let tabs = Rect::from_min_size(middle.min, vec2(150.0, middle.height()));
        let list = Rect::from_min_max(pos2(tabs.right() + 14.0, middle.top()), middle.max);
        holo::rule(&p, pos2(tabs.right() + 7.0, middle.top()), pos2(tabs.right() + 7.0, middle.bottom()), a(t.ink(), 0.13));
        let mut column = ui.new_child(egui::UiBuilder::new().max_rect(tabs).layout(egui::Layout::top_down(egui::Align::Min)));
        column.spacing_mut().item_spacing.y = 2.0;
        for (i, name) in ["Favourites"].into_iter().chain(groups.iter().map(|(name, _)| *name)).enumerate() {
            if crate::holo_ui::item(&mut column, name, !searching && menu.tab == i).clicked() {
                (menu.tab, menu.cursor) = (i, 0);
                menu.query.clear();
            }
        }

        let mut side = ui.new_child(egui::UiBuilder::new().max_rect(list).layout(egui::Layout::top_down(egui::Align::Min)));
        side.spacing_mut().item_spacing.y = 0.0;
        holo_block::faded(&mut side, egui::ScrollArea::vertical().auto_shrink([false, false]), |ui| {
            let width = ui.available_width();
            for (i, app) in rows.iter().enumerate() {
                let (r, row) = ui.allocate_exact_size(vec2(width, ROW), Sense::click());
                let chosen = searching && menu.cursor == i;
                let is_pinned = pins.iter().any(|pin| pin.id == app.id);
                if ui.is_rect_visible(r) {
                    let p = ui.painter();
                    let text = Rect::from_min_max(pos2(r.left() + 30.0, r.top()), r.max);
                    holo_kit::row(p, text, "", &app.name, if is_pinned && (searching || menu.tab != 0) { "PINNED" } else { &app.generic }, chosen as u8 as f32, row.hovered() as u8 as f32, 1.0);
                    paint_icon(p, icons, &app.id, &app.name, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), Vec2::splat(22.0)), 1.0);
                }
                if row.clicked() {
                    action = Some(MenuAction::Launch(app.id.clone()));
                } else if row.secondary_clicked() {
                    action = Some(MenuAction::Pin(app.id.clone(), !is_pinned));
                }
            }
            if searching {
                let (r, row) = ui.allocate_exact_size(vec2(width, ROW), Sense::click());
                let chosen = menu.cursor == rows.len();
                holo_kit::row(ui.painter(), r, ">", &format!("Tell J.A.I.: {}", menu.query.trim()), "ENTER", chosen as u8 as f32, row.hovered() as u8 as f32, 1.0);
                if row.clicked() {
                    action = Some(MenuAction::Order(menu.query.trim().to_string()));
                }
            } else if rows.is_empty() {
                holo_kit::empty(ui.painter(), ui.available_rect_before_wrap(), "Nothing is pinned yet: a right click on a program pins it.", 1.0);
            }
        });

        // The power keys, from the right; one that ends the session asks to be pressed again.
        let mut right = keys.right();
        for (power, label) in Power::ALL.iter().rev() {
            let armed = menu.armed == Some(*power);
            let label = if armed { format!("CONFIRM {label}") } else { label.to_string() };
            let r = Rect::from_min_size(pos2(right - holo_kit::key_width(&p, &label), keys.top()), vec2(holo_kit::key_width(&p, &label), holo_kit::KEY_H));
            right = r.left() - 6.0;
            let key = ui.interact(r, ui.id().with(("power", *power as u8)), Sense::click());
            holo_kit::key_tinted(&p, r, &label, key.hovered() as u8 as f32, key.is_pointer_button_down_on(), false, 1.0, armed.then(|| t.alert()));
            if key.clicked() {
                if power.ends_session() && !armed {
                    menu.armed = Some(*power);
                } else {
                    action = Some(MenuAction::Power(*power));
                }
            }
        }

        if escape {
            action = Some(MenuAction::Close);
        } else if enter_key && action.is_none() {
            action = enter(menu, &rows);
        }
    });
    action
}

// ------------------------------------------------------------------------------- desktop ----

/// One icon of the desktop: a file, a folder or a program's entry in the Desktop folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeskItem {
    pub name: String,
    pub path: PathBuf,
    /// Names its picture in the icons.
    pub icon: String,
}

/// An icon's cell in points.
pub const CELL: Vec2 = vec2(104.0, 92.0);
const DESK_MARGIN: f32 = 22.0;

/// Where his core stands on a desktop: a square about its middle.
pub fn core_rect(screen: Rect) -> Rect {
    Rect::from_center_size(screen.center(), Vec2::splat((screen.width().min(screen.height()) * 0.44).clamp(200.0, 460.0)))
}

/// The cells of `count` desktop icons on `area`: down the columns from the top left, never
/// under the core (`clear`); what finds no cell is not shown.
pub fn icon_grid(area: Rect, clear: Rect, count: usize) -> Vec<Rect> {
    let inner = area.shrink(DESK_MARGIN);
    let (columns, rows) = ((inner.width() / CELL.x).floor() as usize, (inner.height() / CELL.y).floor() as usize);
    (0..columns)
        .flat_map(|c| (0..rows).map(move |r| Rect::from_min_size(inner.min + vec2(c as f32 * CELL.x, r as f32 * CELL.y), CELL)))
        .filter(|cell| !cell.intersects(clear))
        .take(count)
        .collect()
}

/// The Desktop folder a `user-dirs.dirs` file names (`XDG_DESKTOP_DIR="$HOME/Schreibtisch"`),
/// `home`/Desktop without one.
pub fn desktop_dir(user_dirs: &str, home: &str) -> PathBuf {
    user_dirs
        .lines()
        .filter_map(|line| line.trim().strip_prefix("XDG_DESKTOP_DIR="))
        .map(|value| value.trim_matches('"').replace("$HOME", home))
        .find(|dir| dir.starts_with('/'))
        .map_or_else(|| PathBuf::from(home).join("Desktop"), PathBuf::from)
}

/// The icon theme's name for a file of the Desktop folder, by its ending.
pub fn file_icon(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "bmp" => "image-x-generic",
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" => "audio-x-generic",
        "mp4" | "mkv" | "webm" | "avi" | "mov" => "video-x-generic",
        "pdf" => "application-pdf",
        "zip" | "7z" | "rar" | "gz" | "xz" | "zst" | "tar" => "package-x-generic",
        "sh" | "appimage" | "exe" => "application-x-executable",
        "odt" | "doc" | "docx" | "ods" | "xlsx" | "odp" | "pptx" => "x-office-document",
        _ => "text-x-generic",
    }
}

/// Draws the desktop over the whole context: the drafting table about the core's place and the
/// icons. A click chooses an icon (`selected`), a double click opens it: its number is returned.
pub fn paint_desktop(ctx: &egui::Context, items: &[DeskItem], icons: &Icons, selected: &mut Option<usize>) -> Option<usize> {
    let t = holo::theme();
    let mut opened = None;
    egui::CentralPanel::default().frame(egui::Frame::none().fill(t.void())).show(ctx, |ui| {
        let screen = ui.max_rect();
        let p = ui.painter().clone();
        let core = core_rect(screen);
        // The drafting table: the dot grid, the core's axes and its first range circle.
        holo::dot_field(&p, screen, 28.0, screen.center(), a(t.ink(), 0.10));
        let c = screen.center();
        let reach = core.width() / 2.0 + 18.0;
        for (from, to) in [(pos2(screen.left(), c.y), pos2(c.x - reach, c.y)), (pos2(c.x + reach, c.y), pos2(screen.right(), c.y)), (pos2(c.x, screen.top()), pos2(c.x, c.y - reach)), (pos2(c.x, c.y + reach), pos2(c.x, screen.bottom()))] {
            holo::rule(&p, from, to, a(t.ink(), 0.07));
        }
        let mut table = holo::Light::new(&p);
        table.ring(c, reach, 1.0, a(t.ink(), 0.13));
        table.ticks(c, reach, reach + 6.0, 0.0, std::f32::consts::TAU, 60, 1.0, a(t.ink(), 0.22));
        table.paint(&p);

        let back = ui.interact(screen, ui.id().with("desk"), Sense::click());
        let mut hit = false;
        for (i, (item, cell)) in items.iter().zip(icon_grid(screen, core, items.len())).enumerate() {
            let icon = ui.interact(cell.shrink(3.0), ui.id().with(("icon", i)), Sense::click());
            if *selected == Some(i) {
                holo_frame::card(&p, cell.shrink(3.0), 1.0, 1.0);
            }
            paint_icon(&p, icons, &item.icon, &item.name, Rect::from_center_size(pos2(cell.center().x, cell.top() + 34.0), Vec2::splat(44.0)), 1.0);
            let g = holo::galley_within(&p, Face::Body, &item.name, 12.5, t.ink(), cell.width() - 14.0);
            p.galley(pos2(holo::snap(cell.center().x - g.size().x / 2.0, holo::ppp(&p)), cell.top() + 64.0), g, t.ink());
            if icon.clicked() || icon.double_clicked() {
                (*selected, hit) = (Some(i), true);
            }
            if icon.double_clicked() {
                opened = Some(i);
            }
        }
        if back.clicked() && !hit {
            *selected = None;
        }
    });
    opened
}

/// Draws his core over the whole context (the desktop's own surface for it: the one piece of
/// the desktop that moves).
pub fn paint_core(ctx: &egui::Context, core: &crate::holo_core::Core) {
    egui::CentralPanel::default().frame(egui::Frame::none().fill(holo::theme().void())).show(ctx, |ui| {
        let r = ui.max_rect();
        let share = if core.style == crate::holo_core::CoreStyle::Simple { 0.36 } else { 0.30 };
        crate::holo_core::paint(ui.painter(), egui::Id::new("os-core"), core.style, r.center(), r.width().min(r.height()) * share, &core.look());
    });
}

/// A list a tray item or a key unfolds (a context menu): each row's words, whether it can be
/// chosen and whether it is ticked; an empty label is a rule.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListRow {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub ticked: bool,
    /// How deep it stands in a sub-menu.
    pub depth: u8,
}

/// The size a list of these rows takes.
pub fn list_size(rows: &[ListRow]) -> Vec2 {
    vec2(260.0, rows.iter().map(|r| if r.label.is_empty() { 9.0 } else { crate::holo_ui::ROW_H }).sum::<f32>() + 16.0)
}

/// Draws the list over the whole context; the id of the row that was clicked.
pub fn paint_list(ctx: &egui::Context, rows: &[ListRow]) -> Option<i32> {
    let t = holo::theme();
    let mut picked = None;
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let frame = ui.max_rect().shrink(1.0);
        holo_frame::card(ui.painter(), frame, 0.6, 1.0);
        let mut y = frame.top() + 7.0;
        for (i, row) in rows.iter().enumerate() {
            if row.label.is_empty() {
                holo::rule(ui.painter(), pos2(frame.left() + 8.0, y + 4.0), pos2(frame.right() - 8.0, y + 4.0), a(t.ink(), 0.16));
                y += 9.0;
                continue;
            }
            let r = Rect::from_min_size(pos2(frame.left() + 1.0, y), vec2(frame.width() - 2.0, crate::holo_ui::ROW_H));
            y += crate::holo_ui::ROW_H;
            let hit = ui.interact(r, ui.id().with(("row", i)), if row.enabled { Sense::click() } else { Sense::hover() });
            let p = ui.painter();
            if row.enabled && hit.hovered() {
                p.rect_filled(r, 0.0, a(t.ink(), 0.08));
            }
            let x = r.left() + 12.0 + row.depth as f32 * 14.0;
            if row.ticked {
                p.rect_filled(Rect::from_min_size(pos2(r.left(), r.top()), vec2(2.0, r.height())), 0.0, t.accent());
            }
            let colour = if !row.enabled { t.faint() } else if row.ticked { t.accent() } else { t.ink() };
            let g = holo::galley_within(p, Face::Body, &row.label, 13.0, colour, r.right() - 10.0 - x);
            p.galley(pos2(x, holo::snap(r.center().y - g.size().y / 2.0, holo::ppp(p))), g, colour);
            if hit.clicked() {
                picked = Some(row.id);
            }
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{clipped_text, text_over_text, Stage};
    use eframe::egui::Pos2;

    fn task(uuid: &str, app: &str, active: bool, minimised: bool) -> Task {
        Task { uuid: uuid.into(), title: format!("{app} window {uuid}"), app: app.into(), name: String::new(), active, minimised }
    }

    #[test]
    fn the_taskbar_fits_every_monitor_width_and_scale() {
        for width in [1024.0, 1280.0, 1366.0, 1920.0, 2560.0, 3440.0, 3840.0] {
            for tasks in [0, 1, 4, 12, 40, 90] {
                for tray in [0, 3, 9] {
                    let l = bar_layout(width, tasks, tray);
                    let what = format!("{width} wide, {tasks} keys, {tray} tray items");
                    let mut row = vec![l.j];
                    row.extend(&l.tasks);
                    row.extend(l.more.map(|(r, _)| r));
                    row.extend(&l.tray);
                    row.extend([l.lamp, l.clock]);
                    for pair in row.windows(2) {
                        assert!(pair[0].right() <= pair[1].left() + 0.01, "{what}: {:?} runs into {:?}", pair[0], pair[1]);
                    }
                    for r in &row {
                        assert!(r.left() >= 0.0 && r.right() <= width && r.top() >= 0.0 && r.bottom() <= BAR_H && r.width() > 0.0, "{what}: {r:?}");
                    }
                    // Every key is shown or counted; a key is never narrower than its icon.
                    assert_eq!(l.tasks.len() + l.more.map_or(0, |(_, n)| n), tasks, "{what}");
                    assert!(l.tasks.iter().all(|r| r.width() >= TASK_MIN - 4.5 && r.width() <= TASK_MAX), "{what}");
                }
            }
        }
        assert_eq!(bar_layout(1920.0, 3, 0).tasks[0].width(), TASK_MAX - 4.0);
        assert!(bar_layout(1280.0, 90, 3).more.is_some_and(|(_, hidden)| hidden > 60));
        // The buffer of a 1920-point bar at 100, 125, 150, 175 and 200 %.
        let sizes: Vec<_> = [120, 150, 180, 210, 240].into_iter().map(|scale| buffer_size((1920, BAR_H as u32), scale)).collect();
        assert_eq!(sizes, [(1920, 44), (2400, 55), (2880, 66), (3360, 77), (3840, 88)]);
        assert_eq!(buffer_size((1707, 44), 180), (2561, 66));
        assert_eq!(buffer_size((0, 0), 120), (1, 1));
        // The picture the rasteriser makes of a surface is that buffer, to the pixel.
        for scale in [120, 150, 180, 210, 240] {
            let canvas = crate::ui_snapshot::Snapshot::new(vec2(101.0, BAR_H)).render_scaled(1, scale as f32 / 120.0, |_| {});
            assert_eq!((canvas.width as u32, canvas.height as u32), buffer_size((101, BAR_H as u32), scale), "at {scale}/120");
        }
        // A tray picture off the bus: ARGB bytes become RGBA; a wrong length is no picture.
        let picture = pixmap_image(2, 1, &[255, 10, 20, 30, 0, 0, 0, 0]).expect("a picture");
        assert_eq!((picture.size, picture.pixels[0], picture.pixels[1]), ([2, 1], Color32::from_rgb(10, 20, 30), Color32::TRANSPARENT));
        assert_eq!((pixmap_image(2, 2, &[0; 8]), pixmap_image(-1, 1, &[]), pixmap_image(0, 0, &[])), (None, None, None));
    }

    #[test]
    fn windows_group_by_program_and_keep_their_place() {
        let mut order = Vec::new();
        let first = [task("b", "firefox", true, false), task("a", "konsole", false, false), task("c", "firefox", false, true)];
        let g = groups(&first, &mut order);
        assert_eq!(g.iter().map(|g| (g.app.as_str(), g.windows.len())).collect::<Vec<_>>(), [("konsole", 1), ("firefox", 2)]);
        assert_eq!(g[1].windows.iter().map(|w| w.uuid.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        // KWin now lists another window first, and a new program opened: nothing moves.
        let then = [task("d", "dolphin", true, false), task("c", "firefox", false, false), task("a", "konsole", false, false), task("b", "firefox", false, false)];
        assert_eq!(groups(&then, &mut order).iter().map(|g| g.app.as_str()).collect::<Vec<_>>(), ["konsole", "firefox", "dolphin"]);
        // A program whose last window closed gives its place up.
        assert_eq!(groups(&then[..2], &mut order).iter().map(|g| g.app.as_str()).collect::<Vec<_>>(), ["firefox", "dolphin"]);
        assert_eq!(groups(&[], &mut order), Vec::new());
        // The key is named by the program where the menu knows it, by the window otherwise.
        let named = Task { name: "Firefox".into(), ..task("x", "firefox", false, false) };
        assert_eq!(groups(&[named], &mut order)[0].name, "Firefox");
        assert_eq!(groups(&[task("x", "odd", false, false)], &mut order)[0].name, "odd window x");
    }

    #[test]
    fn a_click_on_a_key_brings_to_the_front_steps_back_or_goes_round() {
        let mut order = Vec::new();
        let one = |active| groups(&[task("a", "konsole", active, !active)], &mut Vec::new()).remove(0);
        assert_eq!(click(&one(false)), BarAction::Activate("a".into()));
        assert_eq!(click(&one(true)), BarAction::Minimise("a".into()));
        let mut three = |front: &str| groups(&["a", "b", "c"].map(|u| task(u, "firefox", u == front, false)), &mut order).remove(0);
        assert_eq!(click(&three("")), BarAction::Activate("a".into()));
        assert_eq!(click(&three("a")), BarAction::Activate("b".into()));
        assert_eq!(click(&three("c")), BarAction::Activate("a".into()));
    }

    #[test]
    fn the_menu_pins_searches_and_hands_the_rest_to_jai() {
        let apps = crate::os_apps::tests::sample();
        // The stock favourites this PC has, in their order; Sir's own pins once he has any.
        assert_eq!(favourites(&apps, &[]).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["firefox", "steam", "org.kde.dolphin", "org.kde.konsole"]);
        assert_eq!(favourites(&apps, &["org.kde.krita".into(), "gone".into()]).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["org.kde.krita"]);
        assert_eq!(pinned_after(&apps, &[], "org.kde.krita", true), ["firefox", "steam", "org.kde.dolphin", "org.kde.konsole", "org.kde.krita"]);
        assert_eq!(pinned_after(&apps, &[], "steam", false), ["firefox", "org.kde.dolphin", "org.kde.konsole"]);
        assert_eq!(pinned_after(&apps, &["steam".into()], "steam", true), ["steam"]);

        let typed = |query: &str, cursor: usize| {
            let menu = Menu { query: query.into(), cursor, ..Menu::default() };
            enter(&menu, &crate::os_apps::search(&apps, query))
        };
        assert_eq!(typed("fire", 0), Some(MenuAction::Launch("firefox".into())));
        assert_eq!(typed("k", 1), Some(MenuAction::Launch("org.kde.krita".into())));
        // Past the last program, and when nothing matches: an order for him.
        assert_eq!(typed("k", 2), Some(MenuAction::Order("k".into())));
        assert_eq!(typed(" turn the lights off ", 0), Some(MenuAction::Order("turn the lights off".into())));
        assert_eq!(typed("  ", 0), None);
        assert!(Power::ShutDown.ends_session() && Power::LogOut.ends_session() && !Power::Lock.ends_session() && !Power::Sleep.ends_session());
    }

    #[test]
    fn desktop_icons_fill_columns_and_leave_the_core_free() {
        for (w, h) in [(1280.0, 756.0), (1920.0, 1036.0), (2560.0, 1396.0), (1080.0, 1876.0)] {
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h));
            let core = core_rect(screen);
            assert!(screen.contains_rect(core) && core.center() == screen.center());
            let cells = icon_grid(screen, core, 400);
            assert!(cells.len() > 40 && cells.len() < 400, "{w}x{h}: {}", cells.len());
            for (i, cell) in cells.iter().enumerate() {
                assert!(screen.shrink(DESK_MARGIN - 0.5).contains_rect(*cell) && !cell.intersects(core), "{w}x{h}: {cell:?}");
                assert!(cells[i + 1..].iter().all(|other| !other.shrink(0.5).intersects(*cell)), "{w}x{h}: {cell:?} lies on another");
            }
            // Down the first column first.
            assert_eq!((cells[0].min, cells[1].min), (pos2(DESK_MARGIN, DESK_MARGIN), pos2(DESK_MARGIN, DESK_MARGIN + CELL.y)));
            assert_eq!(icon_grid(screen, core, 3).len(), 3);
        }
        assert_eq!(desktop_dir("# comment\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\nXDG_MUSIC_DIR=\"$HOME/Musik\"\n", "/home/max"), PathBuf::from("/home/max/Schreibtisch"));
        assert_eq!(desktop_dir("XDG_DESKTOP_DIR=\"/data/desk\"\n", "/home/max"), PathBuf::from("/data/desk"));
        assert_eq!(desktop_dir("", "/home/max"), PathBuf::from("/home/max/Desktop"));
        assert_eq!((file_icon("Games", true), file_icon("holiday.JPG", false), file_icon("notes", false), file_icon("setup.tar.zst", false)), ("folder", "image-x-generic", "text-x-generic", "package-x-generic"));
    }

    /// A picture that stands in for a program's icon: a disc on a darker square.
    fn disc(rgb: [u8; 3]) -> Arc<egui::ColorImage> {
        let n = 44usize;
        let mut image = egui::ColorImage::new([n, n], Color32::TRANSPARENT);
        for y in 0..n {
            for x in 0..n {
                let d = ((x as f32 - 21.5).powi(2) + (y as f32 - 21.5).powi(2)).sqrt();
                if d < 20.0 {
                    let k = if d < 12.0 { 1.0 } else { 0.45 };
                    image.pixels[y * n + x] = Color32::from_rgb((rgb[0] as f32 * k) as u8, (rgb[1] as f32 * k) as u8, (rgb[2] as f32 * k) as u8);
                }
            }
        }
        Arc::new(image)
    }

    fn icons() -> Icons {
        [("firefox", [255, 120, 30]), ("org.kde.dolphin", [60, 150, 255]), ("steam", [40, 60, 110]), ("org.kde.konsole", [70, 70, 70]), ("tray:discord", [110, 120, 250]), ("tray:steam", [40, 60, 110]), ("folder", [80, 170, 255]), ("image-x-generic", [120, 200, 120])]
            .into_iter()
            .map(|(key, rgb)| (key.to_string(), disc(rgb)))
            .collect()
    }

    fn no_findings(frame: &crate::visual_ui_verifier::Frame, what: &str) {
        assert_eq!(clipped_text(frame), Vec::<String>::new(), "{what}: clipped text");
        assert_eq!(text_over_text(frame), Vec::<String>::new(), "{what}: text over text");
    }

    #[test]
    fn os_shell_picture_of_the_taskbar() {
        let _look = crate::holo_theme::look_for_pictures();
        let apps = crate::os_apps::tests::sample();
        let name = |id: &str| apps.iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_default();
        let tasks = [
            Task { name: name("firefox"), title: "J.A.I.OS: the plan - Mozilla Firefox".into(), ..task("a", "firefox", true, false) },
            Task { name: name("firefox"), ..task("b", "firefox", false, false) },
            Task { name: name("org.kde.dolphin"), title: "Downloads".into(), ..task("c", "org.kde.dolphin", false, false) },
            Task { name: name("org.kde.konsole"), title: "max@jaios: ~".into(), ..task("d", "org.kde.konsole", false, true) },
            Task { title: "Elden Ring".into(), ..task("e", "steam_app_1245620", false, false) },
        ];
        let groups = groups(&tasks, &mut Vec::new());
        let tray = [TrayItem { key: "tray:discord".into(), title: "Discord".into(), attention: true }, TrayItem { key: "tray:steam".into(), title: "Steam".into(), attention: false }, TrayItem { key: "tray:none".into(), title: "KDE Connect".into(), attention: false }];
        let icons = icons();
        for (label, width, state) in [("wide", 1920.0, "thinking"), ("narrow", 1024.0, "idle")] {
            let mut stage = Stage::new(vec2(width, BAR_H));
            let bar = Bar { groups: &groups, tray: &tray, icons: &icons, state, clock: ("14:05", "WED 25 NOV"), menu_open: label == "wide" };
            let frame = stage.frames(2, &mut |ctx| {
                paint_bar(ctx, &bar);
            });
            stage.raster(&frame).save(&format!("os_shell_taskbar_{label}"));
            no_findings(&frame, label);
            for word in ["14:05", "WED 25 NOV", if state == "idle" { "READY" } else { "THINKING" }, "Firefox", "Downloads", "Elden Ring"] {
                assert!(!frame.find_text(word).is_empty(), "{label}: {word} is not on the bar:\n{}", frame.all_text());
            }
            // A click on the [J] key asks for the menu; one on Dolphin's key for its window.
            let layout = bar_layout(width, groups.len(), tray.len());
            let mut asked = Vec::new();
            for at in [layout.j.center(), layout.tasks[1].center(), layout.tray[0].center(), layout.lamp.center()] {
                stage.move_pointer(at);
                stage.frame(&mut |ctx| drop(paint_bar(ctx, &bar)));
                stage.press(egui::PointerButton::Primary, true);
                stage.frame(&mut |ctx| drop(paint_bar(ctx, &bar)));
                stage.press(egui::PointerButton::Primary, false);
                stage.frame(&mut |ctx| asked.extend(paint_bar(ctx, &bar)));
            }
            assert_eq!(asked, [BarAction::Menu, BarAction::Activate("c".into()), BarAction::Tray(0, TrayClick::Activate), BarAction::Lamp], "{label}");
        }
    }

    #[test]
    fn os_shell_picture_of_the_menu() {
        let _look = crate::holo_theme::look_for_pictures();
        let apps = crate::os_apps::tests::sample();
        let icons = icons();
        for (label, query, tab) in [("favourites", "", 0), ("group", "", 3), ("search", "k", 0), ("order", "turn the lights off", 0)] {
            let mut stage = Stage::new(MENU_SIZE);
            let mut menu = Menu { query: query.into(), tab, ..Menu::default() };
            let frame = stage.frames(3, &mut |ctx| {
                paint_menu(ctx, &mut menu, &apps, &[], &icons);
            });
            stage.raster(&frame).save(&format!("os_shell_menu_{label}"));
            no_findings(&frame, label);
            let expect: &[&str] = match label {
                "favourites" => &["Firefox", "Steam (Runtime)", "Dolphin", "Konsole", "SHUT DOWN", "LOCK", "Games"],
                "group" => &["Krita", "Digital Painting"],
                "search" => &["Konsole", "Krita", "Tell J.A.I.: k"],
                _ => &["Tell J.A.I.: turn the lights off"],
            };
            for word in expect {
                assert!(!frame.find_text(word).is_empty(), "{label}: {word} is not on the menu:\n{}", frame.all_text());
            }
            assert!(label != "group" || frame.find_text("Firefox").is_empty(), "the graphics group shows a browser");
        }
        // Shut down is pressed twice: the first press arms the key.
        let mut stage = Stage::new(MENU_SIZE);
        let mut menu = Menu::default();
        let mut asked = Vec::new();
        let frame = stage.frames(3, &mut |ctx| drop(paint_menu(ctx, &mut menu, &apps, &[], &icons)));
        let key = frame.find_exact("SHUT DOWN")[0].center();
        for _ in 0..2 {
            stage.move_pointer(key);
            stage.frame(&mut |ctx| drop(paint_menu(ctx, &mut menu, &apps, &[], &icons)));
            stage.press(egui::PointerButton::Primary, true);
            stage.frame(&mut |ctx| drop(paint_menu(ctx, &mut menu, &apps, &[], &icons)));
            stage.press(egui::PointerButton::Primary, false);
            stage.frame(&mut |ctx| asked.extend(paint_menu(ctx, &mut menu, &apps, &[], &icons)));
            if asked.is_empty() {
                assert_eq!(menu.armed, Some(Power::ShutDown));
                let armed = stage.frame(&mut |ctx| drop(paint_menu(ctx, &mut menu, &apps, &[], &icons)));
                stage.raster(&armed).save("os_shell_menu_armed");
                no_findings(&armed, "armed");
                assert!(!armed.find_exact("CONFIRM SHUT DOWN").is_empty());
            }
        }
        assert_eq!(asked, [MenuAction::Power(Power::ShutDown)]);
    }

    #[test]
    fn os_shell_picture_of_the_desktop() {
        let _look = crate::holo_theme::look_for_pictures();
        let icons = icons();
        let items: Vec<DeskItem> = [("Games", "folder"), ("holiday.jpg", "image-x-generic"), ("A file with a very long name indeed.txt", "text-x-generic"), ("Firefox", "firefox"), ("Steam", "steam")]
            .into_iter()
            .map(|(name, icon)| DeskItem { name: name.into(), path: PathBuf::from("/home/max/Desktop").join(name), icon: icon.into() })
            .collect();
        let size = vec2(1280.0, 756.0);
        let mut stage = Stage::new(size);
        let mut core = crate::holo_core::Core::new(crate::holo_core::Pose::Front);
        for _ in 0..90 {
            core.step(1.0 / 60.0, &crate::holo_core::Feed { day_secs: 14.0 * 3600.0 + 300.0, ..Default::default() });
        }
        let mut selected = Some(1);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
        let draw = |ctx: &egui::Context, selected: &mut Option<usize>| {
            let opened = paint_desktop(ctx, &items, &icons, selected);
            // (On the PC the core has a surface of its own over the desktop's middle.)
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("core")));
            crate::holo_core::paint(&p, egui::Id::new("os-core"), core.style, screen.center(), core_rect(screen).width() * 0.36, &core.look());
            opened
        };
        let frame = stage.frames(2, &mut |ctx| _ = draw(ctx, &mut selected));
        let canvas = stage.raster(&frame);
        canvas.save("os_shell_desktop");
        no_findings(&frame, "desktop");
        assert!(!frame.find_text("holiday.jpg").is_empty() && !frame.find_text("Games").is_empty());
        // The long name is cut to its cell, and the core's middle is lit while the corner is black.
        let cells = icon_grid(screen, core_rect(screen), items.len());
        assert!(frame.find_text("A file with").iter().all(|r| cells[2].expand(1.0).contains_rect(*r)));
        assert!(canvas.brightness(core_rect(screen)) > canvas.brightness(Rect::from_min_size(pos2(900.0, 500.0), vec2(200.0, 200.0))) + 1.0);
        // A click chooses, a double click opens, a click on the canvas lets go.
        let mut opened = Vec::new();
        // The core's own surface draws him alone.
        let mut alone = Stage::new(core_rect(screen).size());
        let lone = alone.frames(2, &mut |ctx| paint_core(ctx, &core));
        let lone = alone.raster(&lone);
        lone.save("os_shell_core");
        assert!(lone.brightness(Rect::from_min_size(pos2(0.0, 0.0), core_rect(screen).size())) > 1.0);
        let click = |stage: &mut Stage, at: Pos2, selected: &mut Option<usize>, opened: &mut Vec<usize>| {
            stage.move_pointer(at);
            stage.frame(&mut |ctx| _ = draw(ctx, selected));
            stage.press(egui::PointerButton::Primary, true);
            stage.frame(&mut |ctx| _ = draw(ctx, selected));
            stage.press(egui::PointerButton::Primary, false);
            stage.frame(&mut |ctx| opened.extend(draw(ctx, selected)));
        };
        click(&mut stage, cells[3].center(), &mut selected, &mut opened);
        assert_eq!((selected, opened.len()), (Some(3), 0));
        click(&mut stage, cells[3].center(), &mut selected, &mut opened);
        assert_eq!(opened, [3]);
        click(&mut stage, pos2(1000.0, 600.0), &mut selected, &mut opened);
        assert_eq!(selected, None);
    }

    /// Probe: what one frame of each surface costs on the CPU (egui's frame, then the raster).
    /// Sir's PC, 2026-11-27, a frame and its raster: the complex core 9 + 40 ms, the desktop
    /// 10 + 178 ms, every pixel sampling the atlas's white; sampled once a shape and with each
    /// row's span, 5 + 23 and 4 + 38 ms.
    #[test]
    #[ignore]
    fn os_shell_frame_costs_probe() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1440.0));
        for style in [crate::holo_core::CoreStyle::Simple, crate::holo_core::CoreStyle::Complex] {
            let mut core = crate::holo_core::Core::new(crate::holo_core::Pose::Front);
            core.style = style;
            core.set_mood(crate::holo_core::Mood::Speaking);
            let mut stage = Stage::new(core_rect(screen).size());
            for k in 0..30 {
                core.step(1.0 / 15.0, &crate::holo_core::Feed { voice: 0.55, day_secs: 50_000.0 + k as f32, ..Default::default() });
                let t = std::time::Instant::now();
                let frame = stage.frames(1, &mut |ctx| paint_core(ctx, &core));
                let made = t.elapsed();
                let _ = stage.raster(&frame);
                if k % 10 == 9 {
                    println!("core {style:?} {}x{}: frame {:?}, raster {:?}", core_rect(screen).width(), core_rect(screen).height(), made, t.elapsed() - made);
                }
            }
        }
        let icons = icons();
        let mut stage = Stage::new(screen.size());
        let t = std::time::Instant::now();
        let frame = stage.frames(1, &mut |ctx| _ = paint_desktop(ctx, &[], &icons, &mut None));
        let made = t.elapsed();
        let _ = stage.raster(&frame);
        println!("desktop 2560x1440: frame {:?}, raster {:?}", made, t.elapsed() - made);
    }

    #[test]
    fn os_shell_picture_of_a_tray_menu() {
        let _look = crate::holo_theme::look_for_pictures();
        let rows = [
            ListRow { id: 1, label: "Open Discord".into(), enabled: true, ..Default::default() },
            ListRow { id: 2, label: "Mute".into(), enabled: true, ticked: true, ..Default::default() },
            ListRow::default(),
            ListRow { id: 3, label: "Status".into(), enabled: false, ..Default::default() },
            ListRow { id: 4, label: "Do not disturb".into(), enabled: true, depth: 1, ..Default::default() },
            ListRow { id: 5, label: "Quit Discord".into(), enabled: true, ..Default::default() },
        ];
        let mut stage = Stage::new(list_size(&rows));
        let frame = stage.frames(2, &mut |ctx| _ = paint_list(ctx, &rows));
        stage.raster(&frame).save("os_shell_tray_menu");
        no_findings(&frame, "tray menu");
        let mut picked = Vec::new();
        for label in ["Status", "Quit Discord"] {
            stage.move_pointer(frame.find_exact(label)[0].center());
            stage.frame(&mut |ctx| _ = paint_list(ctx, &rows));
            stage.press(egui::PointerButton::Primary, true);
            stage.frame(&mut |ctx| _ = paint_list(ctx, &rows));
            stage.press(egui::PointerButton::Primary, false);
            stage.frame(&mut |ctx| picked.extend(paint_list(ctx, &rows)));
        }
        // (A row that cannot be chosen takes no click.)
        assert_eq!(picked, [5]);
    }
}
