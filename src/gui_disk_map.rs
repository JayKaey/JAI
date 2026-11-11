//! The DISK MAP block (`crate::disk_map`): on the board, or in its own window once detached.
//!
//! While the scan runs, the drive's top-level folders are flat tiles that grow and re-squarify
//! beside a dark READING tile of what is still to come; a tile that grew flashes pale and every
//! new layout cross-fades over the last one. The finished map (up to three levels deep, folders
//! with a header strip) wipes in from the left. A click opens a folder, a right-click goes back
//! up. The side column holds the drive's ring, the file types and the largest items.
//!
//! Everything here is painted from `disk_map::snapshot()`: the panel never touches the disk.

use crate::disk_map::{self as dm, Category, Kind, Phase, Snapshot, Tree};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, FontFamily, FontId, Frame, Margin, Painter, Pos2, Rect, RichText, Rounding, Sense, Shape, Ui, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
};
use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "disk_map";
const TITLE: &str = "Disk map";

/// A tile that grew flashes pale this long (seconds).
const FLASH: f64 = 0.10;
/// A tile flashes again only after this long, and only when it grew by `REFLASH_GROWTH`.
const REFLASH: f64 = 0.4;
const REFLASH_GROWTH: f64 = 1.03;
/// A new live layout fades in over the last one this long.
const XFADE: f64 = 0.08;
/// The finished map is wiped in from the left this long.
const WIPE: f64 = 0.13;
/// While anything moves: a frame every 33 ms (30 fps), and none otherwise.
const TICK: Duration = Duration::from_millis(33);
const INK: Color32 = Color32::from_rgb(0xF4, 0xFA, 0xFF);
/// Text on a bright tile.
const DARK: Color32 = Color32::from_rgb(0x05, 0x0A, 0x10);
const ALERT: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x4E);
const ALERT_RGB: [u8; 3] = [0xFF, 0x3B, 0x4E];
const MAP_FILL: Color32 = Color32::from_rgb(0x03, 0x07, 0x0C);
const READING_FILL: Color32 = Color32::from_rgb(0x0B, 0x15, 0x24);
const TRACK: Color32 = Color32::from_rgb(0x12, 0x1E, 0x2C);
const HIDDEN_FILL: Color32 = Color32::from_rgb(0x2B, 0x33, 0x3E);
const HATCH: Color32 = Color32::from_rgb(0x46, 0x50, 0x5C);
const PALE: Color32 = Color32::from_rgb(0xE6, 0xF6, 0xFF);
const BUTTON_FILL: Color32 = Color32::from_rgb(0x03, 0x07, 0x0C);
/// The most tiles one map draws.
const MAX_TILES: usize = 1600;
/// Levels of the finished map: the folder's content, and that of its folders, twice.
const NEST_LEVELS: usize = 3;
/// At most this many tiles of their own per folder, by level (the rest is one tile).
const LEVEL_CAP: [usize; 3] = [60, 28, 14];
/// Leaf tiles are this bright, by level.
const LEAF_SHADE: [f32; 3] = [0.66, 0.60, 0.55];

/// The map as last drawn: closed while it was a window of its own, the window plays its exit
/// with this (closing lets the tree go at once).
static LAST: Mutex<Option<Snapshot>> = Mutex::new(None);

fn body(ui: &mut Ui, _rt: &Handle) {
    // (Closed while it was its own window: its exit shows the map it had and takes no clicks.)
    if !dm::is_open() {
        let last = super::lock(&LAST).clone();
        draw_panel(ui, &last.unwrap_or_default());
        return;
    }
    let snap = dm::snapshot();
    {
        let mut last = super::lock(&LAST);
        if last.as_ref().map_or(true, |l| l.version != snap.version) {
            *last = Some(snap.clone());
        }
    }
    describe(&snap);
    match draw_panel(ui, &snap) {
        Some(Action::Drill(node)) => dm::drill(node),
        Some(Action::Back) => {
            dm::back();
        }
        Some(Action::Jump(depth)) => dm::jump(depth),
        Some(Action::Rescan) => dm::rescan(),
        Some(Action::Explorer) => dm::open_in_explorer(),
        None => {}
    }
}

/// Registers the disk map while it is open (a block on the board, or its own window once
/// detached).
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, ViewportId::from_hash_of("jai_disk_map"), crate::disk_map::is_open()) {
        crate::hud_dock::forget(ID);
        super::lock(&LAST).take();
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
    let mut builder = ViewportBuilder::default().with_title("Disk map — J.A.I.").with_app_id("jai").with_inner_size([1120.0, 700.0]).with_min_inner_size([680.0, 440.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let rt = rt.clone();
    ctx.show_viewport_deferred(ViewportId::from_hash_of("jai_disk_map"), builder, move |ctx, class| {
        crate::crash_guard::guard("disk map", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!crate::disk_map::is_open() && !super::detached_leaving(ctx)) {
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

// ------------------------------------------------------------------------- the content ----

/// The map's state as text (the block's Copy Content and the search across the blocks), set
/// again whenever the map changes.
fn describe(snap: &Snapshot) {
    static DESCRIBED: AtomicU64 = AtomicU64::new(u64::MAX);
    if DESCRIBED.swap(snap.version, Ordering::AcqRel) != snap.version {
        crate::hud_dock::describe_block(ID, &summary(snap));
    }
}

fn summary(snap: &Snapshot) -> String {
    let size = dm::format_size;
    let mut lines = vec![format!("DISK MAP {}", snap.label)];
    match snap.phase {
        Phase::Scanning => {
            let st = &snap.live.stats;
            let of = snap.expected.map(|e| format!(" of {}", size(e))).unwrap_or_default();
            lines.push(format!("Scanning: {}{of} read, {}, {}", size(st.bytes), count(st.files, "file", "files"), count(st.dirs, "folder", "folders")));
        }
        Phase::Done => {
            let st = &snap.stats;
            lines.push(format!("{} in {} and {}, scanned in {:.1} s", size(st.bytes), count(st.files, "file", "files"), count(st.dirs, "folder", "folders"), snap.elapsed.as_secs_f64()));
        }
        Phase::Failed => lines.push(snap.error.clone().unwrap_or_else(|| "The scan failed.".to_string())),
        Phase::Idle => {}
    }
    if let Some(d) = snap.drive {
        lines.push(format!("{}: {} used of {}, {} free", d.letter, size(d.used()), size(d.total), size(d.free)));
    }
    if let Some(tree) = snap.tree.as_deref() {
        let at = snap.current();
        let top: Vec<String> = tree.nodes[at as usize].children.iter().take(8).map(|&c| format!("{} {}", tree.name(c), size(tree.nodes[c as usize].size))).collect();
        lines.push(format!("Largest in {}: {}", tree.name(at), top.join(", ")));
        let cats = tree.cats(at);
        let types: Vec<String> = Category::ALL.iter().filter(|k| cats[k.index()] > 0).map(|k| format!("{} {}", k.label(), size(cats[k.index()]))).collect();
        lines.push(format!("File types: {}", types.join(", ")));
    }
    let st = if snap.phase == Phase::Scanning { &snap.live.stats } else { &snap.stats };
    let mut extra = Vec::new();
    if let Some(bin) = snap.recycle.filter(|b| *b > 0) {
        extra.push(format!("Recycle Bin {}", size(bin)));
    }
    if st.cloud_bytes > 0 {
        extra.push(format!("{} in the cloud ({})", size(st.cloud_bytes), count(st.cloud_files, "file", "files")));
    }
    if st.unreadable > 0 {
        extra.push(format!("{} unreadable", count(st.unreadable, "folder", "folders")));
    }
    if !extra.is_empty() {
        lines.push(extra.join(" \u{B7} "));
    }
    lines.join("\n")
}

/// "1,336,317 files", "1 folder".
fn count(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", dm::group(n), if n == 1 { one } else { many })
}

// ------------------------------------------------------------------------------- style ----

/// What a click on the panel asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Open this folder.
    Drill(u32),
    /// One folder up.
    Back,
    /// Back to the breadcrumb's n-th folder (0: the root).
    Jump(usize),
    Rescan,
    Explorer,
}

/// The theme's colours, read once per frame.
struct Colours {
    accent: Color32,
    border: Color32,
    bracket: Color32,
}

impl Colours {
    fn now() -> Self {
        Self { accent: crate::hud_board::active_colour(), border: crate::hud_board::border_colour(), bracket: crate::hud_board::bracket_colour() }
    }
}

/// Type sizes for the panel's size: the reference at about 1090 x 650, smaller and larger
/// panels in proportion (never below what stays legible: text that does not fit is left out).
#[derive(Clone, Copy)]
struct Metrics {
    s: f32,
    head: f32,
    mono: f32,
    small: f32,
    label: f32,
    tile: f32,
    big: f32,
}

impl Metrics {
    fn new(size: Vec2) -> Self {
        let s = (size.x / 1090.0).min(size.y / 650.0).clamp(0.75, 1.3);
        Self { s, head: (25.0 * s).max(18.0), mono: (10.5 * s).max(9.5), small: (9.5 * s).max(9.0), label: (11.5 * s).max(10.0), tile: (11.0 * s).max(9.5), big: (21.0 * s).max(15.0) }
    }
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// The condensed display face (Bahnschrift).
fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

fn rgb([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// A file type's colour, darker by `k`.
fn shade([r, g, b]: [u8; 3], k: f32) -> Color32 {
    let f = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(r), f(g), f(b))
}

/// Text that reads on `fill`.
fn ink_on(fill: Color32) -> Color32 {
    let luma = 0.2126 * fill.r() as f32 + 0.7152 * fill.g() as f32 + 0.0722 * fill.b() as f32;
    if luma > 0.55 * 255.0 {
        DARK
    } else {
        INK
    }
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

// -------------------------------------------------------------------------------- text ----

fn row_h(ctx: &egui::Context, font: &FontId) -> f32 {
    ctx.fonts(|f| f.row_height(font))
}

fn measure(ctx: &egui::Context, text: &str, font: &FontId) -> Vec2 {
    ctx.fonts(|f| f.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE).size())
}

/// `text` cut to `n` characters by taking out its middle ("Program…s (x86)").
fn middle_ellipsis(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= n {
        return text.to_string();
    }
    if n == 0 {
        return String::new();
    }
    let (head, tail) = (n / 2, (n - 1) / 2);
    let mut out: String = chars[..head].iter().collect();
    out.push('\u{2026}');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// `text` as it fits in `width`: whole, or (`shorten`) without its middle; None when not even
/// four characters would fit.
fn fit(ctx: &egui::Context, text: &str, font: &FontId, width: f32, shorten: bool) -> Option<String> {
    if text.is_empty() || width <= 0.0 {
        return None;
    }
    // (Half a pixel of slack: a box made exactly as wide as its text may come out a hair narrower.)
    let full = measure(ctx, text, font).x;
    if full <= width + 0.5 {
        return Some(text.to_string());
    }
    if !shorten {
        return None;
    }
    let chars = text.chars().count();
    let mut n = ((chars as f32 * width / full.max(1.0)).floor() as usize).min(chars.saturating_sub(1));
    while n >= 4 {
        let short = middle_ellipsis(text, n);
        if measure(ctx, &short, font).x <= width + 0.5 {
            return Some(short);
        }
        n -= 1;
    }
    None
}

/// A text with its place, ready to paint.
#[derive(Clone, Debug)]
struct Label {
    at: Pos2,
    text: String,
    font: FontId,
    colour: Color32,
}

/// `text` placed in `rect` at `align` if it fits there (shortened in the middle when `shorten`),
/// with the rectangle it takes.
fn place(ctx: &egui::Context, rect: Rect, text: &str, font: &FontId, colour: Color32, align: Align2, shorten: bool) -> Option<(Label, Rect)> {
    if rect.height() + 0.5 < row_h(ctx, font) {
        return None;
    }
    let text = fit(ctx, text, font, rect.width(), shorten)?;
    let r = align.align_size_within_rect(measure(ctx, &text, font), rect);
    Some((Label { at: r.min, text, font: font.clone(), colour }, r))
}

fn paint_label(p: &Painter, l: &Label) {
    p.text(l.at, Align2::LEFT_TOP, &l.text, l.font.clone(), l.colour);
}

/// Paints `text` in `rect` if it fits there (see `place`); where it went.
fn put(p: &Painter, rect: Rect, text: &str, font: &FontId, colour: Color32, align: Align2, shorten: bool) -> Option<Rect> {
    let (label, r) = place(p.ctx(), rect, text, font, colour, align, shorten)?;
    paint_label(p, &label);
    Some(r)
}

/// `parts` joined by " · ", as many as fit from the first on.
fn put_parts(p: &Painter, rect: Rect, parts: &[String], font: &FontId, colour: Color32, align: Align2) -> Option<Rect> {
    for n in (1..=parts.len()).rev() {
        let text = parts[..n].join(" \u{B7} ");
        if measure(p.ctx(), &text, font).x <= rect.width() {
            return put(p, rect, &text, font, colour, align, false);
        }
    }
    parts.first().and_then(|first| put(p, rect, first, font, colour, align, true))
}

// ------------------------------------------------------------------------------- panel ----

/// The whole panel in the space `ui` has left; what a click asked for.
fn draw_panel(ui: &mut Ui, snap: &Snapshot) -> Option<Action> {
    let avail = ui.available_size();
    let width = avail.x.max(40.0);
    // (An auto-sized window offers the whole screen's height.)
    let height = if avail.y.is_finite() && avail.y < 4000.0 { avail.y } else { width * 0.62 }.max(40.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let ctx = ui.ctx().clone();
    let m = Metrics::new(rect.size());
    let c = Colours::now();
    let now = ui.input(|i| i.time);
    let p = ui.painter_at(rect);
    let scanning = snap.phase == Phase::Scanning;
    let mut action = None;
    let mut moving = scanning || snap.graft.is_some();

    // DISK MAP, and LIVE on the right.
    let head = display(m.head);
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), row_h(&ctx, &head)));
    let live = mono(m.mono);
    let live_rect = Rect::from_min_max(pos2(top.right() - measure(&ctx, "LIVE", &live).x - 2.0, top.top()), pos2(top.right() - 2.0, top.bottom()));
    let mut title_end = top.right();
    if put(&p, live_rect, "LIVE", &live, INK, Align2::RIGHT_CENTER, false).is_some() {
        let dot = 3.5 * m.s;
        let pulse = if scanning { 0.45 + 0.55 * (0.5 + 0.5 * (now * 5.0).sin() as f32) } else { 1.0 };
        let centre = pos2(live_rect.left() - dot - 5.0 * m.s, live_rect.center().y);
        p.circle_filled(centre, dot, ALERT.gamma_multiply(pulse));
        title_end = centre.x - dot - 12.0 * m.s;
    }
    put(&p, Rect::from_min_max(top.min, pos2(title_end, top.bottom())), "DISK MAP", &head, INK, Align2::LEFT_CENTER, false);

    // "SCANNING C:\" or "C:\ · 223 GB · 1,336,317 FILES · 315,942 FOLDERS".
    let sub_font = mono(m.mono);
    let sub = Rect::from_min_size(pos2(rect.left(), top.bottom() + 2.0 * m.s), vec2(rect.width(), row_h(&ctx, &sub_font)));
    put_parts(&p, sub, &subtitle(snap), &sub_font, if scanning { c.accent } else { super::STRIP_INK }, Align2::LEFT_CENTER);

    // The thin line with its accent.
    let line_y = (sub.bottom() + 5.0 * m.s).round() + 0.5;
    p.line_segment([pos2(rect.left(), line_y), pos2(rect.right(), line_y)], super::stroke(1.0, c.border));
    p.line_segment([pos2(rect.left(), line_y), pos2(rect.left() + rect.width() * 0.17, line_y)], super::stroke(2.0, c.accent));

    // C:\ › Users › max
    let crumb_font = mono(m.mono);
    let crumbs = Rect::from_min_size(pos2(rect.left(), line_y + 1.5 + 5.0 * m.s), vec2(rect.width(), row_h(&ctx, &crumb_font)));
    if let Some(k) = breadcrumb(ui, &p, crumbs, &crumb_names(snap), &crumb_font, &c) {
        action = Some(Action::Jump(k));
    }

    // The ticker (while reading) or the summary line.
    let foot_font = mono(m.small);
    let foot = Rect::from_min_max(pos2(rect.left(), rect.bottom() - row_h(&ctx, &foot_font)), rect.max);
    footer(&p, foot, snap, &foot_font, &c);

    let main = Rect::from_min_max(pos2(rect.left(), crumbs.bottom() + 8.0 * m.s), pos2(rect.right(), foot.top() - 8.0 * m.s));
    if main.height() >= 48.0 && main.width() >= 120.0 {
        let gap = 14.0 * m.s;
        let side_w = if main.width() >= 560.0 { (main.width() * 0.29).clamp(200.0, 330.0) } else { 0.0 };
        let map = if side_w > 0.0 { Rect::from_min_max(main.min, pos2(main.right() - side_w - gap, main.bottom())) } else { main };
        let (clicked, animating) = draw_map(ui, &p, map, snap, &m, now, &c);
        action = action.or(clicked);
        moving |= animating;
        if side_w > 0.0 {
            let side = Rect::from_min_max(pos2(main.right() - side_w, main.top()), main.max);
            action = action.or(draw_side(ui, &p, side, snap, &m, now, &c));
        }
    }
    if moving {
        ctx.request_repaint_after(TICK);
    }
    action
}

fn subtitle(snap: &Snapshot) -> Vec<String> {
    let label = snap.label.to_uppercase();
    match snap.phase {
        Phase::Scanning => vec![format!("SCANNING {label}")],
        Phase::Done => vec![label, dm::format_size(snap.stats.bytes), count(snap.stats.files, "FILE", "FILES"), count(snap.stats.dirs, "FOLDER", "FOLDERS")],
        Phase::Failed => vec![format!("{label} COULD NOT BE READ")],
        Phase::Idle => vec!["READY".to_string()],
    }
}

/// The breadcrumb's names: the root, then each folder opened.
fn crumb_names(snap: &Snapshot) -> Vec<String> {
    match snap.tree.as_deref() {
        Some(tree) => std::iter::once(tree.name(0).to_string()).chain(snap.view.iter().map(|&id| tree.name(id).to_string())).collect(),
        None => vec![snap.label.clone()],
    }
}

/// "C:\ › Users › max": every crumb but the last is a way back (its index when clicked). Too
/// long, it keeps the first crumb and the last ones with "…" between them.
fn breadcrumb(ui: &mut Ui, p: &Painter, area: Rect, crumbs: &[String], font: &FontId, c: &Colours) -> Option<usize> {
    let ctx = ui.ctx().clone();
    let n = crumbs.len();
    if n == 0 {
        return None;
    }
    let (sep, ell) = (" \u{203A} ", "\u{2026}");
    let sep_w = measure(&ctx, sep, font).x;
    let ell_w = measure(&ctx, ell, font).x;
    let widths: Vec<f32> = crumbs.iter().map(|t| measure(&ctx, t, font).x).collect();
    let width_of = |shown: &[Option<usize>]| shown.iter().map(|s| s.map_or(ell_w, |i| widths[i])).sum::<f32>() + sep_w * shown.len().saturating_sub(1) as f32;
    // None stands for the "…".
    let mut shown: Vec<Option<usize>> = (0..n).map(Some).collect();
    let mut hide = 1;
    while width_of(&shown) > area.width() && hide < n.saturating_sub(1) {
        shown = [Some(0), None].into_iter().chain((hide + 1..n).map(Some)).collect();
        hide += 1;
    }
    if width_of(&shown) > area.width() && n > 1 {
        shown = vec![None, Some(n - 1)];
    }
    let mut x = area.left();
    let mut clicked = None;
    for (k, item) in shown.iter().enumerate() {
        if k > 0 {
            let room = Rect::from_min_max(pos2(x, area.top()), area.max);
            match put(p, room, sep, font, c.border, Align2::LEFT_CENTER, false) {
                Some(r) => x = r.right(),
                None => break,
            }
        }
        let room = Rect::from_min_max(pos2(x, area.top()), area.max);
        let Some(i) = *item else {
            match put(p, room, ell, font, super::STRIP_MUTED, Align2::LEFT_CENTER, false) {
                Some(r) => x = r.right(),
                None => break,
            }
            continue;
        };
        let last = i + 1 == n;
        let Some((mut label, r)) = place(&ctx, room, &crumbs[i], font, INK, Align2::LEFT_CENTER, last) else { break };
        if !last {
            let response = ui.interact(r, ui.id().with(("jai_disk_map_crumb", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            label.colour = if response.hovered() { c.accent } else { super::STRIP_MUTED };
            if response.clicked() {
                clicked = Some(i);
            }
        }
        paint_label(p, &label);
        x = r.right();
    }
    clicked
}

/// While reading: READING and the folder being read. When done: how long it took, what could
/// not be read, what is in the cloud, and what the mouse does.
fn footer(p: &Painter, area: Rect, snap: &Snapshot, font: &FontId, c: &Colours) {
    let reading = match (snap.phase, snap.graft) {
        (Phase::Scanning, _) => Some("READING "),
        (Phase::Done, Some(_)) => Some("READING AGAIN "),
        _ => None,
    };
    if let Some(head) = reading {
        if let Some(r) = put(p, area, head, font, c.accent, Align2::LEFT_CENTER, false) {
            put(p, Rect::from_min_max(pos2(r.right(), area.top()), area.max), &snap.live.current, font, super::STRIP_MUTED, Align2::LEFT_CENTER, true);
        }
        return;
    }
    if snap.phase != Phase::Done {
        return;
    }
    let st = &snap.stats;
    let mut parts = vec![format!("SCANNED IN {:.1} S", snap.elapsed.as_secs_f64())];
    if st.unreadable > 0 {
        parts.push(format!("{} UNREADABLE", count(st.unreadable, "FOLDER", "FOLDERS")));
    }
    if st.cloud_bytes > 0 {
        parts.push(format!("{} IN THE CLOUD", dm::format_size(st.cloud_bytes)));
    }
    parts.push("CLICK: OPEN".to_string());
    parts.push("RIGHT-CLICK: BACK".to_string());
    put_parts(p, area, &parts, font, super::STRIP_MUTED, Align2::LEFT_CENTER);
}

// ----------------------------------------------------------------------------- treemap ----

/// The squarified treemap (Bruls, Huizing and van Wijk): one rectangle per size, in the order
/// given, together filling `rect` in proportion. The largest come first (top left); each row
/// runs along the shorter side and takes another item as long as that leaves its worst aspect
/// ratio no worse. A size of zero gets an empty rectangle.
fn squarify(sizes: &[f64], rect: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::from_min_size(rect.min, Vec2::ZERO); sizes.len()];
    let total: f64 = sizes.iter().filter(|s| **s > 0.0).sum();
    if total <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return out;
    }
    let mut order: Vec<usize> = (0..sizes.len()).filter(|&i| sizes[i] > 0.0).collect();
    order.sort_by(|&a, &b| sizes[b].total_cmp(&sizes[a]));
    let scale = rect.width() as f64 * rect.height() as f64 / total;
    let areas: Vec<f64> = order.iter().map(|&i| sizes[i] * scale).collect();
    let (mut x0, mut y0, x1, y1) = (rect.left() as f64, rect.top() as f64, rect.right() as f64, rect.bottom() as f64);
    let mut start = 0;
    while start < areas.len() {
        let (w, h) = (x1 - x0, y1 - y0);
        let side = w.min(h).max(1e-9);
        let (mut end, mut sum) = (start + 1, areas[start]);
        let mut best = worst(&areas[start..end], sum, side);
        while end < areas.len() {
            let next = worst(&areas[start..=end], sum + areas[end], side);
            if next > best {
                break;
            }
            best = next;
            sum += areas[end];
            end += 1;
        }
        let last = end == areas.len();
        if w >= h {
            // A column on the left.
            let col = if last { w } else { (sum / h.max(1e-9)).min(w) };
            let mut y = y0;
            for k in start..end {
                let bottom = if k + 1 == end { y1 } else { y + areas[k] / col.max(1e-9) };
                out[order[k]] = Rect::from_min_max(pos2(x0 as f32, y as f32), pos2((x0 + col) as f32, bottom as f32));
                y = bottom;
            }
            x0 += col;
        } else {
            // A strip along the top.
            let row = if last { h } else { (sum / w.max(1e-9)).min(h) };
            let mut x = x0;
            for k in start..end {
                let right = if k + 1 == end { x1 } else { x + areas[k] / row.max(1e-9) };
                out[order[k]] = Rect::from_min_max(pos2(x as f32, y0 as f32), pos2(right as f32, (y0 + row) as f32));
                x = right;
            }
            y0 += row;
        }
        start = end;
    }
    out
}

/// The worst aspect ratio of a row of `areas` (summing to `sum`) laid along a side of `side`.
fn worst(areas: &[f64], sum: f64, side: f64) -> f64 {
    let (lo, hi) = areas.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), &a| (lo.min(a), hi.max(a)));
    let (s2, w2) = (sum * sum, side * side);
    (w2 * hi / s2).max(s2 / (w2 * lo))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TileKind {
    Folder,
    File,
    /// A folder's smaller items, together.
    Rest,
    /// "System & hidden".
    Hidden,
    /// What the running scan has still to read.
    Reading,
    /// A top-level item of the running scan, read to the end.
    Live,
    /// The top-level folder being read.
    Busy,
}

/// One tile of a map, laid out and labelled.
#[derive(Clone, Debug)]
struct Tile {
    rect: Rect,
    /// The strip with the folder's name and size above its own tiles.
    strip: Option<Rect>,
    strip_fill: Color32,
    fill: Color32,
    kind: TileKind,
    labels: Vec<Label>,
    /// Its node in the finished tree.
    node: Option<u32>,
    /// The folder a click on it opens.
    opens: Option<u32>,
    bytes: u64,
    /// How many smaller items a bucket holds.
    items: u32,
    /// What a live tile is from update to update (it flashes when it grew).
    key: String,
    /// The first line of its tooltip (a live tile's name, a bucket's folder).
    title: String,
}

impl Tile {
    fn new(rect: Rect, kind: TileKind, fill: Color32, bytes: u64) -> Self {
        Self { rect, strip: None, strip_fill: fill, fill, kind, labels: Vec::new(), node: None, opens: None, bytes, items: 0, key: String::new(), title: String::new() }
    }
}

/// A tile's name in its middle, and its size beneath when there is room.
fn centre_labels(ctx: &egui::Context, r: Rect, name: &str, size: Option<&str>, m: &Metrics, ink: Color32) -> Vec<Label> {
    let inner = r.shrink(4.0 * m.s.min(1.0));
    let (name_font, size_font) = (display(m.tile), mono(m.small));
    let (name_h, size_h) = (row_h(ctx, &name_font), row_h(ctx, &size_font));
    if inner.width() < 18.0 || inner.height() < name_h {
        return Vec::new();
    }
    let Some(name) = fit(ctx, name, &name_font, inner.width(), true) else { return Vec::new() };
    let size = size.filter(|_| inner.height() >= name_h + size_h + 1.0).and_then(|s| fit(ctx, s, &size_font, inner.width(), false));
    let total = name_h + size.as_ref().map_or(0.0, |_| size_h + 1.0);
    let top = inner.center().y - total / 2.0;
    let name_w = measure(ctx, &name, &name_font).x;
    let mut out = vec![Label { at: pos2(inner.center().x - name_w / 2.0, top), text: name, font: name_font, colour: ink }];
    if let Some(size) = size {
        let w = measure(ctx, &size, &size_font).x;
        out.push(Label { at: pos2(inner.center().x - w / 2.0, top + name_h + 1.0), text: size, font: size_font, colour: ink.gamma_multiply(0.8) });
    }
    out
}

/// A bucket's name as it fits in tile `r`: "83,907 smaller items", else "83,907 items", else
/// "83,907"; None when not even the number fits (a count cut in the middle says nothing).
fn rest_name(ctx: &egui::Context, r: Rect, items: u32, m: &Metrics) -> Option<String> {
    let width = r.shrink(4.0 * m.s.min(1.0)).width();
    let font = display(m.tile);
    let n = u64::from(items);
    [dm::smaller_items(items), count(n, "item", "items"), dm::group(n)].into_iter().find(|t| measure(ctx, t, &font).x <= width + 0.5)
}

/// A folder strip's name on the left and its size on the right (the size goes first when both
/// do not fit).
fn strip_labels(ctx: &egui::Context, strip: Rect, name: &str, size: &str, m: &Metrics, ink: Color32) -> Vec<Label> {
    let inner = strip.shrink2(vec2(4.0 * m.s.min(1.0), 0.0));
    let (name_font, size_font) = (display(m.tile), mono(m.small));
    let (name_h, size_h) = (row_h(ctx, &name_font), row_h(ctx, &size_font));
    if inner.width() < 18.0 || inner.height() + 0.5 < name_h {
        return Vec::new();
    }
    let size_w = measure(ctx, size, &size_font).x;
    let beside = inner.width() - size_w - 8.0 * m.s;
    let mid = inner.center().y;
    if let Some(name) = (beside >= 30.0).then(|| fit(ctx, name, &name_font, beside, true)).flatten() {
        return vec![
            Label { at: pos2(inner.left(), mid - name_h / 2.0), text: name, font: name_font, colour: ink },
            Label { at: pos2(inner.right() - size_w, mid - size_h / 2.0), text: size.to_string(), font: size_font, colour: ink.gamma_multiply(0.8) },
        ];
    }
    fit(ctx, name, &name_font, inner.width(), true).map(|name| vec![Label { at: pos2(inner.left(), mid - name_h / 2.0), text: name, font: name_font, colour: ink }]).unwrap_or_default()
}

/// The running scan's map: the top-level items as flat tiles, and what is still to read.
fn live_tiles(ctx: &egui::Context, snap: &Snapshot, area: Rect, m: &Metrics) -> Vec<Tile> {
    let to_go = snap.expected.map_or(0, |e| e.saturating_sub(snap.live.stats.bytes));
    let mut sizes: Vec<f64> = snap.live.tiles.iter().map(|t| t.bytes as f64).collect();
    sizes.push(to_go as f64);
    let rects = squarify(&sizes, area);
    let mut out = Vec::new();
    for (t, r) in snap.live.tiles.iter().zip(&rects) {
        let r = r.shrink(0.5);
        if r.width() < 1.0 || r.height() < 1.0 {
            continue;
        }
        let bucket = t.items > 0;
        let colour = if bucket {
            Category::Other.rgb()
        } else if t.name.eq_ignore_ascii_case(dm::RECYCLE_NAME) {
            ALERT_RGB
        } else {
            t.cat.rgb()
        };
        let fill = shade(colour, if bucket { 0.42 } else { 0.72 });
        let kind = if bucket {
            TileKind::Rest
        } else if t.done {
            TileKind::Live
        } else {
            TileKind::Busy
        };
        let mut tile = Tile::new(r, kind, fill, t.bytes);
        let name = if bucket { rest_name(ctx, r, t.items, m) } else { Some(t.name.clone()) };
        tile.labels = name.map(|name| centre_labels(ctx, r, &name, Some(&dm::format_size(t.bytes)), m, ink_on(fill))).unwrap_or_default();
        tile.items = t.items;
        tile.key = t.name.clone();
        tile.title = t.name.clone();
        out.push(tile);
    }
    if let Some(r) = rects.last().filter(|_| to_go > 0) {
        let r = r.shrink(0.5);
        let mut tile = Tile::new(r, TileKind::Reading, READING_FILL, to_go);
        tile.labels = centre_labels(ctx, r, "READING..", Some(&format!("{} TO GO", dm::format_size(to_go))), m, INK);
        tile.title = "Still to read".to_string();
        out.push(tile);
    }
    out
}

/// One item of a folder's map: a node of its own, or its smaller items together.
struct Item {
    node: Option<u32>,
    bytes: u64,
    items: u32,
}

/// What `folder`'s tiles are in an area of `area` px²: its children that get at least
/// `min_area` (largest first, at most the level's cap), then everything else as one item.
fn items_of(tree: &Tree, folder: u32, area: f32, depth: usize, min_area: f32) -> Vec<Item> {
    let Some(n) = tree.node(folder) else { return Vec::new() };
    if n.size == 0 {
        return Vec::new();
    }
    let per_byte = area as f64 / n.size as f64;
    let cap = LEVEL_CAP[depth.min(LEVEL_CAP.len() - 1)];
    let mut out = Vec::new();
    let (mut rest, mut items) = (n.rest_bytes, n.rest_items);
    for &c in &n.children {
        let size = tree.nodes[c as usize].size;
        if out.len() < cap && size as f64 * per_byte >= min_area as f64 {
            out.push(Item { node: Some(c), bytes: size, items: 0 });
        } else {
            rest += size;
            items += 1;
        }
    }
    if rest > 0 {
        out.push(Item { node: None, bytes: rest, items });
    }
    out
}

/// Whether node `id` is the Recycle Bin or lies in it.
fn in_bin(tree: &Tree, mut id: u32) -> bool {
    while let Some(n) = tree.node(id) {
        if n.kind == Kind::Dir && tree.name(id).eq_ignore_ascii_case(dm::RECYCLE_NAME) {
            return true;
        }
        id = n.parent;
    }
    false
}

/// `folder`'s content as tiles in `area`; a folder with room shows its own content inside,
/// under a header strip (down to `NEST_LEVELS`).
#[allow(clippy::too_many_arguments)]
fn nest(ctx: &egui::Context, tree: &Tree, folder: u32, area: Rect, depth: usize, bin: bool, m: &Metrics, out: &mut Vec<Tile>) {
    let items = items_of(tree, folder, area.area(), depth, (14.0 * m.s).powi(2));
    let sizes: Vec<f64> = items.iter().map(|i| i.bytes as f64).collect();
    let rects = squarify(&sizes, area);
    let strip_h = row_h(ctx, &display(m.tile)) + 2.0;
    // A click inside an opened folder's tiles opens that folder.
    let opens_here = (depth > 0).then_some(folder);
    for (item, r) in items.iter().zip(rects) {
        if out.len() >= MAX_TILES {
            return;
        }
        let r = r.shrink(0.5);
        if r.width() < 1.0 || r.height() < 1.0 {
            continue;
        }
        let size = dm::format_size(item.bytes);
        let leaf_shade = LEAF_SHADE[depth.min(LEAF_SHADE.len() - 1)];
        let Some(id) = item.node else {
            let fill = shade(if bin { ALERT_RGB } else { tree.nodes[folder as usize].cat.rgb() }, 0.42);
            let mut tile = Tile::new(r, TileKind::Rest, fill, item.bytes);
            tile.labels = rest_name(ctx, r, item.items, m).map(|name| centre_labels(ctx, r, &name, Some(&size), m, ink_on(fill))).unwrap_or_default();
            tile.items = item.items;
            tile.opens = opens_here;
            tile.title = tree.path(folder).display().to_string();
            out.push(tile);
            continue;
        };
        let n = &tree.nodes[id as usize];
        let name = tree.name(id);
        match n.kind {
            Kind::Dir => {
                let red = bin || name.eq_ignore_ascii_case(dm::RECYCLE_NAME);
                let colour = if red { ALERT_RGB } else { n.cat.rgb() };
                let room = depth + 1 < NEST_LEVELS && r.width() >= 44.0 * m.s && r.height() >= strip_h + 24.0 * m.s && (!n.children.is_empty() || n.rest_bytes > 0);
                if room {
                    let strip = Rect::from_min_size(r.min, vec2(r.width(), strip_h));
                    let mut tile = Tile::new(r, TileKind::Folder, shade(colour, 0.30), item.bytes);
                    tile.strip = Some(strip);
                    tile.strip_fill = shade(colour, 0.80);
                    tile.labels = strip_labels(ctx, strip, name, &size, m, ink_on(tile.strip_fill));
                    tile.node = Some(id);
                    tile.opens = Some(id);
                    out.push(tile);
                    let inner = Rect::from_min_max(pos2(r.left() + 1.0, strip.bottom() + 1.0), pos2(r.right() - 1.0, r.bottom() - 1.0));
                    nest(ctx, tree, id, inner, depth + 1, red, m, out);
                } else {
                    let fill = shade(colour, leaf_shade);
                    let mut tile = Tile::new(r, TileKind::Folder, fill, item.bytes);
                    tile.labels = centre_labels(ctx, r, name, Some(&size), m, ink_on(fill));
                    tile.node = Some(id);
                    tile.opens = Some(id);
                    out.push(tile);
                }
            }
            Kind::File => {
                let fill = shade(if bin { ALERT_RGB } else { n.cat.rgb() }, leaf_shade);
                let mut tile = Tile::new(r, TileKind::File, fill, item.bytes);
                tile.labels = centre_labels(ctx, r, name, Some(&size), m, ink_on(fill));
                tile.node = Some(id);
                tile.opens = opens_here;
                out.push(tile);
            }
            Kind::Hidden => {
                let mut tile = Tile::new(r, TileKind::Hidden, HIDDEN_FILL, item.bytes);
                tile.labels = centre_labels(ctx, r, name, Some(&size), m, super::STRIP_INK);
                tile.node = Some(id);
                out.push(tile);
            }
        }
    }
}

/// The tiles of the map as the snapshot has it.
fn build_tiles(ctx: &egui::Context, snap: &Snapshot, area: Rect, m: &Metrics) -> Vec<Tile> {
    match (snap.phase, snap.tree.as_deref()) {
        (Phase::Scanning, _) => live_tiles(ctx, snap, area, m),
        (Phase::Done, Some(tree)) => {
            let at = snap.current();
            let mut out = Vec::new();
            nest(ctx, tree, at, area, 0, in_bin(tree, at), m, &mut out);
            out
        }
        _ => Vec::new(),
    }
}

/// What a layout was made from: a new one is made when any of it changes.
#[derive(Clone, PartialEq)]
struct LayoutKey {
    phase: Phase,
    /// Scanning: the published update. Finished: which tree (it changes when a folder was
    /// read again).
    data: (u64, usize, u64),
    view: Vec<u32>,
    area: [i32; 4],
}

impl LayoutKey {
    fn of(snap: &Snapshot, area: Rect) -> Self {
        let data = match (snap.phase, &snap.tree) {
            (Phase::Done, Some(tree)) => (Arc::as_ptr(tree) as usize as u64, tree.nodes.len(), tree.nodes.first().map_or(0, |n| n.size)),
            _ => (snap.version, 0, 0),
        };
        Self { phase: snap.phase, data, view: snap.view.clone(), area: [area.min.x, area.min.y, area.max.x, area.max.y].map(|v| v.round() as i32) }
    }
}

/// The map between frames: its tiles, the last layout while it fades out, and what animates.
#[derive(Clone, Default)]
struct ViewState {
    key: Option<LayoutKey>,
    tiles: Vec<Tile>,
    old: Vec<Tile>,
    xfade_at: Option<f64>,
    wipe_at: Option<f64>,
    /// Per live tile: its size when it last flashed, and when.
    flashes: HashMap<String, (u64, f64)>,
}

impl ViewState {
    /// A new layout: the finished map (or another folder of it) wipes in, a live update
    /// cross-fades, a resize just takes the new places.
    fn change(&mut self, key: LayoutKey, tiles: Vec<Tile>, now: f64) {
        let previous = self.key.replace(key.clone());
        let resized = previous.as_ref().is_some_and(|old| old.phase == key.phase && old.data == key.data && old.view == key.view);
        if resized {
            self.old.clear();
            self.xfade_at = None;
        } else if key.phase == Phase::Done {
            self.wipe_at = Some(now);
            self.old.clear();
            self.xfade_at = None;
        } else if previous.is_some() {
            self.old = std::mem::take(&mut self.tiles);
            self.xfade_at = Some(now);
        }
        if key.phase == Phase::Scanning {
            for t in tiles.iter().filter(|t| !t.key.is_empty()) {
                let flash = match self.flashes.get(&t.key) {
                    None => true,
                    Some(&(size, at)) => t.bytes as f64 >= size as f64 * REFLASH_GROWTH && now - at >= REFLASH,
                };
                if flash {
                    self.flashes.insert(t.key.clone(), (t.bytes, now));
                }
            }
        } else {
            self.flashes.clear();
        }
        self.tiles = tiles;
    }

    /// Paints the map into `area`; whether it still moves.
    fn paint(&mut self, p: &Painter, area: Rect, now: f64, c: &Colours) -> bool {
        let wipe = self.wipe_at.map(|t| ((now - t) / WIPE).clamp(0.0, 1.0) as f32);
        if wipe.is_some_and(|t| t >= 1.0) {
            self.wipe_at = None;
        }
        let xfade = self.xfade_at.map(|t| ((now - t) / XFADE).clamp(0.0, 1.0) as f32);
        if xfade.is_some_and(|t| t >= 1.0) {
            self.xfade_at = None;
            self.old.clear();
        }
        let reveal = match wipe {
            Some(t) if t < 1.0 => area.left() + area.width() * ease(t),
            _ => f32::INFINITY,
        };
        let tp = p.with_clip_rect(Rect::from_min_max(area.min, pos2(reveal.min(area.right()), area.bottom())));
        let pulse = 0.5 + 0.5 * (now * 4.0).sin() as f32;
        for t in &self.tiles {
            fill_tile(&tp, t, c, pulse);
        }
        if let Some(x) = xfade.filter(|x| *x < 1.0) {
            let mut fading = tp.clone();
            fading.multiply_opacity(1.0 - x);
            for t in &self.old {
                fill_tile(&fading, t, c, pulse);
            }
        }
        let mut flashing = false;
        for t in &self.tiles {
            if let Some(&(_, at)) = self.flashes.get(&t.key) {
                let age = now - at;
                if (0.0..FLASH).contains(&age) {
                    flashing = true;
                    tp.rect_filled(t.rect, Rounding::ZERO, PALE.gamma_multiply(0.55 * (1.0 - (age / FLASH) as f32)));
                }
            }
        }
        // Names only on tiles the wipe has passed.
        for t in self.tiles.iter().filter(|t| t.rect.right() <= reveal + 0.5) {
            for l in &t.labels {
                paint_label(&tp, l);
            }
        }
        self.wipe_at.is_some() || self.xfade_at.is_some() || flashing
    }
}

fn fill_tile(p: &Painter, t: &Tile, c: &Colours, pulse: f32) {
    p.rect_filled(t.rect, Rounding::ZERO, t.fill);
    if let Some(strip) = t.strip {
        p.rect_filled(strip, Rounding::ZERO, t.strip_fill);
    }
    match t.kind {
        TileKind::Hidden => hatch(p, t.rect, HATCH, 6.0),
        TileKind::Reading => {
            p.rect_stroke(t.rect.shrink(0.5), Rounding::ZERO, super::stroke(1.0, c.border));
        }
        TileKind::Busy => {
            p.rect_stroke(t.rect.shrink(0.5), Rounding::ZERO, super::stroke(1.0, c.accent.gamma_multiply(0.35 + 0.65 * pulse)));
        }
        TileKind::Folder | TileKind::File | TileKind::Rest | TileKind::Live => {}
    }
}

/// Diagonal lines across `r` (what no folder shows).
fn hatch(p: &Painter, r: Rect, colour: Color32, step: f32) {
    let hp = p.with_clip_rect(r);
    let mut x = r.left() - r.height();
    while x < r.right() {
        hp.line_segment([pos2(x, r.bottom()), pos2(x + r.height(), r.top())], super::stroke(1.0, colour));
        x += step;
    }
}

/// The tooltip's lines for tile `t`.
fn tooltip(tree: Option<&Tree>, t: &Tile) -> Vec<String> {
    let size = dm::format_size(t.bytes);
    if let (Some(tree), Some(id)) = (tree, t.node) {
        let n = &tree.nodes[id as usize];
        if n.kind == Kind::Hidden {
            return vec![dm::HIDDEN_NAME.to_string(), size, "Used on the drive, but in no folder the scan could read: system files, shadow copies, the file system's own tables.".to_string()];
        }
        let mut lines = vec![tree.path(id).display().to_string()];
        lines.push(match n.kind {
            Kind::Dir => format!("{size} \u{B7} {} \u{B7} {}", count(u64::from(n.files), "file", "files"), count(u64::from(n.dirs), "folder", "folders")),
            _ => format!("{size} \u{B7} {}", n.cat.label()),
        });
        if n.unreadable {
            lines.push("Part of it could not be read.".to_string());
        }
        if n.truncated {
            lines.push("Opening it reads it in full.".to_string());
        }
        return lines;
    }
    let mut lines = vec![t.title.clone()];
    lines.push(match t.kind {
        TileKind::Rest => format!("{} \u{B7} {size}", dm::smaller_items(t.items)),
        TileKind::Busy => format!("{size} so far (being read)"),
        _ => size,
    });
    lines
}

/// The treemap in `frame`: its click's action, and whether it still moves.
fn draw_map(ui: &mut Ui, p: &Painter, frame: Rect, snap: &Snapshot, m: &Metrics, now: f64, c: &Colours) -> (Option<Action>, bool) {
    let ctx = ui.ctx().clone();
    p.rect_filled(frame, Rounding::ZERO, MAP_FILL);
    p.rect_stroke(frame, Rounding::ZERO, super::stroke(1.0, c.border));
    crate::hud_board::brackets(p, frame, c.bracket);
    let area = frame.shrink(3.0);
    let tree = snap.tree.as_deref();
    let empty = match snap.phase {
        Phase::Scanning => snap.live.tiles.is_empty() && snap.expected.unwrap_or(0) <= snap.live.stats.bytes,
        Phase::Done => tree.is_none(),
        Phase::Idle | Phase::Failed => true,
    };
    if empty {
        let (text, colour) = match snap.phase {
            Phase::Failed => (snap.error.clone().unwrap_or_else(|| "The scan failed.".to_string()), ALERT),
            _ => ("READING..".to_string(), super::STRIP_MUTED),
        };
        put(p, area.shrink(8.0), &text, &mono(m.label), colour, Align2::CENTER_CENTER, true);
        return (None, false);
    }
    let id = ui.id().with("jai_disk_map_view");
    let mut view: ViewState = ui.data_mut(|d| d.remove_temp(id)).unwrap_or_default();
    let key = LayoutKey::of(snap, area);
    if view.key.as_ref() != Some(&key) {
        let tiles = build_tiles(&ctx, snap, area, m);
        view.change(key, tiles, now);
    }
    let animating = view.paint(p, area, now, c);
    let mut action = None;
    let response = ui.interact(frame, ui.id().with("jai_disk_map_area"), Sense::click());
    let hovered = response.hover_pos().and_then(|at| view.tiles.iter().rposition(|t| t.rect.contains(at)));
    if let Some(t) = hovered.map(|i| &view.tiles[i]) {
        p.rect_stroke(t.rect.shrink(0.75), Rounding::ZERO, super::stroke(1.5, INK));
        let opens = t.opens.filter(|_| snap.phase == Phase::Done && snap.graft.is_none());
        if response.clicked() {
            action = opens.map(Action::Drill);
        }
        let lines = tooltip(tree, t);
        let response = if opens.is_some() { response.clone().on_hover_cursor(CursorIcon::PointingHand) } else { response.clone() };
        response.on_hover_ui_at_pointer(|ui| {
            for (k, line) in lines.iter().enumerate() {
                ui.label(RichText::new(line).font(mono(if k == 0 { 11.0 } else { 10.0 })).color(if k == 0 { INK } else { super::STRIP_MUTED }));
            }
        });
    }
    if response.secondary_clicked() && !snap.view.is_empty() {
        action = Some(Action::Back);
    }
    if let (Some(at), Some(tree)) = (snap.graft, tree) {
        graft_strip(p, area, snap, tree, at, m, c);
    }
    ui.data_mut(|d| d.insert_temp(id, view));
    (action, animating)
}

/// A folder being read again: a strip along the map's foot with how far it got.
fn graft_strip(p: &Painter, area: Rect, snap: &Snapshot, tree: &Tree, at: u32, m: &Metrics, c: &Colours) {
    let font = mono(m.small);
    let h = row_h(p.ctx(), &font) + 10.0 * m.s;
    if area.height() < h * 2.0 {
        return;
    }
    let strip = Rect::from_min_max(pos2(area.left(), area.bottom() - h), area.max);
    p.rect_filled(strip, Rounding::ZERO, READING_FILL);
    p.rect_stroke(strip.shrink(0.5), Rounding::ZERO, super::stroke(1.0, c.border));
    let before = tree.node(at).map_or(1, |n| n.size.max(1));
    let share = (snap.live.stats.bytes as f32 / before as f32).clamp(0.0, 1.0);
    p.rect_filled(Rect::from_min_size(pos2(strip.left(), strip.bottom() - 2.0), vec2(strip.width() * share, 2.0)), Rounding::ZERO, c.accent);
    let text = format!("READING {} AGAIN \u{B7} {}", tree.name(at).to_uppercase(), dm::format_size(snap.live.stats.bytes));
    put(p, strip.shrink2(vec2(8.0 * m.s, 0.0)), &text, &font, INK, Align2::LEFT_CENTER, true);
}

// ------------------------------------------------------------------------- side column ----

/// The ring, FILE TYPES, LARGEST IN … and the buttons.
fn draw_side(ui: &mut Ui, p: &Painter, side: Rect, snap: &Snapshot, m: &Metrics, now: f64, c: &Colours) -> Option<Action> {
    let ctx = ui.ctx().clone();
    let gap = 10.0 * m.s;
    let button_font = mono(m.mono);
    let button_h = row_h(&ctx, &button_font) + 10.0 * m.s;
    let buttons = Rect::from_min_max(pos2(side.left(), side.bottom() - button_h), side.max);
    let mut action = draw_buttons(ui, p, buttons, snap, &button_font, m, c);
    let room = Rect::from_min_max(side.min, pos2(side.right(), buttons.top() - gap));
    let d = (side.width() * 0.38).clamp(56.0, 118.0).min(room.height() * 0.32);
    let mut y = room.top();
    if d >= 44.0 {
        ring_block(p, Rect::from_min_size(room.min, vec2(room.width(), d)), snap, m, now, c);
        y += d + gap;
    }
    let types = file_types(p, Rect::from_min_max(pos2(room.left(), y), room.max), snap, m, c);
    if types > 0.0 {
        y += types + gap;
    }
    action = action.or(largest(ui, p, Rect::from_min_max(pos2(room.left(), y), room.max), snap, m, c));
    action
}

/// An arc from `from` to `to` (turns clockwise from the top).
fn arc(p: &Painter, centre: Pos2, r: f32, from: f32, to: f32, width: f32, colour: Color32) {
    if to <= from {
        return;
    }
    let steps = ((to - from) * 120.0).ceil().max(2.0) as usize;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32) * TAU - FRAC_PI_2;
            centre + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(Shape::line(points, super::stroke(width, colour)));
}

/// The gauge: while reading, the progress ("73%" of the drive's used space); when done, how full
/// the drive is ("99%" used, red from 90 %), with the figures beside it.
fn ring_block(p: &Painter, block: Rect, snap: &Snapshot, m: &Metrics, now: f64, c: &Colours) {
    let ctx = p.ctx().clone();
    let d = block.height();
    let centre = pos2(block.left() + d / 2.0, block.center().y);
    let width = (d * 0.085).max(5.0);
    let r = d / 2.0 - width / 2.0 - 5.0;
    let size = dm::format_size;
    let secs = snap.elapsed.as_secs();
    // The share the arc shows, its colour, the two lines in the ring and the lines beside it.
    let (share, colour, big, small, lines): (Option<f32>, Color32, String, String, Vec<(String, Color32)>) = match snap.phase {
        Phase::Scanning => {
            let st = &snap.live.stats;
            let share = snap.expected.filter(|e| *e > 0).map(|e| (st.bytes as f64 / e as f64).min(0.99) as f32);
            let big = share.map_or_else(|| format!("{secs} S"), |s| format!("{:.0}%", s * 100.0));
            let lines = vec![(size(st.bytes), INK), (count(st.files, "file", "files"), super::STRIP_MUTED), (count(st.dirs, "folder", "folders"), super::STRIP_MUTED), (format!("{secs} s"), super::STRIP_MUTED)];
            (share, c.accent, big, "SCANNING".to_string(), lines)
        }
        Phase::Done => match snap.drive {
            Some(drive) if snap.is_drive => {
                let full = drive.used_share() >= dm::NEARLY_FULL;
                let colour = if full { ALERT } else { c.accent };
                let lines = vec![(size(drive.used()), INK), (format!("of {}", size(drive.total)), super::STRIP_MUTED), (format!("FREE {}", size(drive.free)), if full { ALERT } else { super::STRIP_MUTED })];
                (Some(drive.used_share() as f32), colour, format!("{:.0}%", drive.used_share() * 100.0), "USED".to_string(), lines)
            }
            Some(drive) => {
                let share = (snap.stats.bytes as f64 / drive.total.max(1) as f64) as f32;
                let lines = vec![(size(snap.stats.bytes), INK), (count(snap.stats.files, "file", "files"), super::STRIP_MUTED), (format!("{}: FREE {}", drive.letter, size(drive.free)), super::STRIP_MUTED)];
                (Some(share), c.accent, format!("{:.0}%", share * 100.0), format!("OF {}:", drive.letter), lines)
            }
            None => {
                let lines = vec![(size(snap.stats.bytes), INK), (count(snap.stats.files, "file", "files"), super::STRIP_MUTED), (count(snap.stats.dirs, "folder", "folders"), super::STRIP_MUTED)];
                (None, c.accent, String::new(), String::new(), lines)
            }
        },
        Phase::Idle | Phase::Failed => (None, c.accent, String::new(), String::new(), Vec::new()),
    };
    p.circle_stroke(centre, r, super::stroke(width, TRACK));
    match share {
        Some(share) => {
            arc(p, centre, r, 0.0, share.clamp(0.0, 1.0), width, colour);
            if snap.phase == Phase::Done {
                for i in 0..60 {
                    let t = i as f32 / 60.0;
                    let a = t * TAU - FRAC_PI_2;
                    let dir = vec2(a.cos(), a.sin());
                    let r0 = r + width / 2.0 + 1.5;
                    let r1 = r0 + if i % 5 == 0 { 3.5 } else { 2.0 };
                    let tick = if t < share { colour } else { c.border.gamma_multiply(0.6) };
                    p.line_segment([centre + dir * r0, centre + dir * r1], super::stroke(1.0, tick));
                }
            }
        }
        // Reading a folder whose size is not known yet: a turning arc.
        None if snap.phase == Phase::Scanning => {
            let from = (now * 0.8).fract() as f32;
            arc(p, centre, r, from, from + 0.22, width, colour);
        }
        None => {}
    }
    // The figure in the ring.
    let hole = (r - width / 2.0) * 2.0;
    let (big_font, small_font) = (display(m.big * 0.9), mono(m.small));
    let (big_h, small_h) = (row_h(&ctx, &big_font), row_h(&ctx, &small_font));
    let inner_w = hole * 0.88;
    let both = !small.is_empty() && big_h + small_h <= hole * 0.92;
    let top = centre.y - if both { (big_h + small_h) / 2.0 } else { big_h / 2.0 };
    if !big.is_empty() {
        put(p, Rect::from_min_size(pos2(centre.x - inner_w / 2.0, top), vec2(inner_w, big_h)), &big, &big_font, INK, Align2::CENTER_CENTER, false);
    }
    if both {
        put(p, Rect::from_min_size(pos2(centre.x - inner_w / 2.0, top + big_h), vec2(inner_w, small_h)), &small, &small_font, colour, Align2::CENTER_CENTER, false);
    }
    // The figures beside it, as many as fit.
    let col = Rect::from_min_max(pos2(block.left() + d + 10.0 * m.s, block.top()), block.max);
    let fonts: Vec<FontId> = (0..lines.len()).map(|i| if i == 0 { display(m.big) } else { mono(m.mono) }).collect();
    let heights: Vec<f32> = fonts.iter().map(|f| row_h(&ctx, f)).collect();
    let mut shown = lines.len();
    while shown > 0 && heights[..shown].iter().sum::<f32>() > col.height() {
        shown -= 1;
    }
    let mut y = col.center().y - heights[..shown].iter().sum::<f32>() / 2.0;
    for ((text, colour), (font, h)) in lines.iter().zip(fonts.iter().zip(&heights)).take(shown) {
        put(p, Rect::from_min_size(pos2(col.left(), y), vec2(col.width(), *h)), text, font, *colour, Align2::LEFT_CENTER, false);
        y += h;
    }
}

/// FILE TYPES: a bar of the bytes per type and its legend in two columns. The height it took.
fn file_types(p: &Painter, area: Rect, snap: &Snapshot, m: &Metrics, c: &Colours) -> f32 {
    let ctx = p.ctx().clone();
    let cats = match (snap.phase, snap.tree.as_deref()) {
        (Phase::Done, Some(tree)) => tree.cats(snap.current()),
        (Phase::Scanning, _) => snap.live.cats,
        _ => return 0.0,
    };
    let title_font = bold(m.label);
    let title_h = row_h(&ctx, &title_font);
    let cell_font = mono(m.small);
    let cell_h = row_h(&ctx, &cell_font) + 2.0 * m.s;
    let bar_h = (7.0 * m.s).round().max(5.0);
    if area.height() < title_h + 4.0 * m.s + bar_h {
        return 0.0;
    }
    let mut y = area.top();
    put(p, Rect::from_min_size(area.min, vec2(area.width(), title_h)), "FILE TYPES", &title_font, c.accent, Align2::LEFT_CENTER, false);
    y += title_h + 4.0 * m.s;
    let total: u64 = cats.iter().sum();
    let mut order: Vec<Category> = Category::ALL.into_iter().filter(|k| cats[k.index()] > 0).collect();
    order.sort_by(|a, b| cats[b.index()].cmp(&cats[a.index()]));
    let bar = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), bar_h));
    p.rect_filled(bar, Rounding::ZERO, TRACK);
    let mut x = bar.left();
    for k in &order {
        let w = bar.width() * cats[k.index()] as f32 / total.max(1) as f32;
        if w >= 1.0 {
            p.rect_filled(Rect::from_min_max(pos2(x, bar.top()), pos2((x + w - 1.0).max(x + 1.0), bar.bottom())), Rounding::ZERO, rgb(k.rgb()));
        }
        x += w;
    }
    y = bar.bottom() + 5.0 * m.s;
    let col_gap = 10.0 * m.s;
    let col_w = (area.width() - col_gap) / 2.0;
    for row in order.chunks(2) {
        if y + cell_h > area.bottom() + 0.5 {
            break;
        }
        for (col, k) in row.iter().enumerate() {
            let cell = Rect::from_min_size(pos2(area.left() + col as f32 * (col_w + col_gap), y), vec2(col_w, cell_h));
            legend_cell(p, cell, *k, cats[k.index()], &cell_font, m);
        }
        y += cell_h;
    }
    y - area.top()
}

/// One entry of the legend: the type's colour, its name and its size (the size goes first when
/// both do not fit).
fn legend_cell(p: &Painter, cell: Rect, k: Category, bytes: u64, font: &FontId, m: &Metrics) {
    let ctx = p.ctx().clone();
    let sw = (7.0 * m.s).round().max(5.0);
    p.rect_filled(Rect::from_center_size(pos2(cell.left() + sw / 2.0, cell.center().y), vec2(sw, sw)), Rounding::ZERO, rgb(k.rgb()));
    let text = Rect::from_min_max(pos2(cell.left() + sw + 5.0 * m.s, cell.top()), cell.max);
    let size = dm::format_size(bytes);
    if measure(&ctx, k.label(), font).x + 6.0 + measure(&ctx, &size, font).x <= text.width() {
        put(p, text, k.label(), font, super::STRIP_INK, Align2::LEFT_CENTER, false);
        put(p, text, &size, font, super::STRIP_MUTED, Align2::RIGHT_CENTER, false);
    } else {
        put(p, text, k.label(), font, super::STRIP_INK, Align2::LEFT_CENTER, true);
    }
}

/// LARGEST IN …: the items of the folder on screen (while reading: of the root so far) with a
/// thin bar each; the Recycle Bin's in red. A folder's row opens it.
fn largest(ui: &mut Ui, p: &Painter, area: Rect, snap: &Snapshot, m: &Metrics, c: &Colours) -> Option<Action> {
    struct Row {
        name: String,
        bytes: u64,
        colour: Color32,
        opens: Option<u32>,
        busy: bool,
    }
    let ctx = ui.ctx().clone();
    let title_font = bold(m.label);
    let title_h = row_h(&ctx, &title_font);
    if area.height() < title_h {
        return None;
    }
    let (here, rows): (String, Vec<Row>) = match (snap.phase, snap.tree.as_deref()) {
        (Phase::Done, Some(tree)) => {
            let at = snap.current();
            let bin = in_bin(tree, at);
            let rows = tree.nodes[at as usize]
                .children
                .iter()
                .map(|&id| {
                    let n = &tree.nodes[id as usize];
                    let red = bin || (n.kind == Kind::Dir && tree.name(id).eq_ignore_ascii_case(dm::RECYCLE_NAME));
                    let colour = if red {
                        ALERT
                    } else if n.kind == Kind::Hidden {
                        HATCH
                    } else {
                        rgb(n.cat.rgb())
                    };
                    Row { name: tree.name(id).to_string(), bytes: n.size, colour, opens: (n.kind == Kind::Dir).then_some(id), busy: false }
                })
                .collect();
            (tree.name(at).to_string(), rows)
        }
        (Phase::Scanning, _) => {
            let rows = snap
                .live
                .tiles
                .iter()
                .map(|t| {
                    let colour = if t.name.eq_ignore_ascii_case(dm::RECYCLE_NAME) {
                        ALERT
                    } else if t.items > 0 {
                        HATCH
                    } else {
                        rgb(t.cat.rgb())
                    };
                    Row { name: t.name.clone(), bytes: t.bytes, colour, opens: None, busy: !t.done }
                })
                .collect();
            (snap.label.clone(), rows)
        }
        _ => return None,
    };
    // "LARGEST IN " and as much of the folder's name as fits.
    let prefix = "LARGEST IN ";
    let title_row = Rect::from_min_size(area.min, vec2(area.width(), title_h));
    let title = fit(&ctx, &here, &title_font, area.width() - measure(&ctx, prefix, &title_font).x, true).map_or_else(|| "LARGEST".to_string(), |name| format!("{prefix}{name}"));
    put(p, title_row, &title, &title_font, c.accent, Align2::LEFT_CENTER, false);
    let font = mono(m.small);
    let line_h = row_h(&ctx, &font);
    let bar_h = (3.0 * m.s).max(2.0);
    let step = line_h + 2.0 + bar_h + 5.0 * m.s;
    let most = rows.iter().map(|r| r.bytes).max().unwrap_or(1).max(1);
    let mut y = title_row.bottom() + 5.0 * m.s;
    let mut action = None;
    for (i, row) in rows.iter().enumerate() {
        if y + line_h + 2.0 + bar_h > area.bottom() + 0.5 {
            break;
        }
        let line = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), line_h));
        let mut ink = if row.busy { c.accent } else { super::STRIP_INK };
        if let Some(id) = row.opens {
            let whole = Rect::from_min_size(line.min, vec2(area.width(), line_h + 2.0 + bar_h));
            let response = ui.interact(whole, ui.id().with(("jai_disk_map_largest", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            if response.hovered() {
                ink = INK;
            }
            if response.clicked() {
                action = Some(Action::Drill(id));
            }
        }
        let size = dm::format_size(row.bytes);
        let size_w = measure(&ctx, &size, &font).x;
        put(p, line, &size, &font, super::STRIP_MUTED, Align2::RIGHT_CENTER, false);
        put(p, Rect::from_min_max(line.min, pos2(line.right() - size_w - 8.0 * m.s, line.bottom())), &row.name, &font, ink, Align2::LEFT_CENTER, true);
        let bar = Rect::from_min_size(pos2(area.left(), line.bottom() + 2.0), vec2(area.width(), bar_h));
        p.rect_filled(bar, Rounding::ZERO, TRACK);
        let w = bar.width() * row.bytes as f32 / most as f32;
        if w >= 0.5 {
            p.rect_filled(Rect::from_min_size(bar.min, vec2(w, bar_h)), Rounding::ZERO, row.colour);
        }
        y += step;
    }
    action
}

/// `< BACK`, `EXPLORER` and `RESCAN`: outlined, with a short accent on the top edge.
fn draw_buttons(ui: &mut Ui, p: &Painter, area: Rect, snap: &Snapshot, font: &FontId, m: &Metrics, c: &Colours) -> Option<Action> {
    let gap = 6.0 * m.s;
    let w = (area.width() - 2.0 * gap) / 3.0;
    let ready = matches!(snap.phase, Phase::Scanning | Phase::Done | Phase::Failed);
    let buttons = [("< BACK", !snap.view.is_empty(), Action::Back), ("EXPLORER", ready && !snap.demo, Action::Explorer), ("RESCAN", ready, Action::Rescan)];
    let mut action = None;
    for (i, (label, enabled, act)) in buttons.into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(area.left() + i as f32 * (w + gap), area.top()), vec2(w, area.height()));
        let mut response = ui.interact(r, ui.id().with(("jai_disk_map_button", i)), if enabled { Sense::click() } else { Sense::hover() });
        if enabled {
            response = response.on_hover_cursor(CursorIcon::PointingHand);
        }
        let hot = enabled && response.hovered();
        p.rect_filled(r, Rounding::ZERO, BUTTON_FILL);
        p.rect_stroke(r.shrink(0.5), Rounding::ZERO, super::stroke(1.0, if hot { INK } else { c.border }));
        let accent = if enabled { c.accent } else { c.accent.gamma_multiply(0.35) };
        p.line_segment([pos2(r.left() + 0.5, r.top() + 0.5), pos2(r.left() + r.width() * 0.32, r.top() + 0.5)], super::stroke(2.0, accent));
        let ink = if !enabled {
            super::STRIP_MUTED.gamma_multiply(0.55)
        } else if hot {
            INK
        } else {
            super::STRIP_INK
        };
        put(p, r.shrink2(vec2(4.0, 0.0)), label, font, ink, Align2::CENTER_CENTER, false);
        if enabled && response.clicked() {
            action = Some(act);
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};
    use egui::PointerButton;
    use std::cell::RefCell;
    use std::path::Path;

    #[test]
    fn squarified_tiles_fill_the_rect_in_proportion() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 400.0));
        // The example of the paper, shuffled: the rectangles keep the order given.
        let sizes = [2.0, 6.0, 1.0, 4.0, 6.0, 3.0, 2.0];
        let rects = squarify(&sizes, rect);
        let total: f64 = sizes.iter().sum();
        for (size, r) in sizes.iter().zip(&rects) {
            assert!(rect.expand(0.01).contains_rect(*r), "{r:?} leaves {rect:?}");
            let want = size / total * 600.0 * 400.0;
            assert!((r.area() as f64 - want).abs() <= want * 0.005 + 1.0, "{r:?} for {size}: {} px², want {want}", r.area());
            let aspect = (r.width() / r.height()).max(r.height() / r.width());
            assert!(aspect < 3.0, "{r:?} is a sliver ({aspect:.2})");
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let both = a.intersect(*b);
                assert!(both.width() <= 0.01 || both.height() <= 0.01, "{a:?} overlaps {b:?}");
            }
        }
        assert_eq!(rects[1].min, rect.min, "the (first) largest comes first: {rects:?}");
        // Zero sizes take no room; one item takes all of it.
        let z = squarify(&[0.0, 5.0], rect);
        assert_eq!(z[0].area(), 0.0);
        assert!((z[1].area() - rect.area()).abs() < 1.0);
        assert!(squarify(&[], rect).is_empty());
        // Many equal items stay close to squares.
        let many = squarify(&[1.0; 48], Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0)));
        assert!(many.iter().all(|r| (r.width() / r.height()).max(r.height() / r.width()) < 2.0), "{many:?}");
    }

    #[test]
    fn long_names_lose_their_middle() {
        assert_eq!(middle_ellipsis("Program Files (x86)", 15), "Program\u{2026}s (x86)");
        assert_eq!(middle_ellipsis("System & hidden", 11), "Syste\u{2026}idden");
        assert_eq!(middle_ellipsis("Users", 15), "Users");
        assert_eq!(middle_ellipsis("Downloads", 4), "Do\u{2026}s");
        assert_eq!(middle_ellipsis("Downloads", 1), "\u{2026}");
        assert_eq!(middle_ellipsis("Downloads", 0), "");
        assert_eq!(middle_ellipsis("Größenübersicht", 7), "Grö\u{2026}cht");
    }

    #[test]
    fn tiny_children_become_one_tile_of_smaller_items() {
        let snap = dm::demo_snapshot(false);
        let tree = snap.tree.as_deref().expect("the demo tree");
        let windows = tree.find(Path::new("C:\\Windows")).expect("Windows");
        // In 60 x 60 px, Installer (1.4 GB of 28.8) would get less than 14 x 14 px.
        let items = items_of(tree, windows, 3600.0, 0, 196.0);
        let names: Vec<&str> = items.iter().filter_map(|i| i.node).map(|id| tree.name(id)).collect();
        assert_eq!(names, ["WinSxS", "System32", "Temp", "assembly", "servicing", "SysWOW64"]);
        let rest = items.last().expect("the smaller items");
        assert!(rest.node.is_none());
        assert_eq!(rest.items, 30_419, "Windows' own 30,418 smaller items and Installer");
        assert_eq!(items.iter().map(|i| i.bytes).sum::<u64>(), tree.nodes[windows as usize].size);
        // The level's cap: at most 14 tiles of their own at the third level.
        let root = items_of(tree, 0, 1.0e9, 2, 1.0);
        assert!(root.iter().filter(|i| i.node.is_some()).count() <= 14);
        // WinSxS is nothing but its 83,907 smaller items.
        let winsxs = tree.find(Path::new("C:\\Windows\\WinSxS")).expect("WinSxS");
        let only = items_of(tree, winsxs, 1.0e5, 1, 196.0);
        assert_eq!(only.len(), 1);
        assert_eq!(dm::smaller_items(only[0].items), "83,907 smaller items");
        assert!(in_bin(tree, tree.find(Path::new("C:\\$Recycle.Bin\\S-1-5-21-1001")).expect("the bin's folder")));
        assert!(!in_bin(tree, windows));
    }

    fn draw(ctx: &egui::Context, snap: &Snapshot, actions: &RefCell<Vec<Action>>) {
        egui::CentralPanel::default().frame(Frame::none().fill(super::super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
            if let Some(a) = draw_panel(ui, snap) {
                actions.borrow_mut().push(a);
            }
        });
    }

    #[test]
    fn the_panel_is_drawn_whole_while_scanning_and_when_done() {
        for (size, name) in [(vec2(1120.0, 700.0), "1120x700"), (vec2(680.0, 440.0), "680x440")] {
            for scanning in [true, false] {
                let snap = dm::demo_snapshot(scanning);
                let actions = RefCell::new(Vec::new());
                let mut stage = Stage::new(size);
                let frame = stage.frames(30, &mut |ctx| draw(ctx, &snap, &actions));
                let state = if scanning { "scanning" } else { "done" };
                stage.raster(&frame).save(&format!("disk_map_{state}_{name}"));
                assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{state} at {name}");
                assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{state} at {name}");
                assert_eq!(visual::pictographs(&frame), Vec::<String>::new(), "{state} at {name}");
                let mut wanted = vec!["DISK MAP", "FILE TYPES", "LARGEST", "LIVE", "< BACK", "RESCAN", "Users"];
                if scanning {
                    wanted.extend(["SCANNING C:\\", "73%", "169 GB", "1,005,950 files", "212,227 folders", "62.0 GB TO GO", "READING"]);
                } else {
                    wanted.extend(["1,336,317 FILES", "315,942 FOLDERS", "99%", "USED", "231 GB", "of 234 GB", "FREE 3.2 GB", "SCANNED IN 13.3 S", "Windows", "83,907"]);
                }
                for needle in wanted {
                    assert!(!frame.find_text(needle).is_empty(), "\"{needle}\" is not on the panel ({state} at {name}):\n{}", frame.all_text());
                }
                // A count of smaller items loses words, never its middle ("83,907 items", not "83,90…items").
                let all = frame.all_text();
                let cut: Vec<&str> = all.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit()) && l.contains('\u{2026}')).collect();
                assert!(cut.is_empty(), "counts cut in the middle ({state} at {name}): {cut:?}");
                assert!(actions.borrow().is_empty(), "nothing was clicked");
            }
        }
    }

    #[test]
    fn a_click_opens_a_folder_and_a_right_click_goes_back() {
        let mut snap = dm::demo_snapshot(false);
        let tree = snap.tree.clone().expect("the demo tree");
        let users = tree.find(Path::new("C:\\Users")).expect("Users");
        let actions = RefCell::new(Vec::new());
        let mut stage = Stage::new(vec2(1120.0, 700.0));
        let frame = stage.frames(20, &mut |ctx| draw(ctx, &snap, &actions));
        // The map's Users strip lies left of the side column's Users row.
        let label = frame.find_exact("Users").into_iter().min_by(|a, b| a.left().total_cmp(&b.left())).expect("Users on the map");
        stage.click(label.center(), PointerButton::Primary, &mut |ctx| draw(ctx, &snap, &actions));
        assert_eq!(actions.borrow().as_slice(), [Action::Drill(users)]);
        // At the root a right-click does nothing.
        actions.borrow_mut().clear();
        stage.click(label.center(), PointerButton::Secondary, &mut |ctx| draw(ctx, &snap, &actions));
        assert!(actions.borrow().is_empty());
        // Inside Users it goes back; so does < BACK.
        snap.view = vec![users];
        snap.version += 1;
        let frame = stage.frames(20, &mut |ctx| draw(ctx, &snap, &actions));
        assert!(actions.borrow().is_empty());
        let map = frame.find_exact("max").into_iter().min_by(|a, b| a.left().total_cmp(&b.left())).expect("max inside Users");
        stage.click(map.center(), PointerButton::Secondary, &mut |ctx| draw(ctx, &snap, &actions));
        assert_eq!(actions.borrow().as_slice(), [Action::Back]);
        actions.borrow_mut().clear();
        let back = frame.find_exact("< BACK").first().copied().expect("the BACK button");
        stage.click(back.center(), PointerButton::Primary, &mut |ctx| draw(ctx, &snap, &actions));
        assert_eq!(actions.borrow().as_slice(), [Action::Back]);
        // The breadcrumb's root goes back to the top.
        actions.borrow_mut().clear();
        let root = frame.find_exact("C:\\").into_iter().min_by(|a, b| a.top().total_cmp(&b.top()).then(a.left().total_cmp(&b.left()))).expect("the breadcrumb's root");
        stage.click(root.center(), PointerButton::Primary, &mut |ctx| draw(ctx, &snap, &actions));
        assert_eq!(actions.borrow().as_slice(), [Action::Jump(0)]);
    }

    #[test]
    fn the_summary_names_the_drive_and_its_largest_folders() {
        let text = summary(&dm::demo_snapshot(false));
        assert!(text.starts_with("DISK MAP C:\\\n223 GB in 1,336,317 files and 315,942 folders, scanned in 13.3 s"), "{text}");
        assert!(text.contains("C: 231 GB used of 234 GB, 3.2 GB free"), "{text}");
        assert!(text.contains("Largest in C:\\: Users 127 GB, Windows 28.8 GB"), "{text}");
        assert!(text.contains("Recycle Bin 9.0 GB") && text.contains("1.6 GB in the cloud (2,214 files)") && text.contains("326 folders unreadable"), "{text}");
        let scanning = summary(&dm::demo_snapshot(true));
        assert!(scanning.contains("Scanning: 169 GB of 231 GB read, 1,005,950 files, 212,227 folders"), "{scanning}");
    }
}
