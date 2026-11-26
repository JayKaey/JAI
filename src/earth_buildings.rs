//! The houses of a town on the Earth's flat map: the building outlines and heights of
//! OpenStreetMap, read from OpenFreeMap's vector tiles (one tile at a time, kept in the Earth's
//! cache), raised from the map as block models of light once the view is down at street level.
//! They stand in the camera's perspective (`FlatView::lift`): straight down a house shows its
//! roof and the walls turned to the middle of the view; leaned back (3D) the camera looks at
//! them from the side, and every side of a house can be seen by turning it.
//! The same tiles give the hologram its ground from a town's outline down: the streets, the
//! rails, the shores and the streams, and the streets' names along them.

use crate::earth::{self as model, FlatView};
use crate::holo::lit;
use crate::holo_motion::{ease_out_cubic, phase};
use eframe::egui::{pos2, vec2, Align2, Color32, Id, Painter, Pos2, Rect, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tracing::{debug, info};

/// The level of the vector tiles that carry every house.
const Z: u8 = 14;
/// The view's zoom from which they stand, fading in over `FADE` levels.
const SHOW_FROM: f64 = 15.8;
const FADE: f64 = 0.4;
/// A tile that failed is asked for again after this.
const RETRY: Duration = Duration::from_secs(90);
/// Tiles kept in memory; beyond it the ones out of view go.
const KEEP: usize = 24;
/// Houses drawn in one frame (the nearest the middle), and how many still get upright edges.
const MAX_DRAWN: usize = 3_500;
const EDGES_UP_TO: usize = 1_500;
/// A house smaller than this on the screen, across and high (px), is its roof's edge alone.
const SMALL_PX: f32 = 5.0;
/// Below this (px) a house is its walls and its roof's edge: no line along its foot, no glass on
/// its roof, no hipped faces (too small to be read, and most of a city's mesh).
const PLAIN_PX: f32 = 16.0;
/// Where the walls' light comes from on the ground (north up).
const SUN: Vec2 = Vec2::new(-0.6, 0.8);
/// A storey's height: its lines on the walls are a scale of the house's height.
const STOREY_M: f32 = 3.2;
/// A window bay's width: with the storeys, the grid of a wall's rooms.
const BAY_M: f32 = 3.0;
/// From this height a house's roof says how tall it is.
const TALL_M: f32 = 35.0;
/// Houses in view up to which the walls get their storey lines, and labels drawn at most.
const STOREYS_UP_TO: usize = 600;
const MAX_LABELS: usize = 140;
/// Leaned back, the houses behind the middle of the view fall back into the deep, to a ghost at
/// `HOUSES_FADE` along the view (in camera distances: 1 in the middle) and gone at `HOUSES_TO`:
/// seen low over a town, its far half was a tangle of outlines as bright as the near one
/// (2026-11-26). Roofs are labelled up to `LABELS_TO`.
const HOUSES_FADE: f64 = 1.5;
const HOUSES_TO: f64 = 2.4;
const LABELS_TO: f64 = 1.25;

/// How much of a house's light is left `depth` along the view (see `HOUSES_FADE`).
fn behind(depth: f64) -> f32 {
    (1.0 - 0.8 * model::smoothstep(1.0, HOUSES_FADE, depth) - 0.2 * model::smoothstep(HOUSES_FADE, HOUSES_TO, depth)) as f32
}
/// Window panes drawn at most in a frame (a wall's rooms, up close).
const PANES_UP_TO: usize = 30_000;
/// A tower's height scale on its front corner: a tick every so many metres, a long one every fifth.
const TICK_M: f32 = 10.0;
/// How long a house takes to rise, how long the wave of them takes from the middle of the view
/// to its edge, and how much each one's own moment in the wave varies.
const RISE_SECS: f32 = 0.9;
const WAVE_SECS: f32 = 0.8;
const JITTER_SECS: f32 = 0.2;
/// The view's zoom from which the house numbers are read.
const NUMBERS_FROM: f64 = 17.0;
/// The view's zoom from which the hologram shows the ground (a big town's outline), fading in
/// over one level, and the streets' names drawn at most.
const GROUND_FROM: f64 = 10.0;
const MAX_NAMES: usize = 40;
/// The tile set's description: it names the address of this week's tiles.
const TILE_SET: &str = "https://tiles.openfreemap.org/planet";
pub const CREDIT: &str = "Streets and buildings: OpenFreeMap \u{a9} OpenMapTiles, data from OpenStreetMap";

/// A house as it is drawn.
#[derive(Debug, Clone, PartialEq)]
struct Building {
    /// The outline in Web Mercator (0-1), clockwise on the screen, not closed.
    ring: Vec<[f64; 2]>,
    /// The edges (from each point to the next) that are a tile's cut and no wall; empty: none.
    seam: Vec<bool>,
    /// The roof as triangles of the outline's points.
    roof: Vec<[u16; 3]>,
    /// The middle of the outline and how far it reaches from there (Web Mercator).
    mid: [f64; 2],
    reach: f64,
    /// Metres above the ground: where it starts (a passage under it) and its top.
    base: f32,
    top: f32,
    /// Its house number, read off its roof up close.
    number: Option<String>,
    /// Its roof, when it is a small house.
    hip: Option<Hip>,
}

/// A small house's roof. The map knows no roof shapes, so this is a guess by the house's
/// size: hipped, the ridge along its longer side (a point over a square one).
#[derive(Debug, Clone, PartialEq)]
struct Hip {
    /// The ridge's ends (Web Mercator): over the short side before the long side `long`, and
    /// over the one after it.
    ridge: [[f64; 2]; 2],
    /// The side (from this corner to the next) the ridge runs along.
    long: usize,
    /// How far the ridge stands above the eaves, in metres.
    rise: f32,
}

impl Hip {
    fn of(ring: &[[f64; 2]], seam: &[bool], base: f32, top: f32) -> Option<Self> {
        if ring.len() != 4 || !seam.is_empty() || base > 0.0 || top > 14.0 {
            return None;
        }
        // Metres to a unit of the map at the house.
        let m = 40_075_017.0 * model::lat_of_merc_y(ring[0][1]).to_radians().cos();
        let side = |i: usize| [(ring[(i + 1) % 4][0] - ring[i % 4][0]) * m, (ring[(i + 1) % 4][1] - ring[i % 4][1]) * m];
        let len = |e: [f64; 2]| e[0].hypot(e[1]);
        let squared = (0..4).all(|i| {
            let (a, b) = (side(i), side(i + 1));
            ((a[0] * b[0] + a[1] * b[1]) / (len(a) * len(b)).max(1e-9)).abs() < 0.3
        });
        let long = if len(side(0)) >= len(side(1)) { 0 } else { 1 };
        let (long_m, short_m) = (len(side(long)), len(side(long + 1)));
        // A house: square-cornered and of a house's size (a hall keeps its flat roof).
        if !squared || short_m < 4.0 || long_m > 30.0 || long_m * short_m > 450.0 {
            return None;
        }
        let middle = |i: usize| [(ring[i % 4][0] + ring[(i + 1) % 4][0]) / 2.0, (ring[i % 4][1] + ring[(i + 1) % 4][1]) / 2.0];
        let (near, far) = (middle(long + 3), middle(long + 1));
        // Half a short side in from each end (the hips run at 45 degrees on the plan).
        let inset = (short_m / 2.0 / long_m).min(0.5);
        let along = |t: f64| [near[0] + (far[0] - near[0]) * t, near[1] + (far[1] - near[1]) * t];
        // A pitch of thirty degrees, and walls of a storey at least under it.
        let rise = (short_m / 2.0 * 0.58).min(4.0).min((top - base) as f64 - 2.2) as f32;
        (rise >= 1.0).then(|| Self { ridge: [along(inset), along(1.0 - inset)], long, rise })
    }
}

impl Building {
    fn new(ring: Vec<[f64; 2]>, seam: Vec<bool>, base: f32, top: f32) -> Self {
        let (mid, reach) = span(&ring);
        Self { roof: triangulate(&ring), reach, mid, hip: Hip::of(&ring, &seam, base, top), ring, seam, base, top, number: None }
    }

    /// What its roof says: its number, and how tall it is when it is a tower.
    fn label(&self, numbers: bool) -> Option<String> {
        match (self.number.as_deref().filter(|_| numbers), self.top >= TALL_M) {
            (Some(number), true) => Some(format!("{number} \u{b7} {:.0} m", self.top)),
            (Some(number), false) => Some(number.to_string()),
            (None, true) => Some(format!("{:.0} m", self.top)),
            (None, false) => None,
        }
    }

    /// Whether a point (Web Mercator) lies on its ground.
    fn holds(&self, p: [f64; 2]) -> bool {
        let n = self.ring.len();
        (p[0] - self.mid[0]).hypot(p[1] - self.mid[1]) <= self.reach
            && (0..n).map(|i| (self.ring[i], self.ring[(i + 1) % n])).filter(|(a, b)| (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0])).count() % 2 == 1
    }
}

/// Twice the area of the triangle a-b-c: positive when it runs clockwise on the screen (y down).
fn side(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// The outline (clockwise on the screen) cut into triangles, ear by ear.
fn triangulate(ring: &[[f64; 2]]) -> Vec<[u16; 3]> {
    let at = |i: u16| ring[i as usize];
    let mut left: Vec<u16> = (0..ring.len() as u16).collect();
    let mut out = Vec::with_capacity(ring.len().saturating_sub(2));
    while left.len() > 3 {
        let n = left.len();
        let corner = |i: usize| (at(left[(i + n - 1) % n]), at(left[i]), at(left[(i + 1) % n]));
        // A point on the line between its neighbours cuts nothing off: it just goes.
        if let Some(i) = (0..n).find(|&i| matches!(corner(i), (a, b, c) if side(a, b, c) == 0.0)) {
            left.remove(i);
            continue;
        }
        // An ear: a corner that turns the outline's way with no other point in it or on its rim
        // (a point on the rim is the outline coming back to touch it: cutting there cuts outside).
        let ear = (0..n).find(|&i| {
            let (a, b, c) = corner(i);
            side(a, b, c) > 0.0 && !left.iter().map(|&v| at(v)).any(|p| p != a && p != b && p != c && side(a, b, p) >= 0.0 && side(b, c, p) >= 0.0 && side(c, a, p) >= 0.0)
        });
        // (An outline that crosses itself runs out of ears: the rest goes as it comes.)
        let i = ear.unwrap_or(0);
        out.push([left[(i + n - 1) % n], left[i], left[(i + 1) % n]]);
        left.remove(i);
    }
    if let [a, b, c] = left[..] {
        out.push([a, b, c]);
    }
    out
}

// --------------------------------------------------------------------- the vector tile ----

/// A field of a protocol buffers message.
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
    Fixed64(u64),
}

/// The fields of a message, one after another as (number, value); a message that breaks off
/// ends there.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.0.split_first()?;
            self.0 = rest;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte < 0x80 {
                return Some(value);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(taken)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u64, Wire<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            2 => {
                let n = self.varint()? as usize;
                Wire::Bytes(self.take(n)?)
            }
            5 => Wire::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().ok()?)),
            _ => return None,
        };
        Some((key >> 3, value))
    }
}

/// A number written zigzag (small negatives stay short).
fn zigzag(n: u64) -> i64 {
    (n >> 1) as i64 ^ -((n & 1) as i64)
}

/// A tile's value as a number (a yes as 1); text is none.
fn number(value: &[u8]) -> Option<f32> {
    Fields(value).find_map(|field| {
        Some(match field {
            (2, Wire::Fixed32(bits)) => f32::from_bits(bits),
            (3, Wire::Fixed64(bits)) => f64::from_bits(bits) as f32,
            (4 | 5 | 7, Wire::Varint(n)) => n as i64 as f32,
            (6, Wire::Varint(n)) => zigzag(n) as f32,
            _ => return None,
        })
    })
}

/// A tile's value as text.
fn text(value: &[u8]) -> Option<&str> {
    Fields(value).find_map(|field| match field {
        (1, Wire::Bytes(said)) => std::str::from_utf8(said).ok(),
        _ => None,
    })
}

/// The paths of a geometry, in the tile's own units, each with whether it was closed (a
/// polygon's ring) or left open (a line).
fn paths(geometry: &[u8]) -> Vec<(Vec<[i32; 2]>, bool)> {
    let mut words = Fields(geometry);
    let (mut out, mut path, mut at) = (Vec::new(), Vec::new(), [0i32; 2]);
    while let Some(command) = words.varint() {
        match command & 7 {
            // Move to (a new path starts: what stands so far was a line) and line to.
            id @ (1 | 2) => {
                for _ in 0..command >> 3 {
                    let (Some(dx), Some(dy)) = (words.varint(), words.varint()) else { return out };
                    at = [at[0].wrapping_add(zigzag(dx) as i32), at[1].wrapping_add(zigzag(dy) as i32)];
                    if id == 1 {
                        let line = std::mem::take(&mut path);
                        if line.len() > 1 {
                            out.push((line, false));
                        }
                    }
                    path.push(at);
                }
            }
            7 => out.push((std::mem::take(&mut path), true)),
            _ => return out,
        }
    }
    if path.len() > 1 {
        out.push((path, false));
    }
    out
}

/// What a line on a town's ground is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Shore,
    Stream,
    /// A street by its rank: 0 a motorway, 4 a residential street, 6 a path.
    Road(u8),
    Rail,
}

/// A line of a town's ground (a street, a rail, a shore), or a street's name with the line it
/// runs along.
#[derive(Debug, Clone, PartialEq)]
struct Way {
    kind: Kind,
    /// Web Mercator (0-1).
    points: Vec<[f64; 2]>,
    mid: [f64; 2],
    reach: f64,
    name: Option<String>,
}

/// The middle of some points and how far they reach from there.
fn span(points: &[[f64; 2]]) -> ([f64; 2], f64) {
    let (lo, hi) = points.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    (mid, (hi[0] - mid[0]).hypot(hi[1] - mid[1]))
}

/// The part of the segment a-b inside the box from `lo` to `hi` (a tile's square, the map on
/// the screen), if any.
fn clip(a: [f64; 2], b: [f64; 2], lo: [f64; 2], hi: [f64; 2]) -> Option<([f64; 2], [f64; 2])> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for k in 0..2 {
        for (p, q) in [(-d[k], a[k] - lo[k]), (d[k], hi[k] - a[k])] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
            } else if p < 0.0 {
                t0 = t0.max(q / p);
            } else {
                t1 = t1.min(q / p);
            }
        }
    }
    // (A point cut at the edge sits on it exactly: the next tile's part starts there.)
    let on = |v: f64, k: usize| if (v - lo[k]).abs() < 1e-6 { lo[k] } else if (v - hi[k]).abs() < 1e-6 { hi[k] } else { v };
    let at = |t: f64| if t == 0.0 { a } else if t == 1.0 { b } else { [on(a[0] + d[0] * t, 0), on(a[1] + d[1] * t, 1)] };
    (t0 < t1).then(|| (at(t0), at(t1)))
}

/// The lines of a tile's ground and the names along its streets (the OpenMapTiles layers
/// "water", "waterway", "transportation" and "transportation_name"), at any level. A tunnel is
/// left out, and each tile keeps what lies in its own square, cut at its edge: a line over the
/// seam is drawn once (light adds up), and where the tiler cut a lake no shore is drawn.
fn ground(tile_bytes: &[u8], (z, tx, ty): (u8, u32, u32)) -> (Vec<Way>, Vec<Way>) {
    let n_tiles = (1u32 << z) as f64;
    let (mut ways, mut names) = (Vec::new(), Vec::new());
    // (The layer, and its features' kind: 3 a polygon, 2 a line.)
    let sought: [(&[u8], u64); 4] = [(b"water", 3), (b"waterway", 2), (b"transportation", 2), (b"transportation_name", 2)];
    for (layer, shape) in sought {
        for (features, keys, values, extent) in layers(tile_bytes, layer) {
            let size = extent as f64;
            let merc = |p: [f64; 2]| [(tx as f64 + p[0] / size) / n_tiles, (ty as f64 + p[1] / size) / n_tiles];
            for (_, geometry, said) in features.iter().map(|f| feature(f, &keys, &values)).filter(|f| f.0 == shape) {
                let word = |name: &[u8]| said.iter().find(|(key, _)| *key == name).and_then(|(_, value)| text(value));
                let kind = match layer {
                    b"water" => Some(Kind::Shore),
                    b"waterway" => Some(Kind::Stream),
                    _ => word(b"class").and_then(|class| match class {
                        "motorway" | "trunk" => Some(Kind::Road(0)),
                        "primary" => Some(Kind::Road(1)),
                        "secondary" => Some(Kind::Road(2)),
                        "tertiary" => Some(Kind::Road(3)),
                        "minor" | "busway" => Some(Kind::Road(4)),
                        "service" | "track" | "raceway" => Some(Kind::Road(5)),
                        "path" => Some(Kind::Road(6)),
                        "rail" | "transit" => Some(Kind::Rail),
                        _ => None,
                    }),
                };
                let name = word(b"name").filter(|_| layer == b"transportation_name").map(|n| n.chars().take(40).collect::<String>());
                let Some(kind) = kind.filter(|_| word(b"brunnel") != Some("tunnel") && (name.is_some() || layer != b"transportation_name")) else { continue };
                let list = if name.is_some() { &mut names } else { &mut ways };
                for (mut path, closed) in paths(geometry) {
                    path.extend(path.first().copied().filter(|_| closed));
                    let (mut run, mut end): (Vec<[f64; 2]>, Option<[f64; 2]>) = (Vec::new(), None);
                    let mut keep = |run: &mut Vec<[f64; 2]>| {
                        let points = std::mem::take(run);
                        if points.len() > 1 {
                            let (mid, reach) = span(&points);
                            list.push(Way { kind, points, mid, reach, name: name.clone() });
                        }
                    };
                    for pair in path.windows(2) {
                        // (A ring's stretch along the square's edge is where the tiler cut it.)
                        let piece = clip(pair[0].map(f64::from), pair[1].map(f64::from), [0.0; 2], [size; 2]).filter(|(a, b)| !closed || !(0..2).any(|k| a[k] == b[k] && (a[k] == 0.0 || a[k] == size)));
                        if let Some((from, to)) = piece {
                            if end != Some(from) {
                                keep(&mut run);
                                run.push(merc(from));
                            }
                            run.push(merc(to));
                        }
                        end = piece.map(|piece| piece.1);
                    }
                    keep(&mut run);
                }
            }
        }
    }
    (ways, names)
}

/// What a tile holds of a town.
#[derive(Default)]
struct Town {
    houses: Vec<Building>,
    ways: Vec<Way>,
    names: Vec<Way>,
    /// When its houses came (they rise then); None: they were there from the start.
    came: Option<Instant>,
}

/// A ring of a tile as a house: None for a courtyard (it runs the other way round), for what is
/// no area, and for a house that lies whole in the tile's margin (its neighbour draws it).
/// `cut`: the lines the tile's houses were cut along (the least and the most of x and of y).
fn house(mut ring: Vec<[i32; 2]>, extent: i32, cut: [[i32; 2]; 2], tile: (u32, u32), base: f32, top: Option<f32>) -> Option<Building> {
    ring.dedup();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    let n = ring.len();
    let doubled: i64 = (0..n).map(|i| (ring[i], ring[(i + 1) % n])).map(|(a, b)| a[0] as i64 * b[1] as i64 - b[0] as i64 * a[1] as i64).sum();
    if !(3..=600).contains(&n) || doubled <= 0 {
        return None;
    }
    let n_tiles = (1u32 << Z) as f64;
    // A house the map gives no height for (most of a small town's) gets one by its ground: a
    // shed, a house of two storeys under a roof, a block, a hall.
    let metre = 40_075_017.0 * model::lat_of_merc_y((tile.1 as f64 + 0.5) / n_tiles).to_radians().cos() / n_tiles / extent as f64;
    let top = top.unwrap_or(match doubled as f64 / 2.0 * metre * metre {
        ground if ground < 30.0 => 3.0,
        ground if ground < 160.0 => 7.5,
        ground if ground < 700.0 => 10.0,
        _ => 8.5,
    });
    let (top, base) = (top.clamp(2.0, 900.0), base.clamp(0.0, (top - 1.0).max(0.0)));
    // An edge along a cutting line, beyond the tile's own square, is no wall of the house.
    let on_cut = |a: [i32; 2], b: [i32; 2], k: usize| a[k] == b[k] && (a[k] < 0 || a[k] > extent) && (a[k] == cut[0][k] || a[k] == cut[1][k]);
    let seam: Vec<bool> = (0..n).map(|i| (ring[i], ring[(i + 1) % n])).map(|(a, b)| on_cut(a, b, 0) || on_cut(a, b, 1)).collect();
    let cut = seam.contains(&true);
    if !cut {
        // Whole here and, in the margin, whole in the neighbour too: it is the tile's its middle is in.
        let middle = |k: usize| (ring.iter().map(|p| p[k]).min().unwrap_or(0) + ring.iter().map(|p| p[k]).max().unwrap_or(0)).div_euclid(2);
        if !(0..extent).contains(&middle(0)) || !(0..extent).contains(&middle(1)) {
            return None;
        }
    }
    let merc = ring.iter().map(|p| [(tile.0 as f64 + p[0] as f64 / extent as f64) / n_tiles, (tile.1 as f64 + p[1] as f64 / extent as f64) / n_tiles]).collect();
    Some(Building::new(merc, if cut { seam } else { Vec::new() }, base, top))
}

/// The layers of a tile called `name`, each as (its features, its keys, its values, its extent).
fn layers<'a>(tile_bytes: &'a [u8], name: &'a [u8]) -> impl Iterator<Item = (Vec<&'a [u8]>, Vec<&'a [u8]>, Vec<&'a [u8]>, i32)> {
    let all = Fields(tile_bytes).filter_map(|field| match field {
        (3, Wire::Bytes(layer)) => Some(layer),
        _ => None,
    });
    all.filter(move |&l| Fields(l).any(|f| matches!(f, (1, Wire::Bytes(n)) if n == name))).map(|layer| {
        let (mut features, mut keys, mut values, mut extent) = (Vec::new(), Vec::new(), Vec::new(), 4096i32);
        for field in Fields(layer) {
            match field {
                (2, Wire::Bytes(feature)) => features.push(feature),
                (3, Wire::Bytes(key)) => keys.push(key),
                (4, Wire::Bytes(value)) => values.push(value),
                (5, Wire::Varint(e)) => extent = (e as i32).max(1),
                _ => {}
            }
        }
        (features, keys, values, extent)
    })
}

/// A feature's kind (1: a point, 3: a polygon), its geometry and its tags as (key, value).
fn feature<'a>(feature: &'a [u8], keys: &[&'a [u8]], values: &[&'a [u8]]) -> (u64, &'a [u8], Vec<(&'a [u8], &'a [u8])>) {
    let (mut tags, mut kind, mut geometry) = (&[][..], 0, &[][..]);
    for field in Fields(feature) {
        match field {
            (2, Wire::Bytes(b)) => tags = b,
            (3, Wire::Varint(k)) => kind = k,
            (4, Wire::Bytes(b)) => geometry = b,
            _ => {}
        }
    }
    let mut tag = Fields(tags);
    let mut said = Vec::new();
    while let (Some(key), Some(value)) = (tag.varint(), tag.varint()) {
        said.extend(keys.get(key as usize).zip(values.get(value as usize)).map(|(k, v)| (*k, *v)));
    }
    (kind, geometry, said)
}

/// The houses of a vector tile (the OpenMapTiles schema). Its "building" layer gives every
/// outer ring with its `render_min_height` and `render_height` (an outline that only frames its
/// own parts, `hide_3d`, is left to them); its "housenumber" layer gives the numbers, each to
/// the house it stands on.
fn houses(tile_bytes: &[u8], tile: (u32, u32)) -> Vec<Building> {
    let mut out = Vec::new();
    let n_tiles = (1u32 << Z) as f64;
    for (features, keys, values, extent) in layers(tile_bytes, b"building") {
        let mut found: Vec<(Vec<[i32; 2]>, f32, Option<f32>)> = Vec::new();
        for (_, geometry, said) in features.iter().map(|f| feature(f, &keys, &values)).filter(|f| f.0 == 3) {
            let figure = |name: &[u8]| said.iter().find(|(key, _)| *key == name).and_then(|(_, value)| number(value));
            if figure(b"hide_3d").is_some_and(|hidden| hidden != 0.0) {
                continue;
            }
            // (Five metres is what the tiles write where the map says nothing: no height.)
            let top = figure(b"render_height").filter(|h| *h > 0.0 && *h != 5.0);
            let base = figure(b"render_min_height").unwrap_or(0.0);
            found.extend(paths(geometry).into_iter().filter(|path| path.1).map(|(ring, _)| (ring, base, top)));
        }
        // Where the tile cut its houses: the outermost lines any of them reaches.
        let points = || found.iter().flat_map(|(ring, ..)| ring.iter());
        let reach = |k: usize| [points().map(|p| p[k]).min().unwrap_or(0), points().map(|p| p[k]).max().unwrap_or(0)];
        let (x, y) = (reach(0), reach(1));
        let cut = [[x[0], y[0]], [x[1], y[1]]];
        out.extend(found.into_iter().filter_map(|(ring, base, top)| house(ring, extent, cut, tile, base, top)));
    }
    // The numbers: each to the first house without one that its point stands on.
    for (features, keys, values, extent) in layers(tile_bytes, b"housenumber") {
        for (_, geometry, said) in features.iter().map(|f| feature(f, &keys, &values)).filter(|f| f.0 == 1) {
            let Some(number) = said.iter().find(|(key, _)| *key == b"housenumber").and_then(|(_, value)| text(value)) else { continue };
            let mut words = Fields(geometry);
            let (Some(_), Some(x), Some(y)) = (words.varint(), words.varint(), words.varint()) else { continue };
            let at = [(tile.0 as f64 + zigzag(x) as f64 / extent as f64) / n_tiles, (tile.1 as f64 + zigzag(y) as f64 / extent as f64) / n_tiles];
            if let Some(house) = out.iter_mut().find(|b| b.number.is_none() && b.holds(at)) {
                house.number = Some(number.chars().take(8).collect());
            }
        }
    }
    out
}

fn cache_file((z, x, y): (u8, u32, u32)) -> PathBuf {
    model::cache_path(&["buildings", &z.to_string(), &format!("{x}_{y}.pbf")])
}

// --------------------------------------------------------------------------------- state ----

enum Tile {
    Loading,
    Ready(Arc<Town>),
    Failed(Instant),
}

struct Store {
    /// By (level, x, y).
    tiles: HashMap<(u8, u32, u32), Tile>,
    /// Where this week's tiles are ("…/{z}/{x}/{y}.pbf"), once read from the tile set.
    address: Option<String>,
}

/// The houses coming into a view: they rise in a wave from the middle of the view (Web
/// Mercator) out to `reach` (its half diagonal then). Kept by the view's own context.
#[derive(Clone, Copy)]
struct Rise {
    at: Instant,
    from: [f64; 2],
    reach: f64,
}

fn rise_id() -> Id {
    Id::new("earth-houses-rise")
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(Store { tiles: HashMap::new(), address: None }));

fn store() -> MutexGuard<'static, Store> {
    crate::crash_guard::lock(&STORE)
}

/// How much of the houses shows at this zoom (0-1: they fade in at street level).
pub fn showing(flat: &FlatView) -> f32 {
    ((flat.zoom() - SHOW_FROM) / FADE).clamp(0.0, 1.0) as f32
}

/// The level of tiles a view asks for (tiles of 512 pixels, as a vector map draws them), or
/// None while neither the houses (`houses`) nor the ground (`ground`) shows in it.
fn level(flat: &FlatView, houses: bool, ground: bool) -> Option<u8> {
    let zoom = flat.zoom();
    (houses && showing(flat) > 0.0 || ground && zoom >= GROUND_FROM).then(|| (zoom - 1.0).floor().clamp(0.0, Z as f64) as u8)
}

/// Where a point of the map (Web Mercator, 0-1) `h` metres up stands on the screen, and how far
/// along the view it is (in camera distances: 1 in the middle).
fn to_screen(flat: &FlatView) -> impl Fn([f64; 2], f32) -> (Pos2, f64) + '_ {
    let (metre, ground, lift) = (flat.px_per_km() / 1000.0, flat.grounder(), flat.lifter());
    move |m, h| {
        let ([x, y], depth) = lift(ground(m[0], m[1]), h as f64 * metre);
        (pos2(x as f32, y as f32), depth)
    }
}

/// How bright the light of what stands `depth` along the view is: falling back into the deep,
/// and thinner where the lines crowd further off (its light on a patch of the screen stays).
fn dim(depth: f64) -> f32 {
    model::fade(depth) / depth.max(1.0) as f32
}

/// A line of the map (Web Mercator points) on the screen: its runs in front of the camera.
fn on_screen(flat: &FlatView, points: &[[f64; 2]]) -> Vec<Vec<Pos2>> {
    flat.runs(points.iter().copied()).into_iter().map(|run| run.into_iter().map(|[x, y]| pos2(x as f32, y as f32)).collect()).collect()
}

/// Whether a tile lies over `map`.
fn covers(flat: &FlatView, map: Rect, (z, x, y): (u8, u32, u32)) -> bool {
    let (at, n) = (to_screen(flat), (1u32 << z) as f64);
    let corners = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(i, j)| at([(x + i) as f64 / n, (y + j) as f64 / n], 0.0).0);
    map.intersects(Rect::from_points(&corners))
}

/// The tiles of a level over `map`.
fn tiles_in_view(flat: &FlatView, map: Rect, z: u8) -> Vec<(u8, u32, u32)> {
    let n = 1i64 << z;
    let over = flat.tiles([map.left() as f64, map.top() as f64, map.right() as f64, map.bottom() as f64], z);
    over.into_iter().filter(|(_, y)| (0..n).contains(y)).map(|(x, y)| (z, x.rem_euclid(n) as u32, y as u32)).collect()
}

/// What stands over `map` at level `z`, or else at the level nearest to it that has any here:
/// until a new level's tiles come, the last ones stay.
fn towns(flat: &FlatView, map: Rect, z: u8) -> Vec<Arc<Town>> {
    let s = store();
    let here: Vec<(u8, Arc<Town>)> = s
        .tiles
        .iter()
        .filter_map(|(&tile, t)| match t {
            Tile::Ready(town) if covers(flat, map, tile) => Some((tile.0, town.clone())),
            _ => None,
        })
        .collect();
    let best = here.iter().map(|(level, _)| (level.abs_diff(z), *level)).min().map(|(_, level)| level);
    here.into_iter().filter(|(level, _)| Some(*level) == best).map(|(_, town)| town).collect()
}

/// Asks for the tiles in view that are not here yet: from the cache, else from OpenFreeMap. One
/// round at a time. `houses`: the houses are wanted; `ground`: the streets and the water.
pub fn schedule(rt: &Handle, flat: &FlatView, map: Rect, houses: bool, ground: bool) {
    let Some(z) = level(flat, houses, ground).filter(|_| !model::offline()) else { return };
    let mut s = store();
    if s.tiles.values().any(|t| matches!(t, Tile::Loading)) {
        return;
    }
    let view = tiles_in_view(flat, map, z);
    let wanted: Vec<(u8, u32, u32)> = view
        .iter()
        .copied()
        .filter(|t| match s.tiles.get(t) {
            None => true,
            Some(Tile::Failed(at)) => at.elapsed() >= RETRY,
            _ => false,
        })
        .collect();
    if wanted.is_empty() {
        return;
    }
    if s.tiles.len() > KEEP {
        // (The level next to this one stays while it is in view: it is drawn until this one comes.)
        s.tiles.retain(|t, _| t.0.abs_diff(z) <= 1 && covers(flat, map, *t));
    }
    for tile in &wanted {
        s.tiles.insert(*tile, Tile::Loading);
    }
    let address = s.address.clone();
    drop(s);
    rt.spawn(load(wanted, address));
}

/// The towns of `wanted` into the store, each tile read from the cache or fetched (all at once:
/// a town's outline is a dozen tiles) and kept.
async fn load(wanted: Vec<(u8, u32, u32)>, mut address: Option<String>) {
    let (mut read, mut missing) = (Vec::new(), Vec::new());
    for tile in wanted {
        match tokio::fs::read(cache_file(tile)).await {
            Ok(bytes) => read.push((tile, Some(bytes))),
            Err(_) => missing.push(tile),
        }
    }
    if !missing.is_empty() {
        if address.is_none() {
            address = tile_address().await.map_err(|e| debug!("[EARTH] The map tiles' address did not come: {e}")).ok();
        }
        let got = futures_util::future::join_all(missing.iter().map(|&tile| fetch(tile, address.as_deref()))).await;
        // (Gone: this week's tiles moved on, and their address is read again next round.)
        if got.iter().any(|r| matches!(r, Err((Some(404), _)))) {
            address = None;
        }
        read.extend(missing.into_iter().zip(got).map(|(tile, got)| (tile, got.map_err(|(_, e)| debug!("[EARTH] Map tile {tile:?} did not come: {e}")).ok())));
    }
    let town = |bytes: Vec<u8>, (z, x, y): (u8, u32, u32)| {
        let (ways, names) = ground(&bytes, (z, x, y));
        // (Only the last level's tiles carry every house.)
        Arc::new(Town { houses: if z == Z { houses(&bytes, (x, y)) } else { Vec::new() }, ways, names, came: Some(Instant::now()) })
    };
    let built = tokio::task::spawn_blocking(move || read.into_iter().map(|(tile, bytes)| (tile, bytes.map(|b| town(b, tile)))).collect::<Vec<_>>()).await.unwrap_or_default();
    let count = |of: fn(&Town) -> usize| built.iter().filter_map(|(_, town)| town.as_deref()).map(of).sum::<usize>();
    info!("[EARTH] {} building(s) and {} line(s) of streets, rails and water in {} map tile(s)", count(|t| t.houses.len()), count(|t| t.ways.len()), built.len());
    let mut s = store();
    // (A round that was lost leaves nothing loading: its tiles are asked for again.)
    s.tiles.retain(|_, t| !matches!(t, Tile::Loading));
    for (tile, buildings) in built {
        s.tiles.insert(tile, buildings.map_or(Tile::Failed(Instant::now()), Tile::Ready));
    }
    s.address = address;
    drop(s);
    model::wake();
}

/// Where this week's tiles are ("…/{z}/{x}/{y}.pbf"), as the tile set names it.
async fn tile_address() -> Result<String, String> {
    let set = model::get_json(TILE_SET, Duration::from_secs(15)).await?;
    set["tiles"][0].as_str().filter(|t| t.starts_with("https://")).map(str::to_string).ok_or_else(|| "the tile set names no address".to_string())
}

/// A tile from OpenFreeMap, kept in the cache.
async fn fetch(tile: (u8, u32, u32), address: Option<&str>) -> Result<Vec<u8>, (Option<u16>, String)> {
    let address = address.ok_or((None, "no address for this week's tiles".to_string()))?;
    let url = address.replace("{z}", &tile.0.to_string()).replace("{x}", &tile.1.to_string()).replace("{y}", &tile.2.to_string());
    let bytes = model::get(&url, Duration::from_secs(20)).await?;
    model::cache_write(cache_file(tile), bytes.clone()).await;
    Ok(bytes)
}

// ------------------------------------------------------------------------------ painting ----

/// Dark glass with `light` on it: what is behind dims, the light adds.
fn glass(light: Color32, strength: f32, dark: f32) -> Color32 {
    let f = |v: u8| (v as f32 * strength.clamp(0.0, 1.0)) as u8;
    Color32::from_rgba_premultiplied(f(light.r()), f(light.g()), f(light.b()), (dark.clamp(0.0, 1.0) * 255.0) as u8)
}

/// How far a house has risen (0-1) `secs` after its rise began, `far` (0-1) of the way out from
/// where the wave started, with its own moment `jitter` (0-1) in the wave.
fn grown(secs: f32, far: f32, jitter: f32) -> f32 {
    ((secs - WAVE_SECS * far.clamp(0.0, 1.0) - JITTER_SECS * jitter) / RISE_SECS).clamp(0.0, 1.0)
}

/// A house's own moment in the wave (0-1), the same in every frame.
fn jitter(m: [f64; 2]) -> f32 {
    ((m[0] * 7_919_351.3 + m[1] * 3_617_093.7).sin() * 43_758.545).fract().abs() as f32
}

/// The houses in view as Stark's holograms, the far ones first: each traced on the ground as it
/// comes, then risen from that plate in a wave out from the middle of the view under a white-hot
/// edge; walls of glass lit from their feet, white roof lines, uprights in the accent, then the
/// storeys, the windows up close, a tower's height scale and what the roofs say (off the map's
/// cards and keys, `keep_off`). The house `chosen` stands on (a place Sir asked for) burns
/// brightest. Whether any stood on the map, and whether any is still rising.
pub fn paint(p: &Painter, flat: &FlatView, map: Rect, strength: f32, keep_off: &[Rect], chosen: Option<[f64; 2]>) -> (bool, bool) {
    let strength = strength * showing(flat);
    if strength <= 0.0 {
        // (Out of view: they rise again when they come back.)
        p.ctx().data_mut(|d| d.remove::<Rise>(rise_id()));
        return (false, false);
    }
    let fresh = Rise { at: Instant::now(), from: [model::merc_x(flat.lon0), model::merc_y(flat.lat0)], reach: map.size().length() as f64 / 2.0 / flat.world };
    let rise = p.ctx().data_mut(|d| *d.get_temp_mut_or_insert_with(rise_id(), || fresh));
    let now = Instant::now();
    let tiles = towns(flat, map.expand(120.0), Z);
    let screen = to_screen(flat);
    let (metre, eye, on_ground) = (flat.px_per_km() / 1000.0, flat.eye(), flat.grounder());
    // Each house: how far from the camera on the ground, how far along the view and how far it
    // has risen. One under or behind the camera, or beyond the houses' reach, is not seen.
    let mut seen: Vec<(f64, f64, &Building, f32)> = tiles
        .iter()
        .flat_map(|t| {
            let secs = now.saturating_duration_since(t.came.map_or(rise.at, |came| came.max(rise.at))).as_secs_f32();
            t.houses.iter().map(move |b| (b, secs))
        })
        .filter_map(|(b, secs)| {
            let (at, depth) = screen(b.mid, 0.0);
            let reached = depth < HOUSES_TO || chosen.is_some_and(|c| b.holds(c));
            (depth >= model::NEAR && reached && map.expand((b.reach * flat.world / depth) as f32 + 60.0).contains(at)).then(|| {
                let far = (b.mid[0] - rise.from[0]).hypot(b.mid[1] - rise.from[1]) / rise.reach.max(1e-12);
                let g = on_ground(b.mid[0], b.mid[1]);
                ((g[0] - eye[0]).hypot(g[1] - eye[1]), depth, b, grown(secs, far as f32, jitter(b.mid)))
            })
        })
        .collect();
    let (stood, rising) = (!seen.is_empty(), seen.iter().any(|s| s.3 < 1.0));
    seen.retain(|s| s.3 > 0.0);
    if seen.len() > MAX_DRAWN {
        // (The nearest the camera, in no order: they are sorted far to near next. Straight down
        // that is the nearest the middle.)
        seen.select_nth_unstable_by(MAX_DRAWN, |a, b| a.0.total_cmp(&b.0));
        seen.truncate(MAX_DRAWN);
    }
    // The far ones first: a nearer house stands in front.
    seen.sort_by(|a, b| b.1.total_cmp(&a.1));
    let theme = crate::holo::theme();
    let (accent, ink) = (theme.accent(), theme.ink());
    let upright = seen.len() <= EDGES_UP_TO;
    let storeys = seen.len() <= STOREYS_UP_TO;
    let mut panes = 0;
    let mut light = crate::holo::Light::new(p);
    let (mut ground, mut top, mut plan, mut walls): (Vec<Pos2>, Vec<Pos2>, Vec<[f64; 2]>, Vec<(f64, usize)>) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for &(_, depth, b, grown) in &seen {
        // (The far ones fall back into the deep; the one Sir asked for stands out wherever.)
        let picked = chosen.is_some_and(|c| b.holds(c));
        let strength = strength * dim(depth) * if picked { 1.0 } else { behind(depth) };
        // The rise: the plate traced on the ground, then the walls up from it (fast, landing
        // softly) under a white-hot edge; then the storeys, the windows and the scale resolve.
        let trace = phase(grown, 0.0, 0.3);
        let lift = ease_out_cubic(phase(grown, 0.3, 1.0));
        let scan = 1.0 - phase(grown, 0.75, 1.0);
        let settle = phase(grown, 0.8, 1.0);
        let up = |h: f32| b.base + (h - b.base) * lift;
        // (Glass dark enough that a house dims the one behind it, as outlines seen through each
        // other were a tangle, and light enough to be a hologram's: the light is the lines'.)
        let face = if picked { 0.22 } else { 0.15 };
        let roof = [glass(ink, 0.05 * strength, 0.72 * strength)];
        let edge = if picked { lit(ink, 0.95 * strength) } else { lit(accent, 0.6 * strength) };
        let rim = lit(ink, (0.7 + 0.3 * if picked { 1.0 } else { scan }) * strength);
        let (storey, pane, plate, hip_line) = (lit(accent, 0.18 * settle * strength), lit(accent, 0.13 * settle * strength), lit(accent, 0.45 * strength), lit(ink, 0.32 * strength));
        ground.clear();
        top.clear();
        plan.clear();
        // (Under a pitched roof the walls end at the eaves.)
        let eaves = b.top - b.hip.as_ref().map_or(0.0, |hip| hip.rise);
        let mut nearest = f64::MAX;
        for m in &b.ring {
            ground.push(screen(*m, b.base).0);
            let (at, depth) = screen(*m, up(eaves));
            nearest = nearest.min(depth);
            top.push(at);
            plan.push(on_ground(m[0], m[1]));
        }
        // (Come down among the houses, one reaching over the camera is not drawn: its roof
        // there would be thrown across the screen.)
        if nearest < model::NEAR {
            continue;
        }
        // A house a few pixels across is its roof's edge alone: its walls would be specks.
        let size = ((b.reach * flat.world / depth) as f32).max((b.top - b.base) * (metre / depth) as f32);
        let (small, plain) = (size < SMALL_PX, size < PLAIN_PX && !picked);
        let n = ground.len();
        if trace < 1.0 {
            // Its plate, traced round on the ground as it comes.
            let along = trace * n as f32;
            for k in 0..along.ceil() as usize {
                light.seg(ground[k], ground[k].lerp(ground[(k + 1) % n], (along - k as f32).min(1.0)), 1.0, plate);
            }
            continue;
        }
        let seam = |i: usize| b.seam.get(i).copied().unwrap_or(false);
        // A wall's outward side on the ground (north up), as a unit.
        let out = |i: usize| {
            let (a, b) = (plan[i], plan[(i + 1) % n]);
            vec2((b[1] - a[1]) as f32, (a[0] - b[0]) as f32).normalized()
        };
        // The walls turned to the camera, the far ones first.
        walls.clear();
        walls.extend((0..n).filter(|i| !small && !seam(*i)).filter_map(|i| {
            let (a, b) = (plan[i], plan[(i + 1) % n]);
            let o = out(i);
            let mid = [(a[0] + b[0]) / 2.0 - eye[0], (a[1] + b[1]) / 2.0 - eye[1]];
            ((eye[0] - a[0]) * o.x as f64 + (eye[1] - a[1]) * o.y as f64 > 0.0).then(|| (mid[0].hypot(mid[1]), i))
        }));
        walls.sort_by(|a, b| b.0.total_cmp(&a.0));
        // A corner of the house: where its outline turns (a round wall is many points and no
        // corner: a comb of edges in the window of 2026-11-19), and no point the tile cut it at.
        let corner = |k: usize| {
            let (before, after) = (out((k + n - 1) % n), out(k));
            (before.x * after.y - before.y * after.x).abs() > 0.3 && !seam(k) && !seam((k + n - 1) % n)
        };
        // The storeys along the walls, where they are far enough apart to be read.
        let floors = (((eaves - b.base) / STOREY_M).round() as usize).clamp(1, 60);
        let tall = (0..n).map(|i| (top[i] - ground[i]).length()).fold(0.0, f32::max);
        let storeyed = storeys && settle > 0.0 && tall / floors as f32 >= 3.0;
        // An upright edge: white-hot in the accent's glow on the house Sir asked for.
        let upright_edge = |light: &mut crate::holo::Light, from: Pos2, to: Pos2| {
            if picked {
                light.halo(&[from, to], false, 9.0, lit(accent, 0.5 * strength));
            }
            light.seg(from, to, if picked { 1.4 } else { 1.0 }, edge);
        };
        for &(_, i) in &walls {
            let j = (i + 1) % n;
            let shade = 0.55 + 0.45 * out(i).dot(SUN.normalized()).max(0.0);
            // (Lit from its foot, as from the table it is cast from, fading upwards; the plate's
            // edge along its foot.)
            light.sheet(&[ground[i], ground[j]], &[top[i], top[j]], &[glass(accent, face * shade * strength, 0.6 * strength)], &[glass(accent, 0.04 * shade * strength, 0.7 * strength)]);
            if !plain {
                light.seg(ground[i], ground[j], 1.0, plate);
            }
            if storeyed {
                for k in 1..floors {
                    let t = k as f32 / floors as f32;
                    light.seg(ground[i].lerp(top[i], t), ground[j].lerp(top[j], t), 1.0, storey);
                }
                // And its rooms across them, up close: a pane in each, else the bays' lines.
                let (a, c) = (plan[i], plan[j]);
                let bays = ((c[0] - a[0]).hypot(c[1] - a[1]) / metre / BAY_M as f64).round() as usize;
                let bay = (ground[j] - ground[i]).length() / bays.max(1) as f32;
                let room = (top[i] - ground[i]).length().min((top[j] - ground[j]).length()) / floors as f32;
                if bay >= 6.0 && room >= 5.0 && (2..=60).contains(&bays) && panes + bays * floors <= PANES_UP_TO {
                    panes += bays * floors;
                    let at = |s: f32, t: f32| ground[i].lerp(ground[j], s).lerp(top[i].lerp(top[j], s), t);
                    for k in 0..floors {
                        let (t0, t1) = ((k as f32 + 0.3) / floors as f32, (k as f32 + 0.8) / floors as f32);
                        for q in 0..bays {
                            let (s0, s1) = ((q as f32 + 0.22) / bays as f32, (q as f32 + 0.78) / bays as f32);
                            light.sheet(&[at(s0, t0), at(s1, t0)], &[at(s0, t1), at(s1, t1)], &[pane], &[pane]);
                        }
                    }
                } else if bay >= 7.0 && (2..=40).contains(&bays) {
                    for k in 1..bays {
                        let t = k as f32 / bays as f32;
                        light.seg(ground[i].lerp(ground[j], t), top[i].lerp(top[j], t), 1.0, storey);
                    }
                }
            }
            if upright || picked {
                // (The next wall draws its own first corner.)
                if corner(i) {
                    upright_edge(&mut light, ground[i], top[i]);
                }
                if corner(j) && !walls.iter().any(|w| w.1 == j) {
                    upright_edge(&mut light, ground[j], top[j]);
                }
            }
        }
        // A tower's height scale on its corner nearest the camera, read off in metres.
        if storeyed && b.top >= TALL_M {
            let near = |k: usize| (plan[k][0] - eye[0]).hypot(plan[k][1] - eye[1]);
            let front = (0..n).filter(|k| corner(*k) && walls.iter().any(|w| w.1 == *k || (w.1 + 1) % n == *k)).min_by(|x, y| near(*x).total_cmp(&near(*y)));
            let span = (eaves - b.base).max(1.0);
            if let Some(k) = front.filter(|k| (top[*k] - ground[*k]).length() * TICK_M / span >= 4.0) {
                let side = if ground[k].x < screen(b.mid, b.base).0.x { -1.0 } else { 1.0 };
                let tick = lit(accent, 0.5 * settle * strength);
                for s in 1..=(span / TICK_M) as usize {
                    let at = ground[k].lerp(top[k], s as f32 * TICK_M / span);
                    light.seg(at, at + vec2(side * if s % 5 == 0 { 6.0 } else { 3.0 }, 0.0), 1.0, tick);
                }
            }
        }
        if let Some(hip) = b.hip.as_ref().filter(|_| !plain) {
            // Four faces up to the ridge, each as bright as it is turned to the light, those
            // turned away from the camera first; the ridge and the hips as lines.
            let ridge = hip.ridge.map(|end| screen(end, up(b.top)).0);
            let facing = |k: usize| {
                let (i, o) = ((hip.long + k) % 4, out((hip.long + k) % 4));
                (eye[0] - plan[i][0]) * o.x as f64 + (eye[1] - plan[i][1]) * o.y as f64
            };
            let mut faces = [0, 1, 2, 3];
            faces.sort_by(|a, b| facing(*a).total_cmp(&facing(*b)));
            for k in faces {
                let (i, j) = ((hip.long + k) % 4, (hip.long + k + 1) % 4);
                let shade = out(i).dot(SUN.normalized()).max(0.0);
                let face = [glass(ink, (0.03 + 0.11 * shade) * strength, 0.86 * strength)];
                // (Along the ridge, then over the far end, back along it, over the near end.)
                let up = [[ridge[0], ridge[1]], [ridge[1], ridge[1]], [ridge[1], ridge[0]], [ridge[0], ridge[0]]][k];
                light.sheet(&[top[i], top[j]], &up, &face, &face);
                light.seg(top[i], up[0], 1.0, hip_line);
            }
            light.seg(ridge[0], ridge[1], 1.0, rim);
        } else if !plain {
            for t in &b.roof {
                // (A triangle as a sheet whose far edge is one point.)
                let [p0, p1, p2] = t.map(|i| top[i as usize]);
                light.sheet(&[p0, p1], &[p2, p2], &roof, &roof);
            }
            // A flat roof's parapet up close: a faint line just inside its edge.
            if b.seam.is_empty() && settle > 0.0 && b.reach * flat.world / depth >= 14.0 {
                let metres = 40_075_017.0 * model::lat_of_merc_y(b.mid[1]).to_radians().cos();
                let inside: Vec<Pos2> = b
                    .ring
                    .iter()
                    .map(|m| {
                        let (dx, dy) = (b.mid[0] - m[0], b.mid[1] - m[1]);
                        let k = (0.9 / (dx.hypot(dy) * metres).max(1e-6)).min(0.25);
                        screen([m[0] + dx * k, m[1] + dy * k], up(eaves)).0
                    })
                    .collect();
                light.strip(&inside, true, 1.0, lit(ink, 0.22 * settle * strength));
            }
        }
        // The roof's edge, white-hot while it rises (and on the house Sir asked for).
        let glow = if upright || picked { scan.max(if picked { 0.8 } else { 0.0 }) } else { 0.0 };
        if b.seam.is_empty() {
            light.neon(&top, true, 1.0, rim, glow);
        } else {
            for i in (0..n).filter(|i| !seam(*i)) {
                light.neon(&[top[i], top[(i + 1) % n]], false, 1.0, rim, glow);
            }
        }
    }
    light.paint(p);
    // What the roofs say once they stand, the nearest houses first: a house number up close, a
    // tower's height. Each on a roof wide enough for it, inside the map and clear of the others.
    let numbers = flat.zoom() >= NUMBERS_FROM;
    let mut taken = keep_off.to_vec();
    for &(_, depth, b, grown) in seen.iter().rev() {
        if taken.len() >= keep_off.len() + MAX_LABELS {
            break;
        }
        // (No label is narrower than 12 px: a roof that could hold none is not asked for one.)
        let settle = phase(grown, 0.8, 1.0);
        if settle <= 0.0 || depth > LABELS_TO || (b.reach * flat.world / depth) as f32 * 1.4 < 12.0 {
            continue;
        }
        let Some(label) = b.label(numbers) else { continue };
        let (centre, near) = screen(b.mid, b.top);
        let room = Rect::from_center_size(centre, vec2(label.chars().count() as f32 * 6.0 + 6.0, 13.0));
        if near >= model::NEAR && (b.reach * flat.world / depth) as f32 * 1.4 >= room.width() && map.contains_rect(room) && !taken.iter().any(|t| t.intersects(room)) {
            crate::holo::text(p, centre, Align2::CENTER_CENTER, crate::holo::Face::Mono, &label, 9.5, crate::holo::a(ink, 0.85 * strength * dim(depth) * behind(depth) * settle));
            taken.push(room);
        }
    }
    (stood, rising)
}

/// How a line of the ground is drawn at a zoom: whether it is water (the deep tone; the rest is
/// ink), how bright its line is and how wide the street is in metres (a band of faint light
/// under the line once that is wide enough to see); None while it is too small to be drawn.
fn look(kind: Kind, zoom: f64) -> Option<(bool, f32, f32)> {
    // (From what zoom, how bright, how wide: a motorway down to a footpath.)
    const ROADS: [(f64, f32, f32); 7] = [(0.0, 0.8, 26.0), (0.0, 0.7, 18.0), (11.0, 0.58, 14.0), (12.0, 0.48, 12.0), (13.5, 0.36, 9.0), (14.5, 0.24, 6.0), (15.5, 0.16, 0.0)];
    let (from, water, bright, metres) = match kind {
        Kind::Road(rank) => {
            let (from, bright, metres) = ROADS[rank.min(6) as usize];
            (from, false, bright, metres)
        }
        Kind::Rail => (11.5, false, 0.3, 0.0),
        Kind::Shore => (0.0, true, 0.6, 0.0),
        Kind::Stream => (12.0, true, 0.45, 0.0),
    };
    (zoom >= from).then_some((water, bright, metres))
}

/// How much of the ground shows at this zoom on the hologram, and the level of its tiles.
fn ground_showing(flat: &FlatView, strength: f32) -> Option<(f32, u8)> {
    let strength = strength * model::smoothstep(GROUND_FROM, GROUND_FROM + 1.0, flat.zoom()) as f32;
    level(flat, false, true).filter(|_| strength > 0.0).map(|z| (strength, z))
}

/// The hologram's ground from a town's outline down, under its houses: the streets as lines of
/// ink by their rank (thick and faint beside thin and bright: a street as wide as it is under
/// its line, once that can be seen), the rails fainter, the shores and the streams in the deep
/// tone.
pub fn paint_ground(p: &Painter, flat: &FlatView, map: Rect, strength: f32) -> bool {
    let Some((strength, z)) = ground_showing(flat, strength) else { return false };
    let mut drawn = false;
    let (zoom, metre, at) = (flat.zoom(), (flat.px_per_km() / 1000.0) as f32, to_screen(flat));
    let theme = crate::holo::theme();
    let mut light = crate::holo::Light::new(p);
    for way in towns(flat, map, z).iter().flat_map(|t| t.ways.iter()) {
        let Some((water, bright, metres)) = look(way.kind, zoom) else { continue };
        let colour = if water { theme.deep() } else { theme.ink() };
        // (As wide and as bright as it is near: the far ones fall back into the deep.)
        let depth = at(way.mid, 0.0).1.max(model::NEAR);
        let (wide, fade) = (metres * metre / depth as f32, dim(depth));
        for line in on_screen(flat, &way.points).iter().filter(|run| map.intersects(Rect::from_points(run))) {
            if wide >= 3.0 {
                light.strip(line, false, wide, lit(colour, 0.07 * strength * fade));
            }
            light.strip(line, false, 1.0, lit(colour, bright * strength * fade));
            drawn = true;
        }
    }
    light.paint(p);
    drawn
}

/// The longest stretch of a line on the screen that runs nearly straight (none of it more than
/// 3 px off the line between its ends: a name along it stays on the street), as far as `map`
/// shows it: its two ends there. (A street's ends may lie far outside the map.)
fn straightest(line: &[Pos2], map: Rect) -> Option<(Pos2, Pos2)> {
    // (How far `p` lies off the line through `a` and `b`.)
    let off = |a: Pos2, b: Pos2, p: Pos2| ((b - a).x * (p - a).y - (b - a).y * (p - a).x).abs() / (b - a).length().max(1e-3);
    let (lo, hi) = ([map.left() as f64, map.top() as f64], [map.right() as f64, map.bottom() as f64]);
    let at = |p: [f64; 2]| pos2(p[0] as f32, p[1] as f32);
    let mut best: Option<(Pos2, Pos2)> = None;
    let mut s = 0;
    for i in 1..line.len() {
        // (It ends where the line ends or bends away.)
        if i + 1 == line.len() || line[s + 1..=i].iter().any(|p| off(line[s], line[i + 1], *p) > 3.0) {
            let shown = clip([line[s].x as f64, line[s].y as f64], [line[i].x as f64, line[i].y as f64], lo, hi).map(|(a, b)| (at(a), at(b)));
            if let Some(stretch) = shown.filter(|(a, b)| best.is_none_or(|(c, d)| (*b - *a).length() > (d - c).length())) {
                best = Some(stretch);
            }
            s = i;
        }
    }
    best
}

/// The streets' names along them, as on a drawing: above the line and turned with it, the great
/// streets first, each name once, where a straight stretch of the street is long enough, inside
/// the map and clear of `taken` (which they then join).
pub fn paint_names(p: &Painter, flat: &FlatView, map: Rect, strength: f32, taken: &mut Vec<Rect>) {
    let Some((strength, z)) = ground_showing(flat, strength) else { return };
    let zoom = flat.zoom();
    let towns = towns(flat, map, z);
    let mut stretches: Vec<(u8, f32, Pos2, Pos2, &str)> = Vec::new();
    for way in towns.iter().flat_map(|t| t.names.iter()) {
        let (Some(name), Kind::Road(rank)) = (way.name.as_deref(), way.kind) else { continue };
        if look(way.kind, zoom).is_none() {
            continue;
        }
        for line in on_screen(flat, &way.points) {
            if let Some((a, b)) = straightest(&line, map) {
                stretches.push((rank, (b - a).length(), a, b, name));
            }
        }
    }
    stretches.sort_by(|x, y| x.0.cmp(&y.0).then(y.1.total_cmp(&x.1)));
    let ink = crate::holo::theme().ink();
    let mut said: Vec<&str> = Vec::new();
    for (_, length, a, b, name) in stretches {
        // (A name is at least six pixels a letter: no layout for a stretch too short anyway.)
        if said.len() >= MAX_NAMES || said.contains(&name) || name.chars().count() as f32 * 6.0 > length {
            continue;
        }
        // (Fainter as the ground under it falls back.)
        let mid = a.lerp(b, 0.5);
        let colour = crate::holo::a(ink, 0.75 * strength * dim(flat.under(mid.x as f64, mid.y as f64).1));
        let g = crate::holo::galley(p, crate::holo::Face::Label, name, 8.0, colour);
        if g.size().x + 16.0 > length {
            continue;
        }
        // Read from the left; the text stands on the street's upper side.
        let along = if b.x < a.x { (a - b) / length } else { (b - a) / length };
        let down = vec2(-along.y, along.x);
        let half = g.size() / 2.0;
        let middle = a.lerp(b, 0.5) - down * (half.y + 2.0);
        let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(s, t)| middle + along * half.x * s + down * half.y * t);
        let room = Rect::from_points(&corners);
        if !map.contains_rect(room) || taken.iter().any(|t| t.intersects(room)) {
            continue;
        }
        // (Level, it stands on the pixel grid, as all level text does: crisp.)
        let corner = middle - along * half.x - down * half.y;
        let corner = if along.y.abs() < 1e-3 { pos2(p.round_to_pixel(corner.x), p.round_to_pixel(corner.y)) } else { corner };
        p.add(eframe::egui::epaint::TextShape::new(corner, g, colour).with_angle(along.y.atan2(along.x)));
        taken.push(room);
        said.push(name);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    static TOWN: Mutex<()> = Mutex::new(());

    /// The made-up town while a test uses it: no other test's houses meanwhile, none after.
    pub struct StandIn(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for StandIn {
        fn drop(&mut self) {
            store().tiles.clear();
        }
    }

    /// The houses of the view drawn in `ctx` as if they had come into it `secs` ago, from where
    /// they rose then (the middle of the view): mid-rise for a picture, or long since risen.
    pub fn rose(ctx: &eframe::egui::Context, secs: f32) {
        let at = Instant::now().checked_sub(Duration::from_secs_f32(secs)).unwrap_or_else(Instant::now);
        ctx.data_mut(|d| {
            let rise = d.get_temp::<Rise>(rise_id()).map_or(Rise { at, from: [0.0; 2], reach: 1.0 }, |r| Rise { at, ..r });
            d.insert_temp(rise_id(), rise);
        });
    }

    /// A made-up town around a point for the pictures and the geometry checks: `count` houses
    /// in a square of rows, some of them L-shaped, of growing height, with streets between the
    /// rows and the columns (every fifth a main street, named), a river across and a lake.
    pub fn stand_in(lat: f64, lon: f64, count: usize) -> StandIn {
        let town = StandIn(TOWN.lock().unwrap_or_else(|e| e.into_inner()));
        let (dlat, dlon) = (0.00018, 0.00027);
        let rows = (count as f64).sqrt().ceil().max(1.0) as usize;
        let n = (1u32 << Z) as f64;
        let mut by_tile: HashMap<(u32, u32), Vec<Building>> = HashMap::new();
        for k in 0..count {
            let (row, col) = ((k / rows) as f64 - rows as f64 / 2.0, (k % rows) as f64 - rows as f64 / 2.0);
            // (North is up: clockwise on the screen is north-west, north-east, south-east, south-west.)
            let (s, w) = (lat + row * dlat * 1.6, lon + col * dlon * 1.5);
            let outline = if k % 3 == 0 {
                vec![[s + dlat, w], [s + dlat, w + dlon * 0.5], [s + dlat * 0.5, w + dlon * 0.5], [s + dlat * 0.5, w + dlon], [s, w + dlon], [s, w]]
            } else {
                vec![[s + dlat, w], [s + dlat, w + dlon], [s, w + dlon], [s, w]]
            };
            let ring: Vec<[f64; 2]> = outline.iter().map(|p| [model::merc_x(p[1]), model::merc_y(p[0])]).collect();
            let tile = ((ring[0][0] * n) as u32, (ring[0][1] * n) as u32);
            let mut house = Building::new(ring, Vec::new(), 0.0, 6.0 + (k % 7) as f32 * 5.0);
            house.number = Some((k + 1).to_string());
            by_tile.entry(tile).or_default().push(house);
        }
        // The ground: every street in the tile of the town's middle (a tile draws what it holds).
        let at = |lat: f64, lon: f64| [model::merc_x(lon), model::merc_y(lat)];
        let way = |kind: Kind, points: Vec<[f64; 2]>, name: Option<String>| {
            let (mid, reach) = span(&points);
            Way { kind, points, mid, reach, name }
        };
        let (south, north) = (lat - (rows as f64 / 2.0 + 1.0) * dlat * 1.6, lat + (rows as f64 / 2.0 + 1.0) * dlat * 1.6);
        let (west, east) = (lon - (rows as f64 / 2.0 + 1.0) * dlon * 1.5, lon + (rows as f64 / 2.0 + 1.0) * dlon * 1.5);
        const NAMES: [&str; 6] = ["Hauptstraße", "Bahnhofstraße", "Lindenallee", "Gartenweg", "Am Markt", "Schulstraße"];
        let mut ground = Town::default();
        for k in 0..=rows {
            let (s, w) = (lat + (k as f64 - rows as f64 / 2.0) * dlat * 1.6 - dlat * 0.3, lon + (k as f64 - rows as f64 / 2.0) * dlon * 1.5 - dlon * 0.25);
            let main = k % 5 == 0;
            let kind = Kind::Road(if main { 1 } else { 4 });
            for points in [vec![at(s, west), at(s, east)], vec![at(south, w), at(north, w)]] {
                if main {
                    ground.names.push(way(kind, points.clone(), Some(NAMES[(k / 5 + ground.names.len()) % NAMES.len()].to_string())));
                }
                ground.ways.push(way(kind, points, None));
            }
        }
        ground.ways.push(way(Kind::Stream, vec![at(north, west), at(lat, lon - dlon), at(south, east)], None));
        let lake = [(1.0, 1.0), (1.0, 4.0), (-2.0, 4.0), (-2.0, 1.0), (1.0, 1.0)].map(|(n, e)| at(lat + n * dlat * 3.0, east + e * dlon * 3.0));
        ground.ways.push(way(Kind::Shore, lake.to_vec(), None));
        let middle = ((at(lat, lon)[0] * n) as u32, (at(lat, lon)[1] * n) as u32);
        let mut s = store();
        s.tiles.clear();
        for (tile, houses) in by_tile {
            let ground = if tile == middle { std::mem::take(&mut ground) } else { Town::default() };
            s.tiles.insert((Z, tile.0, tile.1), Tile::Ready(Arc::new(Town { houses, ..ground })));
        }
        town
    }

    // A vector tile written by hand.
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn bytes(field: u64, data: &[u8], out: &mut Vec<u8>) {
        varint(field << 3 | 2, out);
        varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }

    fn number_field(field: u64, v: u64, out: &mut Vec<u8>) {
        varint(field << 3, out);
        varint(v, out);
    }

    fn packed(words: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        words.iter().for_each(|w| varint(*w, &mut out));
        out
    }

    fn zz(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }

    /// A polygon feature of one ring through `points`, with `tags` (key index, value index).
    fn feature(points: &[[i64; 2]], tags: &[u64]) -> Vec<u8> {
        shape(points, tags, true)
    }

    /// A line feature through `points`.
    fn line(points: &[[i64; 2]], tags: &[u64]) -> Vec<u8> {
        shape(points, tags, false)
    }

    fn shape(points: &[[i64; 2]], tags: &[u64], ring: bool) -> Vec<u8> {
        let mut geometry = vec![1 << 3 | 1, zz(points[0][0]), zz(points[0][1]), ((points.len() as u64 - 1) << 3) | 2];
        for w in points.windows(2) {
            geometry.extend([zz(w[1][0] - w[0][0]), zz(w[1][1] - w[0][1])]);
        }
        if ring {
            geometry.push(1 << 3 | 7);
        }
        let mut out = Vec::new();
        bytes(2, &packed(tags), &mut out);
        number_field(3, if ring { 3 } else { 2 }, &mut out);
        bytes(4, &packed(&geometry), &mut out);
        out
    }

    /// A layer whose keys and values are words.
    fn words(name: &str, features: &[Vec<u8>], keys: &[&str], values: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        bytes(1, name.as_bytes(), &mut out);
        features.iter().for_each(|f| bytes(2, f, &mut out));
        keys.iter().for_each(|k| bytes(3, k.as_bytes(), &mut out));
        for v in values {
            let mut text = Vec::new();
            bytes(1, v.as_bytes(), &mut text);
            bytes(4, &text, &mut out);
        }
        number_field(5, 4096, &mut out);
        out
    }

    /// The ground of a tile: its streets by rank, its rails and its shores, each cut at the
    /// tile's edge (a line over the seam is drawn once, by the two tiles together), no tunnel,
    /// no ferry, no shore where the tiler cut the lake; the names along their streets.
    #[test]
    fn a_tile_gives_its_streets_water_and_names_once_over_the_seams() {
        let keys = ["class", "name", "brunnel"];
        let values = ["primary", "minor", "rail", "tunnel", "Musterstraße", "ferry"];
        let roads = |dx: i64| {
            words(
                "transportation",
                &[
                    line(&[[100 + dx, 100], [2000 + dx, 100], [2000 + dx, 2000]], &[0, 0]), // a main street round a corner
                    line(&[[3000 + dx, 500], [5000 + dx, 500]], &[0, 1]),                // a street over the east seam
                    line(&[[-300 + dx, 800], [-100 + dx, 900]], &[0, 1]),                // in the margin: the neighbour's
                    line(&[[100 + dx, 3000], [3000 + dx, 3000]], &[0, 2]),               // a rail
                    line(&[[100 + dx, 3500], [3000 + dx, 3500]], &[0, 1, 2, 3]),         // a tunnel
                    line(&[[100 + dx, 3900], [3000 + dx, 3900]], &[0, 5]),               // a ferry
                ],
                &keys,
                &values,
            )
        };
        // A lake the tiler cut along the margin: only its shore inside the tile is drawn.
        let lake = words("water", &[feature(&[[-64, -64], [2000, -64], [2000, 1000], [-64, 1000]], &[])], &[], &[]);
        let names = words("transportation_name", &[line(&[[100, 100], [2000, 100]], &[0, 0, 1, 4]), line(&[[100, 600], [900, 600]], &[0, 1])], &keys, &values);
        let mut tile = Vec::new();
        for layer in [&lake, &roads(0), &names] {
            bytes(3, layer, &mut tile);
        }
        let (ways, named) = ground(&tile, (14, 8802, 5371));
        let n = (1u32 << 14) as f64;
        let merc = |x: f64, y: f64| [(8802.0 + x / 4096.0) / n, (5371.0 + y / 4096.0) / n];
        let kinds: Vec<(Kind, usize)> = ways.iter().map(|w| (w.kind, w.points.len())).collect();
        assert_eq!(kinds, [(Kind::Shore, 3), (Kind::Road(1), 3), (Kind::Road(4), 2), (Kind::Rail, 2)], "{ways:?}");
        assert_eq!(ways[0].points, [merc(2000.0, 0.0), merc(2000.0, 1000.0), merc(0.0, 1000.0)], "the lake's shore inside the tile");
        assert_eq!(ways[2].points, [merc(3000.0, 500.0), merc(4096.0, 500.0)], "cut at the east edge");
        // The same street seen from the east neighbour starts where this tile's part ends.
        let mut east = Vec::new();
        bytes(3, &roads(-4096), &mut east);
        let (theirs, _) = ground(&east, (14, 8803, 5371));
        let street = theirs.iter().find(|w| w.kind == Kind::Road(4)).expect("the street in the neighbour");
        assert_eq!(street.points, [merc(4096.0, 500.0), merc(5000.0, 500.0)]);
        assert_eq!(theirs.len(), 1, "everything else lies west of the neighbour: {theirs:?}");
        // A name only with its street, and a tile of another level in its own units.
        assert_eq!(named.iter().map(|w| (w.kind, w.name.as_deref())).collect::<Vec<_>>(), [(Kind::Road(1), Some("Musterstraße"))]);
        let (low, _) = ground(&tile, (12, 2200, 1342));
        assert_eq!(low[2].points[1], [(2200.0 + 1.0) / 4096.0, (1342.0 + 500.0 / 4096.0) / 4096.0]);
        // A tile that breaks off gives what was whole; no tile gives nothing.
        assert!(ground(&tile[..tile.len() - 5], (14, 8802, 5371)).0.len() <= ways.len() && ground(&[], (14, 0, 0)).0.is_empty());
        // What shows when: a motorway at a town's outline, a footpath only down at the houses,
        // and water in the deep tone.
        assert_eq!(look(Kind::Road(0), 10.5).map(|l| l.0), Some(false));
        assert!(look(Kind::Road(6), 13.0).is_none() && look(Kind::Road(6), 16.0).is_some());
        assert_eq!(look(Kind::Shore, 10.5).map(|l| l.0), Some(true));
    }

    /// A street's name goes on its straightest stretch inside the map.
    /// A town rises in a wave from the middle of the view out: nothing at the first moment, the
    /// middle first, each house at a moment of its own (the same in every frame), all of it
    /// standing within two seconds.
    #[test]
    fn the_houses_rise_from_the_middle_of_the_view_outwards() {
        assert_eq!(grown(0.0, 0.0, 0.0), 0.0, "nothing at the first moment");
        assert_eq!(grown(RISE_SECS, 0.0, 0.0), 1.0, "the middle's house stands after its rise");
        let half = RISE_SECS / 2.0;
        assert!(grown(half, 0.0, 0.0) > grown(half, 1.0, 0.0), "the middle first");
        assert!(grown(half, 0.0, 0.0) > grown(half, 0.0, 1.0), "each at its own moment");
        assert_eq!(grown(half, 5.0, 0.0), grown(half, 1.0, 0.0), "beyond the edge no later than the edge");
        assert_eq!(grown(WAVE_SECS + JITTER_SECS + RISE_SECS + 0.01, 1.0, 1.0), 1.0, "the edge's last house stands by then");
        assert!(WAVE_SECS + JITTER_SECS + RISE_SECS <= 2.0, "a town in two seconds");
        let m = [0.5326, 0.3401];
        assert_eq!(jitter(m), jitter(m));
        let moments: Vec<f32> = (0..50).map(|k| jitter([0.5 + k as f64 * 1e-5, 0.34])).collect();
        assert!(moments.iter().all(|j| (0.0..=1.0).contains(j)) && moments.iter().any(|j| *j < 0.3) && moments.iter().any(|j| *j > 0.7), "neighbours rise at moments of their own: {moments:?}");
    }

    #[test]
    fn a_name_takes_the_longest_straight_stretch() {
        let map = Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 500.0));
        // Along, round a corner, then a longer run, then out of the map.
        let line = [pos2(10.0, 10.0), pos2(110.0, 10.0), pos2(110.0, 300.0), pos2(110.0, 600.0)];
        assert_eq!(straightest(&line[..3], map), Some((pos2(110.0, 10.0), pos2(110.0, 300.0))));
        assert_eq!(straightest(&line, map), Some((pos2(110.0, 10.0), pos2(110.0, 500.0))), "a stretch leaving the map counts as far as the map shows it");
        // A gentle curve is one stretch while none of it strays 3 px off the line between its
        // ends; on a rounder one the name would float beside the street.
        let curve = |bend: f32| -> Vec<Pos2> { (0..5).map(|k| pos2(50.0 + 60.0 * k as f32, 200.0 + bend * (k * k) as f32)).collect() };
        let (gentle, round) = (curve(0.5), curve(3.0));
        assert_eq!(straightest(&gentle, map), Some((gentle[0], gentle[4])));
        assert_ne!(straightest(&round, map), Some((round[0], round[4])));
        assert_eq!(straightest(&[pos2(10.0, 10.0)], map), None);
    }

    fn layer(name: &str, features: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        bytes(1, name.as_bytes(), &mut out);
        features.iter().for_each(|f| bytes(2, f, &mut out));
        for key in ["render_height", "render_min_height", "hide_3d", "housenumber"] {
            bytes(3, key.as_bytes(), &mut out);
        }
        // Values: 21 (an int), 3 (a double), yes, a name, a house number and 40 (a tower's height).
        let mut int = Vec::new();
        number_field(4, 21, &mut int);
        let mut double = Vec::new();
        varint(3 << 3 | 1, &mut double);
        double.extend(3.0f64.to_le_bytes());
        let mut yes = Vec::new();
        number_field(7, 1, &mut yes);
        let (mut text, mut twelve, mut forty) = (Vec::new(), Vec::new(), Vec::new());
        bytes(1, b"Musterhaus", &mut text);
        bytes(1, b"12", &mut twelve);
        number_field(4, 40, &mut forty);
        // (And 5: what the tiles write for a house the map gives no height.)
        let mut five = Vec::new();
        number_field(4, 5, &mut five);
        [int, double, yes, text, twelve, forty, five].iter().for_each(|v| bytes(4, v, &mut out));
        number_field(5, 4096, &mut out);
        out
    }

    #[test]
    fn a_vector_tile_gives_its_houses_and_nothing_else() {
        let square = |x: i64, y: i64, s: i64| vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]];
        let courtyard: Vec<[i64; 2]> = square(120, 120, 40).into_iter().rev().collect();
        let building = layer(
            "building",
            &[
                feature(&square(100, 100, 200), &[0, 0, 1, 1]), // 21 m high, from 3 m up
                feature(&courtyard, &[0, 0]),                    // runs the other way: a hole
                feature(&square(500, 500, 100), &[0, 0, 2, 2]), // only frames its parts
                feature(&square(4100, 600, 40), &[0, 0]),       // whole in the margin: the neighbour's
                feature(&square(4080, 900, 40), &[0, 0]),       // across the edge, its middle beyond it
                feature(&square(4070, 1200, 40), &[0, 0]),      // across the edge, its middle on this side
                feature(&[[3900, 2000], [4160, 2000], [4160, 2200], [3900, 2200]], &[0, 3]), // cut by the tile, no height
                feature(&square(1000, 1000, 300), &[0, 5]),     // a tower of 40 m
                feature(&square(2000, 3000, 20), &[0, 6]),      // a small house, its height unknown
            ],
        );
        // The numbers: one on the first house, a second one there too, one on open ground.
        let point = |x: i64, y: i64, tags: &[u64]| {
            let mut out = Vec::new();
            bytes(2, &packed(tags), &mut out);
            number_field(3, 1, &mut out);
            bytes(4, &packed(&[1 << 3 | 1, zz(x), zz(y)]), &mut out);
            out
        };
        let numbers = layer("housenumber", &[point(200, 200, &[3, 4]), point(250, 250, &[3, 3]), point(3000, 3000, &[3, 4])]);
        let mut tile = Vec::new();
        bytes(3, &layer("water", &[feature(&square(0, 0, 4096), &[])]), &mut tile);
        bytes(3, &numbers, &mut tile);
        bytes(3, &building, &mut tile);
        let found = houses(&tile, (8802, 5371));
        assert_eq!(found.len(), 5, "{found:?}");
        // The house without a height gets one by its ground (seven metres square here: two
        // storeys under a roof), and a small house gets a hipped roof: a point over a square.
        let small = &found[4];
        let hip = small.hip.as_ref().expect("a roof on the small house");
        assert_eq!(small.top, 7.5);
        assert!((1.5..2.6).contains(&hip.rise) && (hip.ridge[0][0] - hip.ridge[1][0]).abs() < 1e-12 && (hip.ridge[0][0] - small.mid[0]).abs() < 1e-12, "{hip:?}");
        assert!(found[..4].iter().all(|b| b.hip.is_none()), "a tower, a block or a cut house under a pitched roof");
        // A shed, a block and a hall by their ground alone.
        let guessed = |side: i32| house(vec![[100, 100], [100 + side, 100], [100 + side, 100 + side], [100, 100 + side]], 4096, [[-64, -64], [4160, 4160]], (8802, 5371), 0.0, None).expect("a house").top;
        assert_eq!((guessed(10), guessed(20), guessed(60), guessed(200)), (3.0, 7.5, 10.0, 8.5));
        // A roof says its number up close, a tower its height from further up.
        assert_eq!((found[0].number.as_deref(), found[0].label(true), found[0].label(false)), (Some("12"), Some("12".to_string()), None));
        assert_eq!((found[3].top, found[3].number.as_deref(), found[3].label(false)), (40.0, None, Some("40 m".to_string())));
        assert!(found[0].holds(found[0].mid) && !found[0].holds(found[3].mid));
        let n = (1u32 << Z) as f64;
        let first = &found[0];
        assert_eq!((first.base, first.top, first.ring.len(), first.roof.len()), (3.0, 21.0, 4, 2));
        assert!(first.seam.is_empty());
        assert!((first.ring[0][0] - (8802.0 + 100.0 / 4096.0) / n).abs() < 1e-12 && (first.ring[0][1] - (5371.0 + 100.0 / 4096.0) / n).abs() < 1e-12);
        assert!((first.mid[0] - (8802.0 + 200.0 / 4096.0) / n).abs() < 1e-12 && first.reach > 0.0);
        assert!((found[1].ring[0][0] - (8802.0 + 4070.0 / 4096.0) / n).abs() < 1e-12, "the one whose middle is on this side");
        // The cut house keeps its three walls: the edge along the cut is none, and it gets the guess for a height.
        assert_eq!((found[2].seam.clone(), found[2].top), (vec![false, true, false, false], 8.5));
        // The same tile seen from its neighbour: the two houses across the edge swap.
        let cut = [[-64, -64], [4160, 4160]];
        assert!(house(vec![[4080 - 4096, 900], [4120 - 4096, 900], [4120 - 4096, 940], [4080 - 4096, 940]], 4096, cut, (8803, 5371), 0.0, Some(8.0)).is_some());
        assert!(house(vec![[4070 - 4096, 1200], [4110 - 4096, 1200], [4110 - 4096, 1240], [4070 - 4096, 1240]], 4096, cut, (8803, 5371), 0.0, Some(8.0)).is_none());
        // A wall that only stands in the margin, off the cutting line, is a wall.
        let wall_in_margin = house(vec![[4000, 3000], [4100, 3000], [4100, 3100], [4000, 3100]], 4096, cut, (8802, 5371), 0.0, Some(8.0)).expect("a house");
        assert!(wall_in_margin.seam.is_empty());
        // A tile that breaks off gives what was whole; no tile gives nothing.
        assert!(houses(&tile[..tile.len() - 7], (8802, 5371)).is_empty());
        assert!(houses(&[], (0, 0)).is_empty());
        assert_eq!(cache_file((Z, 8802, 5371)), model::cache_path(&["buildings", "14", "8802_5371.pbf"]));
    }

    #[test]
    fn an_outline_is_cut_into_triangles_that_cover_it() {
        // An L, clockwise on the screen: five corners turn right, one turns left.
        let l = vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]];
        let b = Building::new(l, Vec::new(), 0.0, 9.0);
        assert_eq!(b.roof.len(), 4);
        let areas: Vec<f64> = b.roof.iter().map(|t| side(b.ring[t[0] as usize], b.ring[t[1] as usize], b.ring[t[2] as usize]) / 2.0).collect();
        assert!(areas.iter().all(|a| *a > 0.0), "a triangle outside the L: {areas:?}");
        assert!((areas.iter().sum::<f64>() - 3.0).abs() < 1e-9, "{areas:?}");
        assert!(b.mid == [1.0, 1.0] && (b.reach - 2f64.sqrt()).abs() < 1e-12 && b.hip.is_none());
        // A house of twelve metres by eight: the ridge runs four metres along its middle, a
        // little over two metres above the eaves. A hall and a tower keep their flat roofs.
        let (x, y) = (model::merc_x(9.0), model::merc_y(48.0));
        let m = 40_075_017.0 * 48f64.to_radians().cos();
        let block = |long: f64, short: f64, top: f32| Building::new(vec![[x, y], [x + long / m, y], [x + long / m, y + short / m], [x, y + short / m]], Vec::new(), 0.0, top);
        let hip = block(12.0, 8.0, 7.5).hip.expect("a roof");
        let ridge = (hip.ridge[1][0] - hip.ridge[0][0]) * m;
        assert!(hip.long == 0 && (ridge - 4.0).abs() < 0.05 && ((hip.ridge[0][1] - y) * m - 4.0).abs() < 0.05 && (hip.rise - 2.32).abs() < 0.05, "{hip:?}, ridge {ridge}");
        assert_eq!(block(8.0, 12.0, 7.5).hip.expect("a roof").long, 1, "the ridge follows the longer side");
        assert!(block(30.0, 30.0, 8.0).hip.is_none() && block(12.0, 8.0, 20.0).hip.is_none() && block(3.0, 3.0, 3.0).hip.is_none());
        // Every outline of five to nine points on a small grid that runs clockwise without
        // crossing itself is covered exactly, and by triangles that all run its way.
        let mut tried = 0;
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n) as f64
        };
        while tried < 400 {
            // A star about the middle: points at growing angles, so it never crosses itself.
            let n = 5 + next(5) as usize;
            let ring: Vec<[f64; 2]> = (0..n)
                .map(|k| {
                    let angle = std::f64::consts::TAU * (k as f64 + 0.15 + 0.7 * next(100) / 100.0) / n as f64;
                    let far = 2.0 + next(7);
                    [(10.0 + far * angle.cos()).round(), (10.0 + far * angle.sin()).round()]
                })
                .collect();
            // (On the grid a point may have slipped past its neighbour: then it is no such outline.)
            let turn = |p: [f64; 2]| (p[1] - 10.0).atan2(p[0] - 10.0);
            let gaps: Vec<f64> = (0..n).map(|i| (turn(ring[(i + 1) % n]) - turn(ring[i])).rem_euclid(std::f64::consts::TAU)).collect();
            if ring.contains(&[10.0, 10.0]) || gaps.iter().any(|g| *g < 1e-9 || *g > std::f64::consts::PI - 1e-9) || (gaps.iter().sum::<f64>() - std::f64::consts::TAU).abs() > 1e-6 {
                continue;
            }
            let whole: f64 = (1..n - 1).map(|i| side(ring[0], ring[i], ring[i + 1])).sum::<f64>() / 2.0;
            tried += 1;
            let roof = triangulate(&ring);
            let areas: Vec<f64> = roof.iter().map(|t| side(ring[t[0] as usize], ring[t[1] as usize], ring[t[2] as usize]) / 2.0).collect();
            assert!(areas.iter().all(|a| *a >= 0.0) && (areas.iter().sum::<f64>() - whole).abs() < 1e-9, "{ring:?}: {areas:?} of {whole}");
        }
    }

    /// A real tile from a file (`JAI_TILE=<path to a z14 .pbf>`, with `JAI_TILE_XY=x,y`): how
    /// many houses it holds and how tall they are.
    #[test]
    #[ignore]
    fn the_houses_of_a_real_tile() {
        let path = std::env::var("JAI_TILE").expect("JAI_TILE");
        let xy: Vec<u32> = std::env::var("JAI_TILE_XY").unwrap_or_else(|_| "0,0".into()).split(',').filter_map(|v| v.trim().parse().ok()).collect();
        let found = houses(&std::fs::read(path).expect("the tile"), (xy[0], xy[1]));
        let tops: Vec<f32> = found.iter().map(|b| b.top).collect();
        let cut = found.iter().filter(|b| !b.seam.is_empty()).count();
        println!("{} houses ({cut} cut by the tile), {:.1} m to {:.1} m, {} raised off the ground, {} roof triangles", found.len(), tops.iter().copied().fold(f32::MAX, f32::min), tops.iter().copied().fold(0.0, f32::max), found.iter().filter(|b| b.base > 0.0).count(), found.iter().map(|b| b.roof.len()).sum::<usize>());
        assert!(found.len() > 10);
    }
}
