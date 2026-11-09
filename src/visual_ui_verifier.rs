//! The visual verifier: the HUD's real painters run in a private `egui::Context` and what they
//! paint is inspected as geometry — text galleys against their clip rectangles, block rectangles
//! against each other and the orb, stroke and mesh colours against the active theme, positions
//! from frame to frame — and, for borders, as rasterised pixels.
//!
//! Nothing here opens a window. A `Stage` steps a scene (the board with a report on it, in-app
//! block windows, the emblem, the core sphere) frame by frame with a simulated pointer; the
//! offscreen HUD (`gui::offscreen`) hands the same `Frame` type back for the whole interface.
//! The checks are the ones of the visual matrix:
//!
//! 1. no text cut or hidden by a clip rectangle unless that region scrolls (a scroll bar or the
//!    board's scroll track is drawn at its edge), and no pictographs in rendered text;
//! 2. no overlap between blocks, or between a block and the orb, with 1, 2, 4, 8 and 12 blocks;
//! 3. every block type and the orb move by exactly the pointer's drag;
//! 4. every stroke and mesh colour belongs to the active theme (the eight presets and three
//!    custom themes), a status red aside;
//! 5. no pictographs in the `.rs` sources (`live_self_tester::source_hygiene`);
//! 6. the page keys are at least 84 x 24 px and turn the page when clicked;
//! 7. the emblem's poses change without a single-frame jump, ring 5's clusters sit 120 degrees
//!    apart with ticks proportional to the voice, ring 6 keeps turning, the idle switch waits
//!    and holds 120-240 s, and the core sphere holds still under the pointer and turns with a
//!    drag.
//!
//! Every check returns its findings as sentences; an empty list is a pass.

use crate::hud_board::{Bar, Block, BlockKind, BoardView, Note, Report};
use crate::rose_emblem_renderer::{self as emblem, RoseEmblem, RoseEmblemState};
use crate::theme_engine::{self, ThemePalette};
use eframe::egui::{self, epaint, pos2, vec2, Color32, PointerButton, Pos2, Rect, Vec2};
use std::collections::HashSet;
use std::time::Duration;

/// The four screens of the matrix: the main monitor, the right one, the left one turned
/// upright, and one board spanning all three.
pub const SCREENS: [(&str, [f32; 2]); 4] = [("main 2560x1440", [2560.0, 1440.0]), ("right 2560x1440", [2560.0, 1440.0]), ("portrait 1080x1920", [1080.0, 1920.0]), ("span 6240x1920", [6240.0, 1920.0])];
/// How many blocks the overlap check fills the screen with.
pub const BLOCK_COUNTS: [usize; 5] = [1, 2, 4, 8, 12];

/// The reference desk: the main monitor, a second one to its right and an upright one to its
/// left that starts 290 px higher (as Windows places Sir's three). The box around them has
/// corners none of them shows, which is what a board across all of them has to live with.
pub fn desk() -> Vec<crate::monitors::Monitor> {
    let monitor = |index: usize, rect: [i32; 4], primary: bool| crate::monitors::Monitor { index, name: format!("DISPLAY{index}"), rect, work: rect, primary };
    vec![monitor(1, [0, 0, 2560, 1440], true), monitor(2, [2560, 0, 5120, 1440], false), monitor(3, [-1080, -290, 0, 1630], false)]
}

/// The HUD's windows on the desk: on each monitor, and one board across all three. (name, the
/// window's size, where its top left corner is on the desk.)
pub fn desk_screens() -> Vec<(&'static str, Vec2, Pos2)> {
    let desk = desk();
    let on = |m: &crate::monitors::Monitor| (vec2(m.width() as f32, m.height() as f32), pos2(m.rect[0] as f32, m.rect[1] as f32));
    let [x, y, w, h] = crate::monitors::virtual_bounds(&desk).unwrap_or([0, 0, 2560, 1440]);
    let (main, right, upright) = (on(&desk[0]), on(&desk[1]), on(&desk[2]));
    vec![("main 2560x1440", main.0, main.1), ("right 2560x1440", right.0, right.1), ("portrait 1080x1920", upright.0, upright.1), ("span 6200x1920", vec2(w as f32, h as f32), pos2(x as f32, y as f32))]
}
/// The least a page key's hitbox may be.
pub const MIN_PAGE_KEY: [f32; 2] = [84.0, 24.0];

// ------------------------------------------------------------------------------ frames ----

/// A text galley as painted: its clip rectangle and its rows (glyph bounds on screen, text).
#[derive(Debug, Clone)]
pub struct TextItem {
    pub clip: Rect,
    pub rows: Vec<(Rect, String)>,
    /// The layout cut the text short with an ellipsis.
    pub elided: bool,
    /// Its place in the frame's painting order (what is painted later lies on top).
    pub order: usize,
}

/// An outline: a rectangle's stroke, a line, a path, a circle.
#[derive(Debug, Clone, Copy)]
pub struct StrokeItem {
    pub clip: Rect,
    pub rect: Rect,
    pub colour: Color32,
    pub width: f32,
}

/// A filled rectangle (scroll bars and tracks among them, also while they are transparent).
#[derive(Debug, Clone, Copy)]
pub struct FillItem {
    pub rect: Rect,
    pub clip: Rect,
    /// How much of what lies under it the fill hides (its alpha, 0-255).
    pub cover: u8,
    /// Its place in the frame's painting order.
    pub order: usize,
}

/// What one frame painted.
pub struct Frame {
    pub screen: Rect,
    pub texts: Vec<TextItem>,
    pub strokes: Vec<StrokeItem>,
    pub fills: Vec<FillItem>,
    /// The vertex colours of raw meshes (rings, ribbons, gauges) with each mesh's bounds.
    pub mesh_colours: Vec<(Rect, Color32)>,
    /// Vertices of raw meshes in the frame.
    pub mesh_vertices: usize,
    shapes: Vec<epaint::ClippedShape>,
    pixels_per_point: f32,
}

impl Default for Frame {
    fn default() -> Self {
        Self { screen: Rect::NOTHING, texts: Vec::new(), strokes: Vec::new(), fills: Vec::new(), mesh_colours: Vec::new(), mesh_vertices: 0, shapes: Vec::new(), pixels_per_point: 1.0 }
    }
}

fn solid(colour: &epaint::ColorMode) -> Option<Color32> {
    match colour {
        epaint::ColorMode::Solid(c) => Some(*c),
        epaint::ColorMode::UV(_) => None,
    }
}

fn bounds(points: &[Pos2]) -> Rect {
    points.iter().fold(Rect::NOTHING, |r, p| r.union(Rect::from_min_max(*p, *p)))
}

fn collect(frame: &mut Frame, clip: Rect, shape: &epaint::Shape) {
    use epaint::Shape;
    let mut stroke = |rect: Rect, colour: Color32, width: f32| {
        if width > 0.0 && colour != Color32::TRANSPARENT {
            frame.strokes.push(StrokeItem { clip, rect, colour, width });
        }
    };
    match shape {
        Shape::Vec(list) => {
            for inner in list {
                collect(frame, clip, inner);
            }
        }
        Shape::Text(text) => {
            if text.angle != 0.0 || text.opacity_factor <= 0.01 {
                return;
            }
            let rows: Vec<(Rect, String)> = text
                .galley
                .rows
                .iter()
                .filter_map(|row| {
                    let said: String = row.glyphs.iter().map(|g| g.chr).collect();
                    if said.trim().is_empty() {
                        return None;
                    }
                    let glyphs = if row.visuals.mesh_bounds.is_positive() { row.visuals.mesh_bounds } else { row.rect };
                    Some((glyphs.translate(text.pos.to_vec2()), said))
                })
                .collect();
            if !rows.is_empty() {
                let order = frame.texts.len() + frame.fills.len();
                frame.texts.push(TextItem { clip, rows, elided: text.galley.elided, order });
            }
        }
        Shape::Rect(r) => {
            let order = frame.texts.len() + frame.fills.len();
            frame.fills.push(FillItem { rect: r.rect, clip, cover: r.fill.a(), order });
            stroke(r.rect, r.stroke.color, r.stroke.width);
        }
        Shape::LineSegment { points, stroke: line } => {
            if let Some(colour) = solid(&line.color) {
                stroke(bounds(points), colour, line.width);
            }
        }
        Shape::Path(path) => {
            if let Some(colour) = solid(&path.stroke.color) {
                stroke(bounds(&path.points), colour, path.stroke.width);
            }
        }
        Shape::Circle(c) => stroke(Rect::from_center_size(c.center, Vec2::splat(c.radius * 2.0)), c.stroke.color, c.stroke.width),
        Shape::Ellipse(e) => stroke(Rect::from_center_size(e.center, e.radius * 2.0), e.stroke.color, e.stroke.width),
        Shape::QuadraticBezier(b) => {
            if let Some(colour) = solid(&b.stroke.color) {
                stroke(bounds(&b.points), colour, b.stroke.width);
            }
        }
        Shape::CubicBezier(b) => {
            if let Some(colour) = solid(&b.stroke.color) {
                stroke(bounds(&b.points), colour, b.stroke.width);
            }
        }
        Shape::Mesh(mesh) => {
            // A textured mesh is a picture; its vertex colours are tints, not a theme colour.
            if mesh.texture_id != egui::TextureId::default() || mesh.vertices.is_empty() {
                return;
            }
            let area = mesh.calc_bounds();
            let mut seen: HashSet<[u8; 4]> = HashSet::new();
            for v in &mesh.vertices {
                if seen.insert(v.color.to_array()) {
                    frame.mesh_colours.push((area, v.color));
                }
            }
            frame.mesh_vertices += mesh.vertices.len();
        }
        Shape::Noop | Shape::Callback(_) => {}
    }
}

/// What a frame's output painted on `screen`.
pub fn capture(output: egui::FullOutput, screen: Rect) -> Frame {
    let mut frame = Frame { screen, pixels_per_point: output.pixels_per_point, ..Frame::default() };
    for clipped in &output.shapes {
        collect(&mut frame, clipped.clip_rect, &clipped.shape);
    }
    frame.shapes = output.shapes;
    frame
}

impl Frame {
    /// Every text row on screen whose words contain `needle` (case-insensitive): its bounds.
    pub fn find_text(&self, needle: &str) -> Vec<Rect> {
        let needle = needle.to_lowercase();
        self.texts.iter().flat_map(|t| t.rows.iter().filter(|(rect, said)| said.to_lowercase().contains(&needle) && t.clip.intersects(*rect)).map(|(rect, _)| *rect)).collect()
    }

    /// Every text row on screen that reads exactly `label` (a menu entry, a chip): its bounds.
    pub fn find_exact(&self, label: &str) -> Vec<Rect> {
        self.texts.iter().flat_map(|t| t.rows.iter().filter(|(rect, said)| said.trim() == label && t.clip.intersects(*rect)).map(|(rect, _)| *rect)).collect()
    }

    /// All text in the frame, row by row.
    pub fn all_text(&self) -> String {
        self.texts.iter().flat_map(|t| t.rows.iter().map(|(_, said)| said.as_str())).collect::<Vec<_>>().join("\n")
    }
}

// ------------------------------------------------------------------- check 1: clipped text ----

/// Slack between a glyph's bounds and its clip rectangle before it counts as cut (anti-aliasing
/// and a descender's last pixel).
const CLIP_SLACK: f32 = 2.5;

/// Whether the region a text is clipped to scrolls: a scroll bar (egui's, also while it is
/// invisible at rest) or the board's scroll track stands beside the text. A vertical bar is
/// narrow, at least a third as tall as the clip rectangle, inside it, and to the right of the
/// text; a horizontal one the same, turned.
fn scrolls(frame: &Frame, text: &TextItem) -> bool {
    let clip = text.clip;
    let near = clip.expand(18.0);
    let text_right = text.rows.iter().map(|(rect, _)| rect.right()).fold(f32::MIN, f32::max).min(clip.right());
    frame.fills.iter().any(|bar| {
        let (w, h) = (bar.rect.width(), bar.rect.height());
        let vertical = w <= 14.0 && h >= clip.height() * 0.3 && bar.rect.top() >= near.top() && bar.rect.bottom() <= near.bottom() && bar.rect.left() >= text_right - 18.0 && bar.rect.right() <= near.right();
        let horizontal = h <= 14.0 && w >= clip.width() * 0.3 && bar.rect.left() >= near.left() && bar.rect.right() <= near.right() && bar.rect.center().y >= clip.center().y && bar.rect.bottom() <= near.bottom();
        vertical || horizontal
    })
}

/// Check 1: text that a clip rectangle cuts or hides although that region does not scroll.
/// One sentence per galley.
pub fn clipped_text(frame: &Frame) -> Vec<String> {
    let mut findings = Vec::new();
    for text in &frame.texts {
        let clip = text.clip.intersect(frame.screen.expand(1.0));
        // Painted for nobody (an invisible sizing pass, something slid off screen).
        if !clip.is_positive() || !text.rows.iter().any(|(rect, _)| clip.intersects(*rect)) {
            continue;
        }
        let roomy = clip.expand(CLIP_SLACK);
        let cut: Vec<&(Rect, String)> = text.rows.iter().filter(|(rect, _)| !roomy.contains_rect(*rect)).collect();
        if cut.is_empty() || scrolls(frame, text) {
            continue;
        }
        let (rect, said) = cut[0];
        let hidden = cut.iter().filter(|(rect, _)| !clip.intersects(*rect)).count();
        findings.push(format!(
            "text cut by its clip rectangle with no scroll bar: \"{}\" at {:?} (clip {:?}; {} of {} rows cut, {hidden} hidden entirely)",
            crate::crash_guard::clip(said.trim(), 60),
            rect,
            text.clip,
            cut.len(),
            text.rows.len()
        ));
    }
    findings
}

/// How much of the lower of two text rows another row must cover before the two count as drawn
/// over each other (the rows of one paragraph touch by a descender at most).
const ROW_OVERLAP: f32 = 0.4;
/// The least two rows must share sideways (two glyphs).
const ROW_OVERLAP_WIDTH: f32 = 7.0;
/// From this alpha on a fill hides the text under it (a window's slate, a menu's frame).
const COVERING_FILL: u8 = 150;

/// Text drawn over other text: two rows whose visible glyphs share space with nothing opaque
/// painted between them (a window over the board hides the board's text, which is no finding).
/// The same words painted twice on one spot are a glow, not a collision.
pub fn text_over_text(frame: &Frame) -> Vec<String> {
    let visible: Vec<(usize, Rect, &str)> = frame
        .texts
        .iter()
        .flat_map(|t| {
            let clip = t.clip.intersect(frame.screen);
            t.rows.iter().filter(move |_| clip.is_positive()).map(move |(rect, said)| (t.order, rect.intersect(clip), said.as_str()))
        })
        .filter(|(_, rect, _)| rect.is_positive())
        .collect();
    let mut findings = Vec::new();
    for (a, (order_a, rect_a, said_a)) in visible.iter().enumerate() {
        for (order_b, rect_b, said_b) in &visible[a + 1..] {
            let shared = rect_a.intersect(*rect_b);
            if !shared.is_positive() || shared.width() < ROW_OVERLAP_WIDTH || shared.height() < ROW_OVERLAP * rect_a.height().min(rect_b.height()) {
                continue;
            }
            if said_a.trim() == said_b.trim() {
                continue;
            }
            let (low, high) = (*order_a.min(order_b), *order_a.max(order_b));
            let hidden = frame.fills.iter().any(|f| f.order > low && f.order < high && f.cover >= COVERING_FILL && f.rect.intersect(f.clip).expand(0.5).contains_rect(shared));
            if !hidden {
                findings.push(format!("text drawn over text: \"{}\" at {rect_a:?} and \"{}\" at {rect_b:?}", crate::crash_guard::clip(said_a.trim(), 40), crate::crash_guard::clip(said_b.trim(), 40)));
            }
        }
    }
    findings
}

/// On a board that spans several monitors: text no screen shows whole (it stands in a corner of
/// the box around the monitors that none of them covers, or across a bezel). `screens`: each
/// monitor's part of the frame.
pub fn off_every_screen(frame: &Frame, screens: &[Rect]) -> Vec<String> {
    if screens.len() < 2 {
        return Vec::new();
    }
    frame
        .texts
        .iter()
        .flat_map(|t| {
            let clip = t.clip.intersect(frame.screen);
            t.rows.iter().filter(move |(rect, _)| clip.is_positive() && clip.intersects(*rect)).map(move |(rect, said)| (rect.intersect(clip), said))
        })
        .filter(|(rect, _)| !screens.iter().any(|s| s.expand(CLIP_SLACK).contains_rect(*rect)))
        .map(|(rect, said)| format!("text that no monitor shows whole: \"{}\" at {rect:?}", crate::crash_guard::clip(said.trim(), 40)))
        .collect()
}

/// Text the layout shortened with an ellipsis (a truncated title or label).
pub fn elided_text(frame: &Frame) -> Vec<String> {
    frame.texts.iter().filter(|t| t.elided && t.clip.is_positive()).map(|t| format!("text shortened with an ellipsis: \"{}\"", crate::crash_guard::clip(t.rows[0].1.trim(), 60))).collect()
}

/// Check 5 (rendered text): pictographs and emoji.
pub fn pictographs(frame: &Frame) -> Vec<String> {
    frame
        .texts
        .iter()
        .flat_map(|t| t.rows.iter())
        .filter(|(_, said)| said.chars().any(crate::live_self_tester::is_pictograph))
        .map(|(_, said)| format!("a pictograph in rendered text: \"{}\"", crate::crash_guard::clip(said.trim(), 60)))
        .collect()
}

// -------------------------------------------------------------------- check 2: overlaps ----

/// Check 2: every pair of rectangles that overlap (touching edges do not count).
pub fn overlaps(rects: &[(String, Rect)]) -> Vec<String> {
    let mut findings = Vec::new();
    for a in 0..rects.len() {
        for b in a + 1..rects.len() {
            if rects[a].1.shrink(0.5).intersects(rects[b].1.shrink(0.5)) {
                findings.push(format!("{} {:?} overlaps {} {:?}", rects[a].0, rects[a].1, rects[b].0, rects[b].1));
            }
        }
    }
    findings
}

// ---------------------------------------------------------------- check 4: theme colours ----

/// A status colour that means the same in every theme (the map pin, a failed probe, the muted
/// microphone): the one colour a stroke may have that is not the theme's.
const STATUS_RED: [u8; 3] = [0xFF, 0x2A, 0x55];

/// A colour as (hue in degrees, saturation 0-1, value 0-255), alpha undone. An additive glow
/// (alpha 0) is its own light.
fn hsv(colour: Color32) -> (f32, f32, f32) {
    let [r, g, b, a] = colour.to_array();
    let scale = if a == 0 { 1.0 } else { 255.0 / a as f32 };
    let rgb = [r, g, b].map(|c| (c as f32 * scale).min(255.0) as u8);
    let (h, s, v) = theme_engine::to_hsv(rgb);
    (h, s, v * 255.0)
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// The hues of a theme's saturated colours (its neutral members are inks, not hues).
fn theme_hues(palette: &ThemePalette) -> Vec<f32> {
    [palette.core_primary, palette.core_secondary, palette.core_hot_center, palette.hud_frame_border, palette.hud_frame_glow, palette.hud_bracket_color].into_iter().map(theme_engine::to_hsv).filter(|(_, s, _)| *s >= SATURATED).map(|(h, _, _)| h).collect()
}

/// Below this saturation a colour is a grey or a white, not a hue.
const SATURATED: f32 = 0.28;
/// How far a stroke's hue may sit from the theme's (blends and the HDR bloom shift it a little).
const HUE_SLACK: f32 = 32.0;

/// Whether `colour` belongs to the theme: one of its hues (or between two of them), a neutral
/// ink (white data ink and the dimmed brackets), near-black, or the status red.
fn of_theme(colour: Color32, hues: &[f32]) -> bool {
    let (h, s, v) = hsv(colour);
    if v < 44.0 || s < SATURATED {
        return true;
    }
    let (status, _, _) = theme_engine::to_hsv(STATUS_RED);
    if hue_distance(h, status) <= 8.0 || pill_colour(colour) {
        return true;
    }
    if hues.iter().any(|t| hue_distance(h, *t) <= HUE_SLACK) {
        return true;
    }
    // A blend of two theme colours lies on the short arc between their hues.
    hues.iter().enumerate().any(|(i, a)| hues[i + 1..].iter().any(|b| {
        let arc = hue_distance(*a, *b);
        arc < 150.0 && (hue_distance(h, *a) + hue_distance(h, *b) - arc).abs() < 1.0
    }))
}

/// Whether `colour` is one of the status pill's own (green listening, amber thinking, red
/// speaking, grey muted or standby, as on the reference board): status colours, the same in
/// every theme. Only those colours themselves, not their hues.
fn pill_colour(colour: Color32) -> bool {
    let [r, g, b, a] = colour.to_array();
    if a == 0 {
        return false;
    }
    // A fading pill's colours are premultiplied: undone, they are the pill's own up to the
    // rounding, which grows as the alpha falls.
    let scale = 255.0 / a as f32;
    let slack = 2.0 + scale;
    crate::gui::PILL_WORDS.iter().flat_map(|word| crate::gui::pill_colours(word)).any(|pill| {
        let [pr, pg, pb, _] = pill.to_array();
        [(r, pr), (g, pg), (b, pb)].iter().all(|&(c, p)| (c as f32 * scale - p as f32).abs() <= slack)
    })
}

/// Check 4: every outline and every mesh colour in the frame that is not of `palette` (named
/// `theme` in the findings). Strokes thinner than a hair or nearly transparent are noise.
pub fn off_theme(frame: &Frame, theme: &str, palette: &ThemePalette) -> Vec<String> {
    let hues = theme_hues(palette);
    let mut findings: Vec<String> = Vec::new();
    let mut reported: HashSet<[u8; 3]> = HashSet::new();
    let mut judge = |what: &str, rect: Rect, colour: Color32| {
        if of_theme(colour, &hues) {
            return;
        }
        let [r, g, b, _] = colour.to_array();
        // One finding per colour (rounded): the first place it was seen.
        if reported.insert([r & 0xF0, g & 0xF0, b & 0xF0]) {
            findings.push(format!("{what} at {rect:?} is #{r:02X}{g:02X}{b:02X}, which is not a colour of the theme '{theme}'"));
        }
    };
    for s in frame.strokes.iter().filter(|s| s.width >= 0.5 && (s.colour.a() >= 40 || s.colour.a() == 0) && s.clip.is_positive()) {
        judge("an outline", s.rect, s.colour);
    }
    for (rect, colour) in &frame.mesh_colours {
        judge("a mesh", *rect, *colour);
    }
    findings
}

/// The themes of the matrix: the eight presets and three custom themes (Sir's own from
/// `config/custom_themes.json` first, filled up with probe colours no preset uses). Each as
/// (palette name for `theme_engine::follow`, label).
pub fn themes() -> Vec<(String, String)> {
    let mut list: Vec<(String, String)> = theme_engine::PRESETS.iter().map(|(name, _)| (name.to_string(), theme_engine::label(name))).collect();
    let mut custom: Vec<(String, String)> = theme_engine::custom_themes().into_iter().take(3).map(|t| (t.key(), format!("custom theme {}", t.name))).collect();
    for probe in ["custom:#FF00C8", "custom:#B4FF00", "custom:#7A5CFF"] {
        if custom.len() < 3 {
            custom.push((probe.to_string(), format!("custom colour {}", &probe[7..])));
        }
    }
    list.extend(custom);
    list
}

// ------------------------------------------------------------------- check 7: animation ----

/// The frames at which a series changes by more than `limit` from one frame to the next.
pub fn jumps(series: &[f32], limit: f32) -> Vec<(usize, f32)> {
    series.windows(2).enumerate().filter(|(_, w)| (w[1] - w[0]).abs() > limit).map(|(i, w)| (i + 1, w[1] - w[0])).collect()
}

/// The largest share of its whole way a smooth glide covers in one frame of `dt` seconds: the
/// first step of an exponential ease with a 60 ms time constant (the emblem's own is 71 ms).
/// Anything larger in a single frame is a jump.
pub fn smooth_step(dt: f32) -> f32 {
    1.0 - (-dt / 0.06).exp()
}

/// Whether a series only ever moves in one direction (`rising`), small float noise aside.
pub fn monotonic(series: &[f32], rising: bool) -> bool {
    series.windows(2).all(|w| if rising { w[1] >= w[0] - 1e-4 } else { w[1] <= w[0] + 1e-4 })
}

/// Check 7 on the emblem: its transitions, ring 5, ring 6, the idle switch. `pass` varies the
/// screen, the frame rate and the seeds.
pub fn check_emblem(screen: Rect, pass: usize) -> Vec<String> {
    let mut findings = Vec::new();
    let fps = [180.0f32, 144.0, 60.0, 120.0, 30.0][pass % 5];
    let dt = 1.0 / fps;
    let mut orb = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
    // Settled in the front pose first.
    for _ in 0..(2.0 * fps) as usize {
        orb.step(dt, 0.0, false);
        orb.glide(screen, dt);
    }
    let diagonal = screen.size().length();
    let route = [
        (RoseEmblemState::ThinkingGyroscope, true, "front emblem -> thinking gyroscope"),
        (RoseEmblemState::CornerDocked, false, "thinking gyroscope -> corner dock"),
        (RoseEmblemState::AwakeFrontEmblem, false, "corner dock -> front emblem"),
        (RoseEmblemState::CornerDocked, false, "front emblem -> corner dock"),
        (RoseEmblemState::ThinkingGyroscope, true, "corner dock -> thinking gyroscope"),
        (RoseEmblemState::AwakeFrontEmblem, false, "thinking gyroscope -> front emblem"),
    ];
    for (state, busy, name) in route {
        let before = orb.frame(screen);
        orb.set_state(state, busy);
        let mut flat = vec![before.flat];
        let mut dock = vec![before.dock];
        let mut x = vec![before.centre.x];
        let mut y = vec![before.centre.y];
        let mut radius = vec![before.radius];
        for _ in 0..(2.5 * fps) as usize {
            orb.step(dt, 0.0, busy);
            orb.glide(screen, dt);
            let f = orb.frame(screen);
            flat.push(f.flat);
            dock.push(f.dock);
            x.push(f.centre.x);
            y.push(f.centre.y);
            radius.push(f.radius);
        }
        let after = orb.frame(screen);
        // No frame moves a pose more than a sixth of the way (the eased peak at 30 FPS), no
        // frame moves the emblem more than a sixth of the screen's diagonal.
        let step = (9.0 / fps).clamp(0.06, 0.34);
        for (what, series, limit) in [("flatness", &flat, step), ("docking", &dock, step), ("x", &x, diagonal * step), ("y", &y, diagonal * step), ("radius", &radius, (before.radius - after.radius).abs().max(40.0) * step.max(0.12))] {
            if let Some((frame, by)) = jumps(series, limit).first() {
                findings.push(format!("{name} at {fps} FPS: {what} jumps by {by:.3} in frame {frame} (limit {limit:.3})"));
            }
            if series.iter().any(|v| !v.is_finite()) {
                findings.push(format!("{name}: {what} is not a number"));
            }
        }
        if (after.flat - before.flat).abs() > 0.5 && !monotonic(&flat, after.flat > before.flat) {
            findings.push(format!("{name}: the fold is not monotonic"));
        }
        if (after.dock - before.dock).abs() > 0.5 && !monotonic(&dock, after.dock > before.dock) {
            findings.push(format!("{name}: the docking is not monotonic"));
        }
        if orb.state() != state {
            findings.push(format!("{name}: the emblem is in {:?}", orb.state()));
        }
    }
    // Ring 5: three clusters 120 degrees apart; every tick's surge is proportional to the voice
    // and symmetric around its cluster; silent below the noise floor.
    let gaps: Vec<f32> = (0..3).map(|i| (emblem::TACH_CLUSTERS[(i + 1) % 3] - emblem::TACH_CLUSTERS[i]).rem_euclid(std::f32::consts::TAU)).collect();
    if gaps.iter().any(|g| (g - std::f32::consts::TAU / 3.0).abs() > 1e-4) {
        findings.push(format!("ring 5: the clusters are not 120 degrees apart: {gaps:?}"));
    }
    let rest = emblem::tach_ticks(0.0);
    let full = emblem::tach_ticks(1.0);
    for voice in [0.1f32, 0.25, 0.37 + 0.1 * pass as f32, 0.9] {
        let ticks = emblem::tach_ticks(voice);
        if ticks.len() != full.len() {
            findings.push(format!("ring 5: {} ticks at voice {voice}, {} at full voice", ticks.len(), full.len()));
            continue;
        }
        for (k, ((_, len), ((_, silent), (_, loud)))) in ticks.iter().zip(rest.iter().zip(&full)).enumerate() {
            let expected = silent + voice.min(1.0) * (loud - silent);
            if (len - expected).abs() > 0.01 {
                findings.push(format!("ring 5: tick {k} is {len:.2} px at voice {voice}, proportional would be {expected:.2} px"));
            }
        }
        let n = ticks.len();
        if (0..n / 2).any(|k| (ticks[k].1 - ticks[n - 1 - k].1).abs() > 1e-3) {
            findings.push(format!("ring 5: the cluster is not symmetric at voice {voice}"));
        }
    }
    if emblem::tach_ticks(0.01).iter().zip(&rest).any(|(a, b)| a != b) {
        findings.push("ring 5: the ticks move below the noise floor".to_string());
    }
    // Ring 6 keeps turning: its main arc advances every frame, its tracer runs the other way.
    let start = 3.0 + pass as f32 * 17.3;
    let arcs: Vec<(f32, f32, f32, f32)> = (0..(4.0 * fps) as usize).map(|i| emblem::halo_arcs(start + i as f32 * dt)).collect();
    if !arcs.windows(2).all(|w| w[1].0 > w[0].0 && w[1].2 < w[0].2) {
        findings.push("ring 6: the arcs do not advance every frame".to_string());
    }
    if arcs.iter().any(|a| a.1 <= 0.0 || a.3 <= 0.0) {
        findings.push("ring 6: an arc has no length".to_string());
    }
    // The idle switch: never before 120 s of idle, always by 240 s; it holds 120-240 s; any
    // activity ends it.
    let mut idle = emblem::IdleMorph::new(0x9E37_79B9u32.wrapping_mul(pass as u32 + 1));
    let mut clock = 0.0f32;
    let mut began: Option<f32> = None;
    let mut ended: Option<f32> = None;
    while clock < 600.0 && ended.is_none() {
        clock += 0.25;
        let shows = idle.step(clock, true);
        match (shows, began) {
            (true, None) => began = Some(clock),
            (false, Some(_)) => ended = Some(clock),
            _ => {}
        }
    }
    match (began, ended) {
        (Some(began), Some(ended)) => {
            if !(emblem::IdleMorph::INTERVAL.0..=emblem::IdleMorph::INTERVAL.1 + 0.5).contains(&began) {
                findings.push(format!("idle switch: began after {began} s of idle (120-240 s wanted)"));
            }
            let held = ended - began;
            if !(emblem::IdleMorph::DURATION.0 - 0.5..=emblem::IdleMorph::DURATION.1 + 0.5).contains(&held) {
                findings.push(format!("idle switch: held {held} s (120-240 s wanted)"));
            }
        }
        _ => findings.push(format!("idle switch: began {began:?}, ended {ended:?} within 600 s")),
    }
    let mut idle = emblem::IdleMorph::new(7 + pass as u32);
    let mut clock = 0.0;
    while !idle.step(clock, true) && clock < 400.0 {
        clock += 0.5;
    }
    if idle.step(clock + 0.5, false) || idle.step(clock + 1.0, true) {
        findings.push("idle switch: activity does not end it at once".to_string());
    }
    findings
}

// ------------------------------------------------------------------------------- stage ----

/// A private egui context of one resolution, stepped frame by frame with a simulated pointer.
pub struct Stage {
    snap: crate::ui_snapshot::Snapshot,
    clock: f64,
    pointer: Option<Pos2>,
    events: Vec<egui::Event>,
}

impl Stage {
    pub fn new(size: Vec2) -> Self {
        Self { snap: crate::ui_snapshot::Snapshot::new(size), clock: 0.0, pointer: None, events: Vec::new() }
    }

    pub fn screen(&self) -> Rect {
        self.snap.screen()
    }

    /// One frame of `ui` (a sixtieth of a second of egui's time later).
    pub fn frame(&mut self, ui: &mut dyn FnMut(&egui::Context)) -> Frame {
        self.clock += 1.0 / 60.0;
        let screen = self.screen();
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(self.clock), predicted_dt: 1.0 / 60.0, events: std::mem::take(&mut self.events), focused: true, ..Default::default() };
        let output = self.snap.frame(input, |ctx| {
            crate::gui::apply_hud_visuals(ctx);
            ui(ctx);
            crate::gui::settle_new_areas(ctx);
        });
        capture(output, screen)
    }

    /// `count` frames; the last one.
    pub fn frames(&mut self, count: usize, ui: &mut dyn FnMut(&egui::Context)) -> Frame {
        let mut last = self.frame(ui);
        for _ in 1..count {
            last = self.frame(ui);
        }
        last
    }

    /// Real time passes (the board's cascade and the fades run on the wall clock).
    pub fn wait(&mut self, secs: f32) {
        std::thread::sleep(Duration::from_secs_f32(secs));
        self.clock += secs as f64;
    }

    pub fn move_pointer(&mut self, to: Pos2) {
        self.pointer = Some(to);
        self.events.push(egui::Event::PointerMoved(to));
    }

    pub fn press(&mut self, button: PointerButton, down: bool) {
        if let Some(pos) = self.pointer {
            self.events.push(egui::Event::PointerButton { pos, button, pressed: down, modifiers: egui::Modifiers::NONE });
        }
    }

    /// A click at `at`; the frame after it.
    pub fn click(&mut self, at: Pos2, button: PointerButton, ui: &mut dyn FnMut(&egui::Context)) -> Frame {
        self.move_pointer(at);
        self.frame(ui);
        self.press(button, true);
        self.frame(ui);
        self.press(button, false);
        self.frame(ui);
        self.frame(ui)
    }

    /// A drag with the primary button from `from` by `delta` in `steps` frames.
    pub fn drag(&mut self, from: Pos2, delta: Vec2, steps: usize, ui: &mut dyn FnMut(&egui::Context)) -> Frame {
        self.move_pointer(from);
        self.frame(ui);
        self.press(PointerButton::Primary, true);
        self.frame(ui);
        let steps = steps.max(2);
        for n in 1..=steps {
            self.move_pointer(from + delta * (n as f32 / steps as f32));
            self.frame(ui);
        }
        self.press(PointerButton::Primary, false);
        self.frame(ui);
        self.frame(ui)
    }

    /// The frame as pixels (1 px per point).
    pub fn raster(&self, frame: &Frame) -> crate::ui_snapshot::Canvas {
        self.snap.rasterise(frame.shapes.clone(), frame.pixels_per_point)
    }
}

/// The most theme-like pixel within a pixel of `at` (the brightest of its neighbourhood: a 1 px
/// line may straddle two pixel rows).
fn brightest_near(canvas: &crate::ui_snapshot::Canvas, at: Pos2) -> [u8; 3] {
    let mut best = [0u8; 3];
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let (x, y) = (at.x as i32 + dx, at.y as i32 + dy);
            if x >= 0 && y >= 0 && (x as usize) < canvas.width && (y as usize) < canvas.height {
                let p = canvas.pixel(x as usize, y as usize);
                if p.iter().map(|c| *c as u32).sum::<u32>() > best.iter().map(|c| *c as u32).sum::<u32>() {
                    best = p;
                }
            }
        }
    }
    best
}

/// Pixel-level border check: along the four edges of the frame `block`, the rasterised border
/// pixels carry one of the theme's hues (a neutral theme: no hue at all). The share of the
/// sampled points that do.
pub fn border_pixels(canvas: &crate::ui_snapshot::Canvas, block: Rect, palette: &ThemePalette) -> f32 {
    let hues = theme_hues(palette);
    let (_, border_s, _) = theme_engine::to_hsv(palette.hud_frame_border);
    let mut points = Vec::new();
    for t in [0.2f32, 0.35, 0.5, 0.65, 0.8] {
        points.push(pos2(block.left() + block.width() * t, block.top()));
        points.push(pos2(block.left() + block.width() * t, block.bottom() - 1.0));
        points.push(pos2(block.left(), block.top() + block.height() * t));
        points.push(pos2(block.right() - 1.0, block.top() + block.height() * t));
    }
    let good = points
        .iter()
        .filter(|p| {
            let [r, g, b] = brightest_near(canvas, **p);
            let (h, s, v) = hsv(Color32::from_rgb(r, g, b));
            v >= 24.0 && if border_s < SATURATED { s < SATURATED + 0.2 } else { s >= 0.15 && hues.iter().any(|t| hue_distance(h, *t) <= HUE_SLACK) }
        })
        .count();
    good as f32 / points.len() as f32
}

// ------------------------------------------------------------------------- the scenes ----

/// Texts of five kinds for the scenes: short, a paragraph, a very long one, German, figures.
fn sample_text(pass: usize, n: usize) -> (String, String) {
    let headline = ["Arc reactor output", "Thermal headroom under sustained load", "What the palladium core costs per hour at full power", "Übersicht für die mündliche Prüfung", "Q3: 63.7 % at 4.2 GW"][(pass + n) % 5];
    let body = [
        "Output holds at 3 GJ per second.",
        "The reactor sustains its output without drift. Cooling keeps the casing below sixty degrees, and the reserve covers a full discharge. No maintenance is due before the next cycle.",
        "Palladium decays in the core faster than the housing can shed the heat it makes. Each cycle shortens the next; the replacement element removes the decay product entirely, which is why the new core runs cooler at three times the output. The lattice was verified against the model of the old exhibition, and the synthesis needs a particle path that clears every wall of the workshop. Nothing else in the suit changes: the same bus, the same converters, the same emergency cell behind the plate. The figures on the right are measured, not projected, and they hold across twelve consecutive discharges.",
        "Für die mündliche Prüfung zählen drei Themen: Ableitungsregeln, die Übergänge zwischen Größen und Maßeinheiten und die Begründung der Lösungswege. Wer die Schritte laut erklärt, gewinnt Zeit; größere Rechnungen gehören auf das Blatt, nicht in den Kopf.",
        "63.7 % of 4.2 GW is 2.675 GW; at 0.12 EUR/kWh that is 321,000 EUR per hour. Peak 5.1 GW (t = 14:05), mean 3.9 GW, sigma 0.4 GW, n = 1,284 samples.",
    ][(pass + 2 * n) % 5];
    (headline.to_string(), body.to_string())
}

/// A report of `count` blocks of every kind in turn, with the texts of `pass`.
pub fn sample_report(count: usize, pass: usize) -> Report {
    let blocks = (0..count)
        .map(|n| {
            let (headline, body) = sample_text(pass, n);
            let label = format!("NOTE // {:02} - {}", n + 1, ["OUTPUT", "THERMALS", "COST", "PRÜFUNG", "FIGURES"][(pass + n) % 5]);
            match (n + pass) % 6 {
                0 => Block::new(label, "FACT", "SOURCE: STARK INDUSTRIES", BlockKind::NoteStack { notes: (1..=3).map(|p| Note { headline: format!("{headline} ({p}/3)"), body: body.clone() }).collect() }),
                1 => Block::new(format!("CHART // {:02}", n + 1), "MEASURED", "SOURCE: TELEMETRY", BlockKind::BarChart { unit: "GW".into(), bars: (0..5).map(|k| Bar { label: format!("Q{}", k + 1), value: 2.0 + ((k * 7 + pass * 3 + n) % 9) as f32 * 0.4 }).collect(), caption: headline }),
                2 => Block::new(format!("FIGURE // {:02}", n + 1), "STAT", "SOURCE: LAB", BlockKind::HeroFigure { header: headline, figure: ["63.7 %", "100+", "4.2 GW", "≈20m", "1,284"][(pass + n) % 5].into(), note: body }),
                3 => Block::new(label, "NOTE", "SOURCE: NOTES", BlockKind::NoteStack { notes: vec![Note { headline, body }] }),
                4 => Block::new(format!("COMMS // {:02}", n + 1), "LIVE", "SOURCE: DIALOGUE", BlockKind::DialogueFeed { heard: headline, reply: body, streaming: false }),
                _ => Block::new(format!("MAP // {:02}", n + 1), "PLACE", "SOURCE: OPENSTREETMAP", BlockKind::DarkVectorMap { place: headline, lat: 48.34, lon: 7.87, span: 0.01, roads: Vec::new(), water: Vec::new(), pending: false }),
            }
        })
        .collect();
    Report { topic: format!("Visual verifier, pass {}", pass + 1), created_at: "2026-11-06 10:00:00".into(), blocks, sources: vec!["https://example.com/source".into()], id: None }
}

/// The docked emblem's box on `screen` (what a full board must keep free).
pub fn docked_orb(screen: Rect) -> Rect {
    Rect::from_center_size(crate::hud_dock::docked_centre(screen, 0.0), Vec2::splat(emblem::DOCKED_DIAMETER))
}

/// Checks 1, 2, 3, 5 and 6 on the board with a report of `count` blocks on a `size` screen:
/// every block has a slot, no slot overlaps another or the docked orb, no text is cut, a block
/// follows a drag by exactly its delta, and (with a stacked note) the page keys are large
/// enough and turn the page. The board is the caller's for the duration (one scene at a time).
pub fn check_board(size: Vec2, count: usize, pass: usize, rt: &tokio::runtime::Handle) -> Vec<String> {
    let mut findings = Vec::new();
    let mut stage = Stage::new(size);
    let screen = stage.screen();
    let area = crate::gui::board_area(screen, 0.0, 0.0);
    let report = sample_report(count, pass);
    crate::hud_board::present(report.clone());
    let mut view = BoardView::default();
    let paint = |ctx: &egui::Context, view: &mut BoardView| {
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
            view.paint(ui, area, rt);
        });
    };
    // The cascade: 120 ms, then 30 ms a block and a quarter second for the last to settle.
    stage.frames(3, &mut |ctx| paint(ctx, &mut view));
    stage.wait(0.5 + 0.03 * count as f32);
    let frame = stage.frames(3, &mut |ctx| paint(ctx, &mut view));

    // Check 2: every block has a slot; no slot overlaps another or the docked orb.
    let slots = crate::hud_board::layout(&report, area, false);
    let mut rects: Vec<(String, Rect)> = Vec::new();
    for (n, slot) in slots.iter().enumerate() {
        match slot {
            Some(rect) => {
                if !area.expand(1.0).contains_rect(*rect) {
                    findings.push(format!("block {} of {count} leaves the board: {rect:?} (board {area:?})", n + 1));
                }
                rects.push((format!("block {}", n + 1), *rect));
            }
            None => findings.push(format!("block {} of {count} has no place on the {}x{} board", n + 1, size.x, size.y)),
        }
    }
    rects.push(("the docked orb".to_string(), docked_orb(screen)));
    findings.extend(overlaps(&rects));
    // Checks 1 and 5: no text cut, no pictographs.
    findings.extend(clipped_text(&frame));
    findings.extend(pictographs(&frame));
    if frame.texts.is_empty() {
        findings.push("the board painted no text at all".to_string());
    }

    // Check 3: a block follows the pointer by exactly the drag.
    let target = (pass + count) % count;
    if let Some(Some(slot)) = slots.get(target) {
        let delta = [vec2(37.0, -23.0), vec2(-41.0, 18.0), vec2(12.0, 44.0), vec2(-9.0, -31.0), vec2(55.0, 7.0)][pass % 5];
        // Off the page keys and the scroll track: the block's upper middle.
        let grip = pos2(slot.center().x, slot.top() + slot.height() * 0.3);
        stage.drag(grip, delta, 6, &mut |ctx| paint(ctx, &mut view));
        let moved = view.moved_by(target);
        if (moved - delta).length() > 0.51 {
            findings.push(format!("block {} was dragged by {delta:?} and moved by {moved:?}", target + 1));
        }
        if crate::hud_board::spotlight_active() {
            findings.push(format!("dragging block {} maximised it", target + 1));
            crate::hud_board::minimize();
        }
    }

    // Check 6: the page keys of the first stacked note.
    if let (Some([previous, next]), Some((page, pages))) = (crate::hud_board::page_keys(area), crate::hud_board::front_page()) {
        for (name, key) in [("[ < PREV ]", previous), ("[ NEXT > ]", next)] {
            if key.width() < MIN_PAGE_KEY[0] || key.height() < MIN_PAGE_KEY[1] {
                findings.push(format!("{name} is {}x{} px (at least {}x{})", key.width(), key.height(), MIN_PAGE_KEY[0], MIN_PAGE_KEY[1]));
            }
        }
        // The keys belong to the first stacked block; a dragged block took its keys along.
        let stacked = report.blocks.iter().position(|b| matches!(&b.kind, BlockKind::NoteStack { notes } if notes.len() > 1)).unwrap_or(0);
        let offset = view.moved_by(stacked);
        stage.click(next.center() + offset, PointerButton::Primary, &mut |ctx| paint(ctx, &mut view));
        let after = crate::hud_board::front_page().map(|(p, _)| p);
        if after != Some((page + 1) % pages) {
            findings.push(format!("a click on [ NEXT > ] left the stack on page {:?} (was {page} of {pages})", after));
        }
        stage.click(previous.center() + offset, PointerButton::Primary, &mut |ctx| paint(ctx, &mut view));
        if crate::hud_board::front_page().map(|(p, _)| p) != Some(page) {
            findings.push("a click on [ < PREV ] did not turn back".to_string());
        }
        if crate::hud_board::spotlight_active() {
            findings.push("a click on a page key maximised the block".to_string());
        }
    }
    crate::hud_board::close_board();
    findings
}

/// Check 4 on the board: one report under every theme of the matrix; every outline and mesh
/// colour must be the theme's, and the rasterised border of the first block must carry the
/// theme's border hue. The theme is put back afterwards.
pub fn check_board_themes(size: Vec2, pass: usize, rt: &tokio::runtime::Handle) -> Vec<String> {
    let mut findings = Vec::new();
    let before = theme_engine::active_name();
    let mut stage = Stage::new(size);
    let area = crate::gui::board_area(stage.screen(), 0.0, 0.0);
    let report = sample_report(4 + pass % 3, pass);
    crate::hud_board::present(report.clone());
    let mut view = BoardView::default();
    let mut paint = |ctx: &egui::Context| {
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
            view.paint(ui, area, rt);
        });
    };
    stage.frames(3, &mut paint);
    stage.wait(0.75);
    let first = crate::hud_board::layout(&report, area, false).into_iter().flatten().next();
    let mut seen: Vec<(String, ThemePalette)> = Vec::new();
    for (name, label) in themes() {
        theme_engine::follow(&name);
        theme_engine::finish_fade();
        seen.push((label, theme_engine::current()));
    }
    theme_engine::follow(&before);
    theme_engine::finish_fade();
    for (n, (label, palette)) in seen.iter().enumerate() {
        theme_engine::follow(&themes()[n].0);
        theme_engine::finish_fade();
        let frame = stage.frames(2, &mut paint);
        findings.extend(off_theme(&frame, label, palette));
        if frame.strokes.is_empty() {
            findings.push(format!("{label}: the board painted no outline"));
        }
        // Pixel level, on a third of the themes each pass (rasterising is the slow part): the
        // first block's own frame (the largest outline in its slot; the footer line sits
        // under it), sampled along its four edges.
        if let Some(slot) = first.filter(|_| n % 3 == pass % 3) {
            let framed = frame.strokes.iter().map(|s| s.rect).filter(|r| slot.expand(1.0).contains_rect(*r) && r.width() >= slot.width() * 0.8 && r.height() >= slot.height() * 0.6).max_by(|a, b| (a.width() * a.height()).total_cmp(&(b.width() * b.height())));
            match framed {
                None => findings.push(format!("{label}: the first block has no frame outline")),
                Some(block) => {
                    let share = border_pixels(&stage.raster(&frame), block, palette);
                    if share < 0.9 {
                        findings.push(format!("{label}: only {:.0} % of the first block's border pixels carry the theme's colours", share * 100.0));
                    }
                }
            }
        }
    }
    theme_engine::follow(&before);
    theme_engine::finish_fade();
    crate::hud_board::close_board();
    findings
}

/// One in-app text block of the window scene.
fn window_id(n: usize) -> String {
    format!("verifier_block_{n}")
}

/// Checks 1, 2, 3 and 5 on in-app block windows: `count` blocks of varied sizes opened one
/// after the other on a `size` screen with the orb in the centre. None may overlap another or
/// the orb, none may leave the board, no text may be cut, and a block must follow a drag by
/// exactly its delta. With `menus`, the right-click menu is exercised entry by entry and the
/// search across blocks is checked.
pub fn check_windows(size: Vec2, count: usize, pass: usize, menus: bool) -> Vec<String> {
    let mut findings = Vec::new();
    let mut stage = Stage::new(size);
    let screen = stage.screen();
    let bounds = crate::gui::block_bounds(screen);
    let orb = Rect::from_center_size(screen.center(), Vec2::splat(emblem::centre_radius(screen) * 2.3));
    let sizes = [[820.0, 600.0], [480.0, 640.0], [900.0, 560.0], [540.0, 330.0], [420.0, 240.0], [560.0, 300.0]];
    let contents: Vec<String> = (0..count).map(|n| sample_text(pass, n).1).collect();
    let mut open = 0usize;
    let mut closed: Vec<usize> = Vec::new();
    let scene = |ctx: &egui::Context, open: usize, closed: &mut Vec<usize>| {
        crate::hud_dock::begin_frame();
        crate::hud_dock::publish_obstacles(Some(orb), Vec::new());
        for n in 0..open {
            if closed.contains(&n) {
                continue;
            }
            // The order of sizes changes with the pass.
            let size = sizes[(n * (pass + 1) + pass) % sizes.len()];
            if crate::gui::text_block(ctx, &window_id(n), &format!("Verifier // block {:02}", n + 1), size, &contents[n]) {
                closed.push(n);
            }
        }
    };
    let rects = |open: usize, closed: &[usize]| -> Vec<(String, Rect)> {
        (0..open).filter(|n| !closed.contains(n)).filter_map(|n| crate::hud_dock::block_rect(&window_id(n)).map(|r| (format!("block {}", n + 1), r))).collect()
    };
    // The blocks arrive one after the other, as Jarvis opens them.
    let mut frame = Frame::default();
    for n in 1..=count {
        open = n;
        frame = stage.frames(7, &mut |ctx| scene(ctx, open, &mut closed));
    }
    let placed = rects(open, &closed);
    if placed.len() != count {
        findings.push(format!("{} of {count} blocks were drawn", placed.len()));
    }
    let mut all = placed.clone();
    all.push(("the orb".to_string(), orb));
    findings.extend(overlaps(&all));
    for (name, rect) in &placed {
        if !bounds.expand(1.0).contains_rect(*rect) {
            findings.push(format!("{name} leaves the board: {rect:?} (bounds {bounds:?})"));
        }
        if rect.width() < crate::hud_dock::MIN_BLOCK[0] - 0.5 || rect.height() < crate::hud_dock::MIN_BLOCK[1] - 0.5 {
            findings.push(format!("{name} is smaller than the smallest block: {rect:?}"));
        }
    }
    findings.extend(clipped_text(&frame));
    findings.extend(pictographs(&frame));
    // A block's title is never shortened: every block is at least as wide as its header needs.
    findings.extend(elided_text(&frame));

    // Check 3: a block follows the pointer by exactly the drag (a drag that keeps it inside).
    let target = (pass * 3 + 1) % count;
    if let Some(home) = crate::hud_dock::block_rect(&window_id(target)) {
        let want = [vec2(23.0, 17.0), vec2(-19.0, 11.0), vec2(14.0, -9.0), vec2(-27.0, -13.0), vec2(31.0, 5.0)][pass % 5];
        // Towards the inside of the board, whichever side the block is on.
        let delta = vec2(if home.center().x > bounds.center().x { -want.x.abs() } else { want.x.abs() }, if home.center().y > bounds.center().y { -want.y.abs() } else { want.y.abs() });
        // The title row, off the chips on its right. Frame by frame, so that a block that does
        // not follow can be told where it lost the pointer. The button goes down in the frame
        // after the pointer arrives, which is also the frame after the title's hint first
        // shows: the hardest moment (see `gui::settle_new_areas`).
        let grip = pos2(home.left() + 90.0, home.top() + 18.0);
        let mut trace: Vec<String> = Vec::new();
        let mut step = |stage: &mut Stage, what: &str, closed: &mut Vec<usize>| {
            stage.frame(&mut |ctx| scene(ctx, open, closed));
            if let Some(now) = crate::hud_dock::block_rect(&window_id(target)) {
                trace.push(format!("{what}: {:.1},{:.1}{}", now.min.x - home.min.x, now.min.y - home.min.y, if stage.snap.ctx.dragged_id().is_some() { " (egui drags)" } else { "" }));
            }
        };
        stage.move_pointer(grip);
        step(&mut stage, "over", &mut closed);
        stage.press(PointerButton::Primary, true);
        step(&mut stage, "down", &mut closed);
        for n in 1..=6 {
            stage.move_pointer(grip + delta * (n as f32 / 6.0));
            step(&mut stage, "move", &mut closed);
        }
        stage.press(PointerButton::Primary, false);
        step(&mut stage, "up", &mut closed);
        step(&mut stage, "rest", &mut closed);
        match crate::hud_dock::block_rect(&window_id(target)) {
            Some(now) if (now.min - home.min - delta).length() <= 0.51 && (now.size() - home.size()).length() <= 0.51 => {}
            Some(now) => {
                // What egui had under the pointer where the title was grabbed.
                let under = stage.snap.ctx.layer_id_at(grip).map(|layer| (0..count).find(|n| egui::Id::new(("hud_block", window_id(*n))) == layer.id).map_or_else(|| format!("{layer:?}"), |n| format!("block {}", n + 1)));
                findings.push(format!("block {} was dragged by {delta:?} from its title at {grip:?} and went from {home:?} to {now:?} (under the pointer: {under:?}; {})", target + 1, trace.join(" / ")));
            }
            None => findings.push(format!("block {} vanished while it was dragged", target + 1)),
        }
    }

    if menus {
        findings.extend(check_block_menu(&mut stage, &mut |ctx| scene(ctx, open, &mut closed), 0, &contents[0], bounds));
        // The search: the block that carries the word is found, and only blocks that carry it.
        let word = contents[0].split_whitespace().find(|w| w.len() > 5).unwrap_or("output").trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        crate::hud_dock::set_search(&word);
        stage.frames(2, &mut |ctx| scene(ctx, open, &mut closed));
        let hits = crate::hud_dock::search_hits();
        let expected: Vec<String> = (0..count).filter(|n| !closed.contains(n) && contents[*n].to_lowercase().contains(&word)).map(window_id).collect();
        if hits.iter().map(|(id, _)| id.clone()).collect::<HashSet<_>>() != expected.iter().cloned().collect::<HashSet<_>>() {
            findings.push(format!("searching '{word}' lit {:?}, the blocks that carry it are {expected:?}", hits));
        }
        crate::hud_dock::set_search("");
    }
    // The scene leaves nothing behind.
    for n in 0..count {
        crate::hud_dock::forget(&window_id(n));
    }
    crate::hud_dock::begin_frame();
    crate::hud_dock::begin_frame();
    crate::hud_dock::publish_obstacles(None, Vec::new());
    findings
}

/// The right-click menu of block `n`: it opens with its six entries, and each entry does what
/// it says (copy, pin and unpin, maximise and restore, both exports, close).
fn check_block_menu(stage: &mut Stage, scene: &mut dyn FnMut(&egui::Context), n: usize, content: &str, bounds: Rect) -> Vec<String> {
    use crate::block_actions::BlockAction;
    let mut findings = Vec::new();
    let id = window_id(n);
    let exports = std::env::temp_dir().join(format!("jarvis_verifier_exports_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&exports);
    crate::block_actions::set_export_dir(Some(exports.clone()));
    // Opens the menu on the block's title row and clicks the entry called `label`.
    fn choose(stage: &mut Stage, scene: &mut dyn FnMut(&egui::Context), id: &str, label: &str) -> Result<(), String> {
        let home = crate::hud_dock::block_rect(id).ok_or("the block is gone")?;
        let menu = stage.click(pos2(home.left() + 90.0, home.top() + 18.0), PointerButton::Secondary, scene);
        let entry = menu.find_exact(label).into_iter().next().ok_or_else(|| format!("the menu has no entry '{label}' (it shows: {})", crate::crash_guard::clip(&menu.all_text().replace('\n', " | "), 200)))?;
        stage.click(entry.center(), PointerButton::Primary, scene);
        stage.frames(6, scene);
        Ok(())
    }
    // The six entries are all there.
    match crate::hud_dock::block_rect(&id) {
        Some(home) => {
            let menu = stage.click(pos2(home.left() + 90.0, home.top() + 18.0), PointerButton::Secondary, scene);
            for action in BlockAction::ALL {
                if menu.find_exact(action.label(false, false)).is_empty() {
                    findings.push(format!("the block menu lacks '{}'", action.label(false, false)));
                }
            }
            findings.extend(clipped_text(&menu));
            // A click beside the menu closes it again.
            stage.click(pos2(bounds.right() - 4.0, bounds.bottom() - 4.0), PointerButton::Primary, scene);
        }
        None => findings.push("the first block was never drawn".to_string()),
    }
    let mut step = |stage: &mut Stage, label: &str, verdict: &dyn Fn() -> Option<String>| match choose(stage, scene, &id, label) {
        Ok(()) => findings.extend(verdict()),
        Err(why) => findings.push(format!("{label}: {why}")),
    };
    step(stage, "COPY CONTENT", &|| (crate::block_actions::last_copied().as_deref() != Some(content)).then(|| "COPY CONTENT did not copy the block's text".to_string()));
    step(stage, "PIN BLOCK", &|| (!crate::hud_dock::is_pinned(&id)).then(|| "PIN BLOCK did not pin the block".to_string()));
    step(stage, "UNPIN BLOCK", &|| crate::hud_dock::is_pinned(&id).then(|| "UNPIN BLOCK left the block pinned".to_string()));
    let home = crate::hud_dock::block_rect(&id);
    step(stage, "MAXIMIZE", &|| {
        let now = crate::hud_dock::block_rect(&id)?;
        ((now.width() - bounds.shrink(8.0).width()).abs() > 2.0 || (now.height() - bounds.shrink(8.0).height()).abs() > 2.0).then(|| format!("MAXIMIZE made the block {now:?}, the board is {:?}", bounds.shrink(8.0)))
    });
    step(stage, "RESTORE SIZE", &|| {
        let (now, home) = (crate::hud_dock::block_rect(&id)?, home?);
        ((now.min - home.min).length() > 1.0 || (now.size() - home.size()).length() > 1.0).then(|| format!("RESTORE SIZE put the block at {now:?}, it was at {home:?}"))
    });
    let exported = |extension: &str| std::fs::read_dir(&exports).map(|dir| dir.flatten().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(extension)).count()).unwrap_or(0);
    step(stage, "EXPORT TO MARKDOWN", &|| (exported("md") != 1).then(|| format!("EXPORT TO MARKDOWN wrote {} Markdown file(s)", exported("md"))));
    step(stage, "EXPORT TO JSON", &|| (exported("json") != 1).then(|| format!("EXPORT TO JSON wrote {} JSON file(s)", exported("json"))));
    step(stage, "CLOSE", &|| crate::hud_dock::is_open(&id).then(|| "CLOSE left the block open".to_string()));
    crate::block_actions::set_export_dir(None);
    let _ = std::fs::remove_dir_all(&exports);
    findings
}

/// Check 7 on the core sphere, through its real painter: it holds still under the pointer,
/// turns again when the pointer leaves, and a drag to the right turns its front to the right.
pub fn check_core(size: Vec2) -> Vec<String> {
    let mut findings = Vec::new();
    let mut stage = Stage::new(size);
    let area = crate::gui::board_area(stage.screen(), 0.0, 0.0);
    let centre = area.center();
    let mut core = crate::core_sphere_3d::CoreSphereView::default();
    let show = |ctx: &egui::Context, core: &mut crate::core_sphere_3d::CoreSphereView| {
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| core.show(ui, area, 0.2));
    };
    let advance = |stage: &mut Stage, core: &mut crate::core_sphere_3d::CoreSphereView, frames: usize| {
        let mut frame = Frame::default();
        for _ in 0..frames {
            stage.wait(0.02);
            frame = stage.frame(&mut |ctx| show(ctx, core));
        }
        frame
    };
    // Away from the pointer it turns.
    let frame = advance(&mut stage, &mut core, 4);
    let start = core.orientation().0;
    advance(&mut stage, &mut core, 6);
    if (core.orientation().0 - start).abs() < 1e-4 {
        findings.push("the core sphere does not rotate on its own".to_string());
    }
    findings.extend(clipped_text(&frame));
    findings.extend(pictographs(&frame));
    // Under the pointer it holds still.
    stage.move_pointer(centre);
    advance(&mut stage, &mut core, 20);
    let held = core.orientation().0;
    advance(&mut stage, &mut core, 10);
    if (core.orientation().0 - held).abs() > 1e-3 {
        findings.push(format!("the core sphere keeps rotating under the pointer ({} rad in 10 frames)", core.orientation().0 - held));
    }
    // A drag to the right turns the front face to the right, a drag up tilts it up.
    let front = |core: &crate::core_sphere_3d::CoreSphereView| core.project([0.0, 0.0, -1.0], centre, 200.0).0;
    let before = front(&core);
    stage.drag(centre, vec2(60.0, 0.0), 6, &mut |ctx| show(ctx, &mut core));
    let after = front(&core);
    if after.x <= before.x + 2.0 {
        findings.push(format!("dragging the core sphere right moved its front from x {} to x {}", before.x, after.x));
    }
    let before = front(&core);
    stage.drag(centre, vec2(0.0, -50.0), 6, &mut |ctx| show(ctx, &mut core));
    if front(&core).y >= before.y - 2.0 {
        findings.push("dragging the core sphere up did not tilt it up".to_string());
    }
    findings
}

/// Check 3 on the orb, through the emblem's own drag: picked up at its centre and dropped
/// somewhere else, it ends exactly under the pointer, having glided there without a jump.
pub fn check_orb_drag(screen: Rect, pass: usize) -> Vec<String> {
    let mut findings = Vec::new();
    let mut orb = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
    let dt = 1.0 / 120.0;
    for _ in 0..240 {
        orb.step(dt, 0.0, false);
        orb.glide(screen, dt);
    }
    let start = orb.frame(screen).centre;
    let delta = [vec2(-310.0, 140.0), vec2(260.0, -180.0), vec2(-120.0, -220.0), vec2(340.0, 90.0), vec2(75.0, 200.0)][pass % 5];
    let delta = vec2(delta.x.clamp(-screen.width() * 0.3, screen.width() * 0.3), delta.y.clamp(-screen.height() * 0.3, screen.height() * 0.3));
    let mut path = vec![start];
    // The pointer moves a fifteenth of the way each frame; the emblem is under it all the way.
    for n in 1..=15 {
        let at = start + delta * (n as f32 / 15.0);
        orb.drag_to(vec2((at.x - screen.left()) / screen.width(), (at.y - screen.top()) / screen.height()));
        orb.step(dt, 0.0, false);
        orb.glide(screen, dt);
        let now = orb.frame(screen).centre;
        if (now - at).length() > 1.0 {
            findings.push(format!("the orb lags the pointer while dragged: pointer {at:?}, orb {now:?}"));
            break;
        }
        path.push(now);
    }
    for _ in 0..120 {
        orb.step(dt, 0.0, false);
        orb.glide(screen, dt);
    }
    let end = orb.frame(screen).centre;
    if (end - (start + delta)).length() > 1.0 {
        findings.push(format!("the orb was dragged by {delta:?} from {start:?} and rests at {end:?}"));
    }
    let hop = delta.length() / 15.0 + 1.0;
    if path.windows(2).any(|w| (w[1] - w[0]).length() > hop * 1.05) {
        findings.push("the orb jumped during the drag".to_string());
    }
    findings
}

/// A voice move ("move the orb to the top right"): the emblem glides to its new home with no
/// single-frame jump and comes to rest there.
pub fn check_orb_glide(screen: Rect, pass: usize) -> Vec<String> {
    let mut findings = Vec::new();
    let mut orb = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
    let fps = [180.0f32, 60.0, 144.0, 120.0, 90.0][pass % 5];
    let dt = 1.0 / fps;
    for _ in 0..(2.0 * fps) as usize {
        orb.step(dt, 0.0, false);
        orb.glide(screen, dt);
    }
    let homes = [vec2(0.86, 0.2), vec2(0.14, 0.2), vec2(0.14, 0.8), vec2(0.86, 0.8), vec2(0.5, 0.5)];
    let home = homes[pass % 5];
    let asked = pos2(screen.left() + home.x * screen.width(), screen.top() + home.y * screen.height());
    let from = orb.frame(screen).centre;
    orb.set_custom_home(Some(home));
    let mut path: Vec<Pos2> = Vec::new();
    for _ in 0..(4.0 * fps) as usize {
        orb.step(dt, 0.0, false);
        orb.glide(screen, dt);
        path.push(orb.frame(screen).centre);
    }
    // Where it came to rest: the spot Sir named, or as near to it as the whole emblem fits on
    // the screen (a narrow monitor keeps it from hanging over the edge).
    let rest = path.last().copied().unwrap_or(from);
    let distance: Vec<f32> = path.iter().map(|p| (*p - rest).length()).collect();
    let whole = (from - rest).length().max(1.0);
    if let Some((frame, by)) = jumps(&distance, whole * smooth_step(dt) + 0.5).first() {
        findings.push(format!("the orb's glide to {home:?} jumps by {by:.1} px in frame {frame} at {fps} FPS"));
    }
    if !monotonic(&distance, false) {
        findings.push(format!("the orb's glide to {home:?} is not monotonic (it overshoots or turns back)"));
    }
    let settled = path.len() >= 30 && path[path.len() - 30..].iter().all(|p| (*p - rest).length() < 0.5);
    if !settled {
        findings.push(format!("the orb has not come to rest four seconds after it was sent to {home:?}"));
    }
    let radius = orb.frame(screen).radius;
    if (rest - asked).length() > radius + 8.0 {
        findings.push(format!("the orb rests {:.1} px from the spot it was sent to ({home:?}); its radius is {radius:.0} px", (rest - asked).length()));
    }
    if (rest - from).length() < 20.0 && (asked - from).length() > 60.0 {
        findings.push(format!("the orb did not move when it was sent to {home:?}"));
    }
    findings
}

/// The radial voice ticks of the 2D orb move with Jarvis' own voice only: Sir's microphone
/// level never reaches the emblem. `voice_of(state, jarvis_level, mic_level)` is the HUD's own
/// mapping (`gui::emblem_voice_of`).
pub fn check_voice_ticks(voice_of: &dyn Fn(&str, f32, f32) -> f32) -> Vec<String> {
    let mut findings = Vec::new();
    for (state, jarvis, mic) in [("listening", 0.0f32, 0.95f32), ("idle", 0.0, 0.85), ("thinking", 0.0, 1.0), ("speaking", 0.0, 0.9)] {
        let voice = voice_of(state, jarvis, mic);
        if emblem::tach_ticks(voice) != emblem::tach_ticks(0.0) {
            findings.push(format!("the voice ticks move while Jarvis is silent ({state}, microphone at {mic}): voice {voice}"));
        }
    }
    let speaking = voice_of("speaking", 0.6, 0.0);
    if emblem::tach_ticks(speaking) == emblem::tach_ticks(0.0) {
        findings.push("the voice ticks stay at rest while Jarvis speaks".to_string());
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime")
    }

    fn fail(what: &str, findings: Vec<String>) {
        assert!(findings.is_empty(), "{what}:\n  {}", findings.join("\n  "));
    }

    #[test]
    fn text_cut_by_a_clip_rectangle_is_found_and_a_scrolling_region_is_not() {
        let mut stage = Stage::new(vec2(800.0, 600.0));
        let long = "The reactor sustains its output without drift. ".repeat(40);
        // A label clipped by a small frame with nothing to scroll: a finding.
        let cut = stage.frames(3, &mut |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let small = Rect::from_min_size(pos2(40.0, 40.0), vec2(300.0, 60.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(small));
                child.set_clip_rect(small);
                child.add(egui::Label::new(long.as_str()).wrap());
            });
        });
        assert_eq!(clipped_text(&cut).len(), 1, "{:?}", clipped_text(&cut));
        // The same text in a scroll area: its bar is there (invisible at rest), no finding.
        let scrolled = stage.frames(3, &mut |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().id_salt("verifier_scroll").max_height(80.0).show(ui, |ui| ui.add(egui::Label::new(long.as_str()).wrap()));
            });
        });
        fail("a scroll area", clipped_text(&scrolled));
        // Text that fits: nothing; a pictograph: found.
        let plain = stage.frames(2, &mut |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Reactor online.");
                ui.label("Done \u{2705}");
            });
        });
        fail("text that fits", clipped_text(&plain));
        assert_eq!(pictographs(&plain).len(), 1);
        assert_eq!(plain.find_text("reactor online").len(), 1);
    }

    #[test]
    fn overlapping_rectangles_and_off_theme_colours_are_found() {
        let a = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let beside = Rect::from_min_size(pos2(100.0, 0.0), vec2(100.0, 100.0));
        let across = Rect::from_min_size(pos2(60.0, 60.0), vec2(100.0, 100.0));
        assert!(overlaps(&[("a".into(), a), ("b".into(), beside)]).is_empty(), "touching edges are no overlap");
        assert_eq!(overlaps(&[("a".into(), a), ("b".into(), beside), ("c".into(), across)]).len(), 2);
        // A cyan outline under a red theme is off theme; the theme's own, white ink, a dim
        // grey-black and the status red are not.
        let red = theme_engine::resolve("crimson_overdrive");
        let mut frame = Frame { screen: Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), ..Frame::default() };
        let clip = frame.screen;
        for colour in [red.border(), red.glow(), Color32::WHITE, Color32::from_gray(30), Color32::from_rgb(STATUS_RED[0], STATUS_RED[1], STATUS_RED[2]), red.border().gamma_multiply(0.4)] {
            frame.strokes.push(StrokeItem { clip, rect: a, colour, width: 1.0 });
        }
        fail("the theme's own colours", off_theme(&frame, "Crimson Overdrive", &red));
        frame.strokes.push(StrokeItem { clip, rect: a, colour: Color32::from_rgb(0x2B, 0x7A, 0x9E), width: 1.0 });
        frame.mesh_colours.push((a, Color32::from_rgb(0x3D, 0xF5, 0x9B)));
        let found = off_theme(&frame, "Crimson Overdrive", &red);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("#2B7A9E") && found[1].contains("#3DF59B"), "{found:?}");
        // The matrix has eleven themes.
        let list = themes();
        assert_eq!(list.len(), 11, "{list:?}");
        assert!(list.iter().any(|(name, _)| name == theme_engine::DEFAULT_PRESET));
        let found = jumps(&[0.0, 0.1, 0.9, 1.0], 0.5);
        assert!(found.len() == 1 && found[0].0 == 2 && (found[0].1 - 0.8).abs() < 1e-5, "{found:?}");
        assert!(monotonic(&[0.0, 0.2, 0.2, 0.9], true) && !monotonic(&[0.0, 0.3, 0.2], true));
    }

    #[test]
    fn the_status_pill_keeps_its_own_colours_in_every_theme() {
        let mut frame = Frame { screen: Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)), ..Frame::default() };
        let clip = frame.screen;
        let at = Rect::from_min_size(pos2(10.0, 10.0), vec2(90.0, 20.0));
        for word in crate::gui::PILL_WORDS {
            for (i, colour) in crate::gui::pill_colours(word).into_iter().enumerate() {
                // Steady, and half-way through the 33 ms cross-fade.
                for colour in [colour, colour.gamma_multiply(0.5), colour.gamma_multiply(0.2)] {
                    if i == 3 {
                        frame.strokes.push(StrokeItem { clip, rect: at, colour, width: 1.0 });
                    } else {
                        frame.mesh_colours.push((at, colour));
                    }
                }
            }
        }
        for (name, palette) in theme_engine::PRESETS {
            fail(&format!("the pill under {name}"), off_theme(&frame, name, &palette));
        }
        // Only the pill's own colours: another green is still off a red theme.
        frame.strokes.push(StrokeItem { clip, rect: at, colour: Color32::from_rgb(0x20, 0xC0, 0x40), width: 1.0 });
        let found = off_theme(&frame, "Crimson Overdrive", &theme_engine::resolve("crimson_overdrive"));
        assert!(found.len() == 1 && found[0].contains("#20C040"), "{found:?}");
        // Every word the pill can show has its colours here.
        for (state, muted, asleep) in [("idle", false, false), ("thinking", false, false), ("speaking", false, false), ("idle", true, false), ("idle", false, true)] {
            assert!(crate::gui::PILL_WORDS.contains(&crate::gui::pill_word(state, muted, asleep)));
        }
    }

    #[test]
    fn the_emblem_moves_without_jumps_on_every_screen_in_five_passes() {
        for (name, size) in SCREENS {
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(size[0], size[1]));
            for pass in 0..5 {
                fail(&format!("emblem, {name}, pass {}", pass + 1), check_emblem(screen, pass));
                fail(&format!("orb drag, {name}, pass {}", pass + 1), check_orb_drag(screen, pass));
                fail(&format!("orb glide, {name}, pass {}", pass + 1), check_orb_glide(screen, pass));
            }
        }
        fail("voice ticks", check_voice_ticks(&crate::gui::emblem_voice_of));
    }

    #[test]
    fn the_board_keeps_its_blocks_apart_whole_and_draggable_on_every_screen() {
        let _board = crate::hud_board::test_lock();
        let _dock = crate::hud_dock::test_lock();
        let rt = runtime();
        for (name, size) in SCREENS {
            for (pass, count) in BLOCK_COUNTS.iter().enumerate() {
                fail(&format!("board, {name}, {count} blocks, pass {}", pass + 1), check_board(vec2(size[0], size[1]), *count, pass, rt.handle()));
            }
        }
    }

    #[test]
    fn every_theme_colours_every_border_of_the_board() {
        let _board = crate::hud_board::test_lock();
        let _dock = crate::hud_dock::test_lock();
        let _theme = crate::theme_engine::test_lock();
        let rt = runtime();
        for pass in 0..2 {
            fail(&format!("board themes, pass {}", pass + 1), check_board_themes(vec2(1600.0, 900.0), pass, rt.handle()));
        }
    }

    #[test]
    fn block_windows_never_overlap_follow_a_drag_and_answer_their_menu() {
        let _board = crate::hud_board::test_lock();
        let _dock = crate::hud_dock::test_lock();
        for (name, size) in SCREENS {
            for (pass, count) in BLOCK_COUNTS.iter().enumerate() {
                fail(&format!("windows, {name}, {count} blocks, pass {}", pass + 1), check_windows(vec2(size[0], size[1]), *count, pass, *count == 2));
            }
        }
    }

    #[test]
    fn the_core_sphere_holds_under_the_pointer_and_turns_with_a_drag() {
        fail("core sphere", check_core(vec2(1600.0, 900.0)));
    }
}
