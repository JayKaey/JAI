//! The houses of a town on the Earth's flat map: the building outlines and heights of
//! OpenStreetMap, read from OpenFreeMap's vector tiles (one tile at a time, kept in the Earth's
//! cache), raised from the map as block models of light once the view is down at street level.
//! The map stays north-up: the models lean one way, as in a plan oblique drawing, and a drag
//! with the right button leans them another, so every side of a house can be looked at.

use crate::earth::{self as model, FlatView};
use crate::holo::lit;
use eframe::egui::{pos2, vec2, Align2, Color32, Painter, Pos2, Rect, Vec2};
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
const MAX_DRAWN: usize = 5_000;
const EDGES_UP_TO: usize = 1_500;
/// Where a metre of height goes on the map, in metres: straight up the screen at first.
const LEAN: Vec2 = Vec2::new(0.0, -0.8);
const LEAN_MAX: f32 = 1.3;
/// Where the walls' light comes from on the screen.
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
/// The view's zoom from which the house numbers are read.
const NUMBERS_FROM: f64 = 17.0;
/// The tile set's description: it names the address of this week's tiles.
const TILE_SET: &str = "https://tiles.openfreemap.org/planet";
pub const CREDIT: &str = "Buildings: OpenFreeMap \u{a9} OpenMapTiles, data from OpenStreetMap";

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
        let (lo, hi) = ring.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
        let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        Self { roof: triangulate(&ring), reach: (hi[0] - mid[0]).hypot(hi[1] - mid[1]), mid, hip: Hip::of(&ring, &seam, base, top), ring, seam, base, top, number: None }
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

/// The rings of a polygon's geometry, in the tile's own units.
fn rings(geometry: &[u8]) -> Vec<Vec<[i32; 2]>> {
    let mut words = Fields(geometry);
    let (mut out, mut ring, mut at) = (Vec::new(), Vec::new(), [0i32; 2]);
    while let Some(command) = words.varint() {
        match command & 7 {
            // Move to (a new ring starts) and line to.
            id @ (1 | 2) => {
                for _ in 0..command >> 3 {
                    let (Some(dx), Some(dy)) = (words.varint(), words.varint()) else { return out };
                    at = [at[0].wrapping_add(zigzag(dx) as i32), at[1].wrapping_add(zigzag(dy) as i32)];
                    if id == 1 {
                        ring.clear();
                    }
                    ring.push(at);
                }
            }
            7 => out.push(std::mem::take(&mut ring)),
            _ => return out,
        }
    }
    out
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
            found.extend(rings(geometry).into_iter().map(|ring| (ring, base, top)));
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

fn cache_file(tile: (u32, u32)) -> PathBuf {
    model::cache_path(&["buildings", &Z.to_string(), &format!("{}_{}.pbf", tile.0, tile.1)])
}

// --------------------------------------------------------------------------------- state ----

enum Tile {
    Loading,
    Ready(Arc<Vec<Building>>),
    Failed(Instant),
}

struct Store {
    tiles: HashMap<(u32, u32), Tile>,
    lean: Vec2,
    /// Where this week's tiles are ("…/{z}/{x}/{y}.pbf"), once read from the tile set.
    address: Option<String>,
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(Store { tiles: HashMap::new(), lean: LEAN, address: None }));

fn store() -> MutexGuard<'static, Store> {
    crate::crash_guard::lock(&STORE)
}

/// How much of the houses shows at this zoom (0-1: they fade in at street level).
pub fn showing(flat: &FlatView) -> f32 {
    ((flat.zoom() - SHOW_FROM) / FADE).clamp(0.0, 1.0) as f32
}

/// A drag with the right button: the roofs go with it, so the models lean another way.
pub fn lean_by(drag: Vec2) {
    let mut s = store();
    let lean = s.lean + drag * 0.006;
    s.lean = if lean.length() > LEAN_MAX { lean.normalized() * LEAN_MAX } else { lean };
}

/// The tiles over `map`, nearest the middle first.
fn tiles_in_view(flat: &FlatView, map: Rect) -> Vec<(u32, u32)> {
    let n = 1i64 << Z;
    let over = flat.tiles([map.left() as f64, map.top() as f64, map.right() as f64, map.bottom() as f64], Z);
    over.into_iter().filter(|(_, y)| (0..n).contains(y)).map(|(x, y)| (x.rem_euclid(n) as u32, y as u32)).collect()
}

/// Asks for the tiles in view that are not here yet: from the cache, else from OpenFreeMap. One
/// round at a time.
pub fn schedule(rt: &Handle, flat: &FlatView, map: Rect) {
    if showing(flat) <= 0.0 || model::offline() {
        return;
    }
    let mut s = store();
    if s.tiles.values().any(|t| matches!(t, Tile::Loading)) {
        return;
    }
    let view = tiles_in_view(flat, map);
    let wanted: Vec<(u32, u32)> = view
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
        s.tiles.retain(|t, _| view.contains(t));
    }
    for tile in &wanted {
        s.tiles.insert(*tile, Tile::Loading);
    }
    let address = s.address.clone();
    drop(s);
    rt.spawn(load(wanted, address));
}

/// The houses of `wanted` into the store, each tile read from the cache or fetched and kept.
async fn load(wanted: Vec<(u32, u32)>, mut address: Option<String>) {
    let mut read = Vec::new();
    for tile in wanted {
        let bytes = match tokio::fs::read(cache_file(tile)).await {
            Ok(bytes) => Some(bytes),
            Err(_) => fetch(tile, &mut address).await.map_err(|e| debug!("[EARTH] The buildings of tile {tile:?} did not come: {e}")).ok(),
        };
        read.push((tile, bytes));
    }
    let built = tokio::task::spawn_blocking(move || read.into_iter().map(|(tile, bytes)| (tile, bytes.map(|b| Arc::new(houses(&b, tile))))).collect::<Vec<_>>()).await.unwrap_or_default();
    let count: usize = built.iter().filter_map(|(_, b)| b.as_ref()).map(|b| b.len()).sum();
    info!("[EARTH] {count} building(s) in {} map tile(s)", built.len());
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

/// A tile from OpenFreeMap, kept in the cache. The address of this week's tiles is read from
/// the tile set first, and again when a tile is gone from it.
async fn fetch(tile: (u32, u32), address: &mut Option<String>) -> Result<Vec<u8>, String> {
    if address.is_none() {
        let set = model::get_json(TILE_SET, Duration::from_secs(15)).await?;
        *address = Some(set["tiles"][0].as_str().filter(|t| t.starts_with("https://")).ok_or("the tile set names no address")?.to_string());
    }
    let url = address.as_deref().unwrap_or_default().replace("{z}", &Z.to_string()).replace("{x}", &tile.0.to_string()).replace("{y}", &tile.1.to_string());
    match model::get(&url, Duration::from_secs(20)).await {
        Ok(bytes) => {
            model::cache_write(cache_file(tile), bytes.clone()).await;
            Ok(bytes)
        }
        Err((status, e)) => {
            if status == Some(404) {
                *address = None;
            }
            Err(e)
        }
    }
}

// ------------------------------------------------------------------------------ painting ----

/// Dark glass with `light` on it: what is behind dims, the light adds.
fn glass(light: Color32, strength: f32, dark: f32) -> Color32 {
    let f = |v: u8| (v as f32 * strength.clamp(0.0, 1.0)) as u8;
    Color32::from_rgba_premultiplied(f(light.r()), f(light.g()), f(light.b()), (dark.clamp(0.0, 1.0) * 255.0) as u8)
}

/// The houses in view, the far ones first: walls of dark glass lit towards the roof, the roof's
/// outline in ink, the upright edges in the accent, storey lines on the walls and what the
/// roofs say (off the map's cards and keys, `keep_off`). Whether any stood on the map.
pub fn paint(p: &Painter, flat: &FlatView, map: Rect, strength: f32, keep_off: &[Rect]) -> bool {
    let strength = strength * showing(flat);
    if strength <= 0.0 {
        return false;
    }
    let (tiles, lean) = {
        let s = store();
        let near = tiles_in_view(flat, map.expand(120.0));
        let ready = near.iter().filter_map(|t| match s.tiles.get(t) {
            Some(Tile::Ready(buildings)) => Some(buildings.clone()),
            _ => None,
        });
        (ready.collect::<Vec<_>>(), s.lean)
    };
    let (ox, oy) = (model::merc_x(flat.lon0), model::merc_y(flat.lat0));
    let screen = |m: [f64; 2]| {
        let dx = m[0] - ox;
        pos2((flat.cx + (dx - dx.round()) * flat.world) as f32, (flat.cy + (m[1] - oy) * flat.world) as f32)
    };
    // A metre of height on the screen.
    let rise = lean * (flat.px_per_km() / 1000.0) as f32;
    let middle = map.center();
    let mut seen: Vec<(Pos2, &Building)> = tiles.iter().flat_map(|t| t.iter()).map(|b| (screen(b.mid), b)).filter(|(at, b)| map.expand((b.reach * flat.world) as f32 + 60.0).contains(*at)).collect();
    if seen.len() > MAX_DRAWN {
        seen.sort_by(|a, b| a.0.distance_sq(middle).total_cmp(&b.0.distance_sq(middle)));
        seen.truncate(MAX_DRAWN);
    }
    // What stands further along the lean is behind.
    let depth = |at: Pos2| -(at.x * lean.x + at.y * lean.y);
    seen.sort_by(|a, b| depth(a.0).total_cmp(&depth(b.0)));
    let theme = crate::holo::theme();
    let (accent, ink) = (theme.accent(), theme.ink());
    let upright = seen.len() <= EDGES_UP_TO;
    // (Glass dark enough that a house hides the one behind it: outlines seen through each other
    // were a tangle; the light is the lines', the panes stay faint.)
    let foot = [glass(accent, 0.0, 0.78 * strength)];
    let roof = [glass(ink, 0.05 * strength, 0.86 * strength)];
    let (edge, rim, storey, hip_line) = (lit(accent, 0.55 * strength), lit(ink, 0.7 * strength), lit(accent, 0.2 * strength), lit(ink, 0.32 * strength));
    // A window bay's width on the screen: the walls get their bays where it can be read.
    let bay = BAY_M * (flat.px_per_km() / 1000.0) as f32;
    let storeys = seen.len() <= STOREYS_UP_TO;
    let mut light = crate::holo::Light::new(p);
    let (mut ground, mut top, mut walls): (Vec<Pos2>, Vec<Pos2>, Vec<(f32, usize)>) = (Vec::new(), Vec::new(), Vec::new());
    for (_, b) in &seen {
        ground.clear();
        top.clear();
        // (Under a pitched roof the walls end at the eaves.)
        let eaves = b.top - b.hip.as_ref().map_or(0.0, |hip| hip.rise);
        for m in &b.ring {
            let at = screen(*m);
            ground.push(at + rise * b.base);
            top.push(at + rise * eaves);
        }
        let n = ground.len();
        let seam = |i: usize| b.seam.get(i).copied().unwrap_or(false);
        // The walls turned to the view, the far ones first.
        walls.clear();
        walls.extend((0..n).filter(|i| !seam(*i)).filter_map(|i| {
            let d = ground[(i + 1) % n] - ground[i];
            (vec2(d.y, -d.x).dot(lean) < 0.0).then(|| (depth(ground[i] + d * 0.5), i))
        }));
        walls.sort_by(|a, b| a.0.total_cmp(&b.0));
        // A corner of the house: where its outline turns (a round wall is many points and no
        // corner: a comb of edges in the window of 2026-11-19), and no point the tile cut it at.
        let corner = |k: usize| {
            let (before, after) = ((ground[k] - ground[(k + n - 1) % n]).normalized(), (ground[(k + 1) % n] - ground[k]).normalized());
            (before.x * after.y - before.y * after.x).abs() > 0.3 && !seam(k) && !seam((k + n - 1) % n)
        };
        // The storeys along the walls, where they are far enough apart to be read.
        let floors = (((eaves - b.base) / STOREY_M).round() as usize).clamp(1, 60);
        let storeyed = storeys && rise.length() * (eaves - b.base) / floors as f32 >= 3.0;
        for &(_, i) in &walls {
            let j = (i + 1) % n;
            let out = (ground[j] - ground[i]).normalized().rot90();
            let shade = 0.55 + 0.45 * out.dot(SUN.normalized()).max(0.0);
            light.sheet(&[ground[i], ground[j]], &[top[i], top[j]], &foot, &[glass(accent, 0.12 * shade * strength, 0.86 * strength)]);
            if storeyed {
                for k in 1..floors {
                    let t = k as f32 / floors as f32;
                    light.seg(ground[i].lerp(top[i], t), ground[j].lerp(top[j], t), 1.0, storey);
                }
                // And its bays across them, up close: the wall as a grid of its rooms.
                let bays = ((ground[j] - ground[i]).length() / bay).round() as usize;
                if bay >= 7.0 && (2..=40).contains(&bays) {
                    for k in 1..bays {
                        let t = k as f32 / bays as f32;
                        light.seg(ground[i].lerp(ground[j], t), top[i].lerp(top[j], t), 1.0, storey);
                    }
                }
            }
            if upright {
                // (The next wall draws its own first corner.)
                if corner(i) {
                    light.seg(ground[i], top[i], 1.0, edge);
                }
                if corner(j) && !walls.iter().any(|w| w.1 == j) {
                    light.seg(ground[j], top[j], 1.0, edge);
                }
            }
        }
        if let Some(hip) = &b.hip {
            // Four faces up to the ridge, each as bright as it is turned to the light; the
            // ridge and the hips as lines.
            let ridge = hip.ridge.map(|end| screen(end) + rise * b.top);
            for k in 0..4 {
                let (i, j) = ((hip.long + k) % 4, (hip.long + k + 1) % 4);
                let shade = (top[j] - top[i]).normalized().rot90().dot(SUN.normalized()).max(0.0);
                let face = [glass(ink, (0.03 + 0.11 * shade) * strength, 0.86 * strength)];
                // (Along the ridge, then over the far end, back along it, over the near end.)
                let up = [[ridge[0], ridge[1]], [ridge[1], ridge[1]], [ridge[1], ridge[0]], [ridge[0], ridge[0]]][k];
                light.sheet(&[top[i], top[j]], &up, &face, &face);
                light.seg(top[i], up[0], 1.0, hip_line);
            }
            light.seg(ridge[0], ridge[1], 1.0, rim);
        } else {
            for t in &b.roof {
                // (A triangle as a sheet whose far edge is one point.)
                let [p0, p1, p2] = t.map(|i| top[i as usize]);
                light.sheet(&[p0, p1], &[p2, p2], &roof, &roof);
            }
        }
        if b.seam.is_empty() {
            light.strip(&top, true, 1.0, rim);
        } else {
            for i in (0..n).filter(|i| !seam(*i)) {
                light.seg(top[i], top[(i + 1) % n], 1.0, rim);
            }
        }
    }
    light.paint(p);
    // What the roofs say, the nearest houses first: a house number up close, a tower's height.
    // Each on a roof wide enough for it, inside the map and clear of the others.
    let numbers = flat.zoom() >= NUMBERS_FROM;
    let mut taken = keep_off.to_vec();
    for (at, b) in seen.iter().rev() {
        let Some(label) = b.label(numbers).filter(|_| taken.len() < keep_off.len() + MAX_LABELS) else { continue };
        let centre = *at + rise * b.top;
        let room = Rect::from_center_size(centre, vec2(label.chars().count() as f32 * 6.0 + 6.0, 13.0));
        if (b.reach * flat.world) as f32 * 1.4 >= room.width() && map.contains_rect(room) && !taken.iter().any(|t| t.intersects(room)) {
            crate::holo::text(p, centre, Align2::CENTER_CENTER, crate::holo::Face::Mono, &label, 9.5, crate::holo::a(ink, 0.85 * strength));
            taken.push(room);
        }
    }
    !seen.is_empty()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    static TOWN: Mutex<()> = Mutex::new(());

    /// The made-up town while a test uses it: no other test's houses meanwhile, none after.
    pub struct Town(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for Town {
        fn drop(&mut self) {
            let mut s = store();
            s.tiles.clear();
            s.lean = LEAN;
        }
    }

    /// How the houses lean now.
    pub fn lean() -> Vec2 {
        store().lean
    }

    /// A made-up town around a point for the pictures and the geometry checks: `count` houses
    /// in a square of rows, some of them L-shaped, of growing height.
    pub fn stand_in(lat: f64, lon: f64, count: usize) -> Town {
        let town = Town(TOWN.lock().unwrap_or_else(|e| e.into_inner()));
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
        let mut s = store();
        s.lean = LEAN;
        s.tiles.clear();
        s.tiles.extend(by_tile.into_iter().map(|(tile, buildings)| (tile, Tile::Ready(Arc::new(buildings)))));
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
        let mut geometry = vec![1 << 3 | 1, zz(points[0][0]), zz(points[0][1]), ((points.len() as u64 - 1) << 3) | 2];
        for w in points.windows(2) {
            geometry.extend([zz(w[1][0] - w[0][0]), zz(w[1][1] - w[0][1])]);
        }
        geometry.push(1 << 3 | 7);
        let mut out = Vec::new();
        bytes(2, &packed(tags), &mut out);
        number_field(3, 3, &mut out);
        bytes(4, &packed(&geometry), &mut out);
        out
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
        assert_eq!(cache_file((8802, 5371)), model::cache_path(&["buildings", "14", "8802_5371.pbf"]));
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

    #[test]
    fn the_lean_follows_the_drag_and_stays_in_reach() {
        let _town = stand_in(48.0, 9.0, 1);
        lean_by(vec2(100.0, 80.0));
        assert!((lean() - vec2(0.6, -0.32)).length() < 1e-4, "{:?}", lean());
        lean_by(vec2(5_000.0, 0.0));
        assert!((lean().length() - LEAN_MAX).abs() < 1e-4);
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
