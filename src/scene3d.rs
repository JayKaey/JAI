//! Models built from primitives for the 3D studio: the JSON scene format the language model
//! writes ("an Iron Man suit" as twenty named parts), the built-in library (planets, primitives,
//! simple machines, a few showpieces) and the mesh generators underneath.
//!
//! A scene is `{"name", "materials": {name: {...}}, "parts": [{"name", "shape", "size",
//! "position", "rotation", "material"}]}` with groups (`{"name", "position", "rotation",
//! "parts": [...]}`) allowed. Y is up, units are metres, the model stands on y = 0. The parser
//! is forgiving: other key names ("type", "pos", "color" on a part), colours as names, hex or
//! RGB, a size as one number, unknown shapes as boxes, and output that was cut off mid-way
//! (`repair_json` closes it at the last complete value), so a streaming answer can be shown
//! part by part while it is still being written.

use crate::mesh_io::{math, srgb_to_linear, AlphaMode, Material, Model, Part};
use crate::procedural_textures::{self as pt, Finish, Planet};
use serde_json::Value;
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};

// ------------------------------------------------------------------------------- colours ----

/// Colour names (English and German stems) and their sRGB values.
const COLOURS: &[(&[&str], [u8; 3])] = &[
    (&["iron man red", "hot rod red", "hot-rod red", "candy red"], [0x9B, 0x11, 0x1E]),
    (&["dark red", "dunkelrot", "maroon", "bordeaux", "weinrot", "burgundy"], [0x6D, 0x0F, 0x1F]),
    (&["crimson", "karminrot"], [0xDC, 0x14, 0x3C]),
    (&["red", "rot"], [0xC8, 0x10, 0x2E]),
    (&["orange"], [0xFF, 0x7A, 0x00]),
    (&["yellow", "gelb"], [0xFF, 0xD0, 0x00]),
    (&["dark green", "dunkelgrün", "olive green", "olivgrün", "army green"], [0x2F, 0x4F, 0x1F]),
    (&["lime", "neon green", "neongrün"], [0x7C, 0xFC, 0x00]),
    (&["green", "grün", "gruen"], [0x1F, 0xA3, 0x4A]),
    (&["navy", "navy blue", "dark blue", "dunkelblau", "marineblau"], [0x0B, 0x1F, 0x5C]),
    (&["light blue", "sky blue", "baby blue", "hellblau", "himmelblau"], [0x6E, 0xC6, 0xFF]),
    (&["blue", "blau"], [0x1F, 0x5F, 0xFF]),
    (&["cyan", "aqua"], [0x00, 0xE5, 0xFF]),
    (&["turquoise", "türkis", "tuerkis"], [0x1E, 0xD6, 0xC3]),
    (&["teal", "petrol"], [0x00, 0x80, 0x80]),
    (&["purple", "violet", "lila", "violett"], [0x7B, 0x2F, 0xBE]),
    (&["magenta"], [0xE0, 0x21, 0x8A]),
    (&["pink", "rosa"], [0xFF, 0x5F, 0xA2]),
    (&["white", "weiß", "weiss"], [0xF2, 0xF2, 0xF2]),
    (&["black", "schwarz"], [0x14, 0x14, 0x16]),
    (&["dark grey", "dark gray", "dunkelgrau", "gunmetal", "anthracite", "anthrazit"], [0x3A, 0x3C, 0x40]),
    (&["light grey", "light gray", "hellgrau"], [0xC8, 0xC8, 0xC8]),
    (&["grey", "gray", "grau"], [0x80, 0x80, 0x80]),
    (&["brown", "braun"], [0x6B, 0x42, 0x26]),
    (&["beige", "sand"], [0xD9, 0xC7, 0xA0]),
    (&["olive", "oliv"], [0x6B, 0x7A, 0x2B]),
];

/// German adjective endings ("rote", "blauen", "grünes").
const DE_ENDINGS: [&str; 6] = ["", "e", "en", "er", "es", "em"];

/// A colour or a finish named in a phrase.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// sRGB name and linear colour.
    Colour(&'static str, [f32; 3]),
    Finish(&'static Finish),
}

impl Paint {
    pub fn name(&self) -> &'static str {
        match self {
            Paint::Colour(n, _) => n,
            Paint::Finish(f) => f.name,
        }
    }
}

pub fn linear([r, g, b]: [u8; 3]) -> [f32; 3] {
    [srgb_to_linear(r as f32 / 255.0), srgb_to_linear(g as f32 / 255.0), srgb_to_linear(b as f32 / 255.0)]
}

/// "#ff8800", "#f80", "ff8800".
pub fn hex_colour(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        6 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8]),
        3 => Some([((v >> 8) & 15) as u8 * 17, ((v >> 4) & 15) as u8 * 17, (v & 15) as u8 * 17]),
        _ => None,
    }
}

/// A phrase as `paints_at` reads it: lower case, punctuation as spaces, single spaces, one space
/// at both ends.
pub fn norm(text: &str) -> String {
    format!(" {} ", text.to_lowercase().replace(['.', ',', '!', '?', '"', '-', ':', ';', '(', ')'], " ").split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Every colour and finish in a phrase, in the order they are said ("red and gold" → red, gold).
pub fn paints_in(text: &str) -> Vec<Paint> {
    paints_at(&norm(text)).into_iter().map(|(_, _, p)| p).collect()
}

/// The colours and finishes in a `norm`ed phrase with where they are (byte offset, length).
pub fn paints_at(t: &str) -> Vec<(usize, usize, Paint)> {
    let mut found: Vec<(usize, usize, Paint)> = Vec::new();
    let take = |at: usize, len: usize, p: Paint, found: &mut Vec<(usize, usize, Paint)>| {
        // A longer phrase over the same words wins ("dark blue" over "blue").
        if found.iter().any(|(a, l, _)| at >= *a && at + len <= a + l) {
            return;
        }
        found.retain(|(a, l, _)| !(*a >= at && a + l <= at + len));
        found.push((at, len, p));
    };
    let mut phrases: Vec<(&'static str, Paint)> = Vec::new();
    for fin in pt::FINISHES {
        for w in fin.words {
            phrases.push((w, Paint::Finish(fin)));
        }
    }
    for (names, rgb) in COLOURS {
        for n in *names {
            phrases.push((n, Paint::Colour(names[0], linear(*rgb))));
        }
    }
    // Longest first so the longer phrase claims its words before its parts do.
    phrases.sort_by_key(|(w, _)| std::cmp::Reverse(w.len()));
    for (w, paint) in phrases {
        let germanic = !w.contains(' ') && w.chars().all(|c| c.is_alphabetic());
        let endings: &[&str] = if germanic { &DE_ENDINGS } else { &[""] };
        for e in endings {
            let needle = format!(" {w}{e} ");
            let mut from = 0;
            while let Some(i) = t[from..].find(&needle) {
                let at = from + i + 1;
                take(at, needle.len() - 2, paint.clone(), &mut found);
                from = at;
            }
        }
    }
    // "#ff8800"
    for (i, word) in t.split(' ').enumerate() {
        if word.starts_with('#') {
            if let Some(rgb) = hex_colour(word) {
                let at = t.split(' ').take(i).map(|w| w.len() + 1).sum();
                take(at, word.len(), Paint::Colour("that colour", linear(rgb)), &mut found);
            }
        }
    }
    found.sort_by_key(|(a, _, _)| *a);
    found
}

// ------------------------------------------------------------------------------ scenes ----

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Box,
    RoundedBox(f32),
    Sphere,
    /// Top radius as a share of the bottom one (1: cylinder, 0: cone).
    Cylinder(f32),
    /// Segments around (a 4-sided cone is a pyramid).
    Cone(u32),
    /// Tube radius as a share of the ring radius.
    Torus(f32),
    Capsule,
    Dome,
    /// A polygon in the XY plane (unit square), extruded along Z.
    Extrude(Vec<[f32; 2]>),
    /// A profile (radius, height) in the unit square, turned around Y.
    Lathe(Vec<[f32; 2]>),
    /// A tube of `radius` along the points (already in metres; `size` is ignored).
    Tube(Vec<[f32; 3]>, f32),
    /// A flat ring in the XZ plane, the inner radius as a share of the outer one.
    Annulus(f32),
    /// Straight members (from, to, radius), in metres, as one mesh: lattices, frames, cables.
    Struts(Vec<([f32; 3], [f32; 3], f32)>),
    /// A lattice tower or girder filling its box: four chords, a ring and X braces per bay, the
    /// top as a share of the bottom (tapers), this many bays.
    Truss(f32, u32),
}

#[derive(Clone, Debug)]
pub struct ScenePart {
    pub name: String,
    pub shape: Shape,
    /// The primitive's size (it fills a 1 × 1 × 1 box before this scale).
    pub size: [f32; 3],
    pub transform: math::M4,
    pub material: String,
    /// More segments for a smooth close-up (planets).
    pub detail: u32,
}

/// A material as a scene describes it.
#[derive(Clone, Debug, Default)]
pub struct MatSpec {
    pub color: Option<[f32; 3]>,
    pub metallic: Option<f32>,
    pub roughness: Option<f32>,
    pub emissive: f32,
    pub emissive_color: Option<[f32; 3]>,
    pub alpha: Option<f32>,
    pub finish: Option<&'static Finish>,
    pub double_sided: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SceneDoc {
    pub name: String,
    pub materials: Vec<(String, MatSpec)>,
    pub parts: Vec<ScenePart>,
}

fn num(v: &Value) -> Option<f32> {
    v.as_f64().map(|f| f as f32).or_else(|| v.as_str().and_then(|s| s.trim().trim_end_matches(['m', '°']).trim().parse().ok())).filter(|f: &f32| f.is_finite())
}

fn vec3(v: &Value, default: [f32; 3]) -> [f32; 3] {
    match v {
        Value::Array(a) if a.len() >= 3 => [num(&a[0]).unwrap_or(default[0]), num(&a[1]).unwrap_or(default[1]), num(&a[2]).unwrap_or(default[2])],
        Value::Array(a) if a.len() == 2 => [num(&a[0]).unwrap_or(default[0]), num(&a[1]).unwrap_or(default[1]), default[2]],
        Value::Object(o) => [o.get("x").and_then(num).unwrap_or(default[0]), o.get("y").and_then(num).unwrap_or(default[1]), o.get("z").and_then(num).unwrap_or(default[2])],
        other => match num(other) {
            Some(k) => [k, k, k],
            None => default,
        },
    }
}

fn points2(v: &Value) -> Vec<[f32; 2]> {
    v.as_array().map(|a| a.iter().filter_map(|p| Some([num(p.get(0)?)?, num(p.get(1)?)?])).collect()).unwrap_or_default()
}

fn first<'a>(o: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| o.get(*k).filter(|v| !v.is_null()))
}

/// A colour value: "#rrggbb", "red", [r, g, b] (0-1 or 0-255). Linear.
pub fn colour_value(v: &Value) -> Option<[f32; 3]> {
    match v {
        Value::String(s) => {
            if let Some(rgb) = hex_colour(s) {
                return Some(linear(rgb));
            }
            let t = s.trim().to_lowercase();
            if let Some(inner) = t.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
                let c: Vec<f32> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if c.len() == 3 {
                    return Some(linear([c[0] as u8, c[1] as u8, c[2] as u8]));
                }
            }
            match paints_in(&t).first()? {
                Paint::Colour(_, c) => Some(*c),
                Paint::Finish(f) => f.color,
            }
        }
        Value::Array(a) if a.len() >= 3 => {
            let c: Vec<f32> = a.iter().take(3).filter_map(num).collect();
            if c.len() != 3 {
                return None;
            }
            if c.iter().any(|x| *x > 1.0) {
                Some(linear([c[0].clamp(0.0, 255.0) as u8, c[1].clamp(0.0, 255.0) as u8, c[2].clamp(0.0, 255.0) as u8]))
            } else {
                Some([srgb_to_linear(c[0].max(0.0)), srgb_to_linear(c[1].max(0.0)), srgb_to_linear(c[2].max(0.0))])
            }
        }
        _ => None,
    }
}

fn mat_spec(v: &Value) -> MatSpec {
    let mut m = MatSpec::default();
    if let Some(s) = v.as_str() {
        // A material given as one word: "gold", "red", "carbon fibre".
        match paints_in(s).first() {
            Some(Paint::Finish(f)) => m.finish = Some(f),
            Some(Paint::Colour(_, c)) => m.color = Some(*c),
            None => {}
        }
        return m;
    }
    m.color = first(v, &["color", "colour", "baseColor", "base_color", "albedo", "rgb"]).and_then(colour_value);
    m.metallic = first(v, &["metallic", "metalness", "metal"]).and_then(|x| num(x).or_else(|| x.as_bool().map(|b| if b { 1.0 } else { 0.0 }))).map(|x| x.clamp(0.0, 1.0));
    m.roughness = first(v, &["roughness", "rough"]).and_then(num).map(|x| x.clamp(0.02, 1.0));
    if let Some(e) = first(v, &["emissive", "emission", "glow"]) {
        match (num(e), colour_value(e), e.as_bool()) {
            (Some(k), _, _) => m.emissive = k.clamp(0.0, 20.0),
            (_, Some(c), _) => {
                m.emissive = 2.0;
                m.emissive_color = Some(c);
            }
            (_, _, Some(true)) => m.emissive = 2.0,
            _ => {}
        }
    }
    m.alpha = first(v, &["opacity", "alpha"]).and_then(num).map(|a| a.clamp(0.05, 1.0)).or_else(|| first(v, &["transparency"]).and_then(num).map(|t| (1.0 - t).clamp(0.05, 1.0)));
    m.finish = first(v, &["finish", "texture", "pattern", "type"]).and_then(Value::as_str).and_then(pt::finish_in);
    m.double_sided = v.get("double_sided").or_else(|| v.get("doubleSided")).and_then(Value::as_bool).unwrap_or(false);
    m
}

fn shape_of(v: &Value) -> Shape {
    let kind = first(v, &["shape", "type", "primitive", "geometry", "kind"]).and_then(Value::as_str).unwrap_or("box").to_lowercase().replace(['-', ' '], "_");
    let pts = first(v, &["points", "profile", "polygon", "outline"]).map(points2).unwrap_or_default();
    match kind.as_str() {
        "sphere" | "ball" | "ellipsoid" | "orb" | "globe" => Shape::Sphere,
        "dome" | "hemisphere" | "half_sphere" => Shape::Dome,
        "cylinder" | "tube" | "pipe" | "rod" | "disc" | "disk" | "wheel" | "can" | "column" | "pillar" => {
            Shape::Cylinder(first(v, &["top", "top_ratio", "taper"]).and_then(num).map(|t| t.clamp(0.0, 4.0)).unwrap_or(1.0))
        }
        "cone" | "spike" => Shape::Cone(first(v, &["segments", "sides"]).and_then(num).map(|s| s.clamp(3.0, 96.0) as u32).unwrap_or(40)),
        "pyramid" => Shape::Cone(4),
        "torus" | "ring" | "donut" | "doughnut" => Shape::Torus(first(v, &["thickness", "tube", "minor"]).and_then(num).map(|t| t.clamp(0.02, 1.0)).unwrap_or(0.3)),
        "capsule" | "pill" => Shape::Capsule,
        "rounded_box" | "roundedbox" | "rounded_cube" | "rounded" | "beveled_box" => Shape::RoundedBox(first(v, &["radius", "rounding", "bevel"]).and_then(num).map(|r| r.clamp(0.0, 0.5)).unwrap_or(0.18)),
        "extrude" | "extrusion" | "prism" | "polygon" | "plate_shape" if pts.len() >= 3 => Shape::Extrude(pts),
        "lathe" | "revolve" | "revolution" | "vase" if pts.len() >= 2 => Shape::Lathe(pts),
        "wedge" | "ramp" => Shape::Extrude(vec![[-0.5, -0.5], [0.5, -0.5], [-0.5, 0.5]]),
        "truss" | "lattice" | "girder" | "pylon" | "scaffold" | "lattice_tower" => {
            let top = first(v, &["top", "top_ratio", "taper"]).and_then(num).map_or(1.0, |t| t.clamp(0.05, 4.0));
            Shape::Truss(top, first(v, &["bays", "sections", "levels"]).and_then(num).map_or(6, |b| b.clamp(1.0, 64.0) as u32))
        }
        "lathe" | "revolve" | "revolution" | "vase" => Shape::Cylinder(1.0),
        _ => Shape::Box,
    }
}

/// The part's size: "size" / "scale" / "dimensions" (three numbers or one), or radius and height.
fn size_of(v: &Value) -> [f32; 3] {
    let mut s = first(v, &["size", "dimensions", "scale", "dims", "extent"]).map(|x| vec3(x, [0.2, 0.2, 0.2])).unwrap_or([0.2, 0.2, 0.2]);
    if let Some(r) = first(v, &["radius", "r"]).and_then(num) {
        if first(v, &["size", "dimensions", "scale"]).is_none() {
            s = [r * 2.0, r * 2.0, r * 2.0];
        }
    }
    if let Some(h) = first(v, &["height", "length", "h"]).and_then(num) {
        if first(v, &["size", "dimensions", "scale"]).is_none() {
            s[1] = h;
        }
    }
    s.map(|c| if c.is_finite() { c.abs().clamp(1e-4, 1e4) } else { 0.2 })
}

fn parse_parts(list: &[Value], parent: &math::M4, out: &mut Vec<ScenePart>, materials: &mut Vec<(String, MatSpec)>, depth: usize) {
    for v in list {
        if out.len() >= 400 || depth > 8 || !v.is_object() {
            continue;
        }
        let pos = first(v, &["position", "pos", "translation", "translate", "at", "location", "offset", "center", "centre"]).map(|p| vec3(p, [0.0; 3])).unwrap_or([0.0; 3]);
        let rot = first(v, &["rotation", "rot", "rotate", "orientation", "euler"]).map(|r| vec3(r, [0.0; 3])).unwrap_or([0.0; 3]);
        let local = math::mul(&math::translate(pos), &math::euler_deg(rot));
        let world = math::mul(parent, &local);
        let name = first(v, &["name", "label", "id", "part"]).and_then(Value::as_str).unwrap_or("").trim().to_string();
        if let Some(children) = first(v, &["parts", "children", "items", "group"]).and_then(Value::as_array) {
            let k = first(v, &["scale"]).filter(|_| first(v, &["shape", "type"]).is_none()).map(|s| vec3(s, [1.0; 3])).unwrap_or([1.0; 3]);
            let world = math::mul(&world, &math::scaling(k));
            parse_parts(children, &world, out, materials, depth + 1);
            continue;
        }
        // The material: a name of the scene's materials, an inline object, or a colour/finish.
        let mut material = String::new();
        match first(v, &["material", "mat", "materialName"]) {
            Some(Value::String(s)) if materials.iter().any(|(n, _)| n.eq_ignore_ascii_case(s)) => {
                material = materials.iter().find(|(n, _)| n.eq_ignore_ascii_case(s)).map(|(n, _)| n.clone()).unwrap_or_default();
            }
            Some(m) => {
                let spec = mat_spec(m);
                if spec.color.is_some() || spec.finish.is_some() || spec.emissive > 0.0 {
                    material = format!("~{}", materials.len());
                    materials.push((material.clone(), spec));
                }
            }
            None => {}
        }
        if material.is_empty() {
            if let Some(c) = first(v, &["color", "colour"]).and_then(colour_value) {
                material = format!("~{}", materials.len());
                let mut spec = MatSpec { color: Some(c), ..MatSpec::default() };
                if let Some(e) = first(v, &["emissive", "glow"]).and_then(num) {
                    spec.emissive = e;
                }
                materials.push((material.clone(), spec));
            }
        }
        let shape = shape_of(v);
        let detail = first(v, &["segments", "detail"]).and_then(num).map(|d| d.clamp(8.0, 256.0) as u32).unwrap_or(0);
        let label = if name.is_empty() { format!("part {}", out.len() + 1) } else { name };
        out.push(ScenePart { name: label, shape, size: size_of(v), transform: world, material, detail });
    }
}

/// A scene from its JSON (already parsed).
pub fn scene_from_value(v: &Value) -> Result<SceneDoc, String> {
    let root = if v.is_array() { &Value::Null } else { v };
    let list: Vec<Value> = if let Value::Array(a) = v { a.clone() } else { first(root, &["parts", "objects", "components", "meshes", "children", "elements"]).and_then(Value::as_array).cloned().unwrap_or_default() };
    let mut materials: Vec<(String, MatSpec)> = Vec::new();
    match first(root, &["materials", "mats"]) {
        Some(Value::Object(o)) => {
            for (k, m) in o {
                materials.push((k.clone(), mat_spec(m)));
            }
        }
        Some(Value::Array(a)) => {
            for (i, m) in a.iter().enumerate() {
                let name = m.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("m{i}"));
                materials.push((name, mat_spec(m)));
            }
        }
        _ => {}
    }
    let mut parts = Vec::new();
    parse_parts(&list, &math::identity(), &mut parts, &mut materials, 0);
    if parts.is_empty() {
        return Err("the scene has no parts".into());
    }
    let name = first(root, &["name", "title", "model"]).and_then(Value::as_str).unwrap_or("model").trim().to_string();
    Ok(SceneDoc { name, materials, parts })
}

/// A scene as the JSON the composer writes (what the language model refines). Members and
/// tubes have no such form and are left out.
pub fn doc_json(doc: &SceneDoc) -> Value {
    let hex = |c: [f32; 3]| {
        let s = |v: f32| (if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", s(c[0]), s(c[1]), s(c[2]))
    };
    let r = |v: f32| (v * 1000.0).round() / 1000.0;
    let materials: serde_json::Map<String, Value> = doc
        .materials
        .iter()
        .map(|(name, m)| {
            let mut o = serde_json::json!({});
            if let Some(c) = m.color {
                o["color"] = hex(c).into();
            }
            if let Some(f) = m.finish {
                o["finish"] = f.name.into();
            }
            o["metallic"] = r(m.metallic.unwrap_or(0.0)).into();
            o["roughness"] = r(m.roughness.unwrap_or(0.5)).into();
            if m.emissive > 0.0 {
                o["emissive"] = r(m.emissive).into();
            }
            (name.clone(), o)
        })
        .collect();
    let parts: Vec<Value> = doc
        .parts
        .iter()
        .filter_map(|p| {
            let points = |pts: &[[f32; 2]]| Value::from(pts.iter().map(|p| vec![r(p[0]), r(p[1])]).collect::<Vec<_>>());
            let (shape, extra): (&str, Vec<(&str, Value)>) = match &p.shape {
                Shape::Box => ("box", vec![]),
                Shape::RoundedBox(k) => ("rounded_box", vec![("radius", r(*k).into())]),
                Shape::Sphere => ("sphere", vec![]),
                Shape::Dome => ("dome", vec![]),
                Shape::Capsule => ("capsule", vec![]),
                Shape::Cylinder(top) => ("cylinder", vec![("top", r(*top).into())]),
                Shape::Cone(sides) => ("cone", vec![("segments", (*sides).into())]),
                Shape::Torus(k) => ("torus", vec![("thickness", r(*k).into())]),
                Shape::Truss(top, bays) => ("truss", vec![("top", r(*top).into()), ("bays", (*bays).into())]),
                Shape::Extrude(pts) => ("extrude", vec![("points", points(pts))]),
                Shape::Lathe(pts) => ("lathe", vec![("points", points(pts))]),
                Shape::Tube(..) | Shape::Struts(_) | Shape::Annulus(_) => return None,
            };
            let mut o = serde_json::json!({ "name": p.name, "shape": shape });
            for (k, v) in extra {
                o[k] = v;
            }
            let m = &p.transform;
            let deg = 180.0 / PI;
            o["size"] = p.size.map(r).to_vec().into();
            o["position"] = vec![r(m[12]), r(m[13]), r(m[14])].into();
            o["rotation"] = vec![r(m[6].atan2(m[10]) * deg), r((-m[2]).clamp(-1.0, 1.0).asin() * deg), r(m[1].atan2(m[0]) * deg)].into();
            o["material"] = p.material.clone().into();
            Some(o)
        })
        .collect();
    serde_json::json!({ "name": doc.name, "materials": materials, "parts": parts })
}

/// Parses a scene the model wrote: the first `{` on, cut-off output repaired.
pub fn parse_scene(text: &str) -> Result<SceneDoc, String> {
    let v = repair_json(text).ok_or("no JSON scene in the answer")?;
    scene_from_value(&v)
}

/// JSON that may have been cut off: the longest prefix that closes into valid JSON, closed.
pub fn repair_json(text: &str) -> Option<Value> {
    let start = text.find(['{', '['])?;
    let s = &text[start..];
    if let Ok(v) = serde_json::from_str::<Value>(s.trim()) {
        return Some(v);
    }
    // A valid prefix ends after a complete value: at a closing bracket, or a string's end.
    let mut stack: Vec<u8> = Vec::new();
    let (mut in_str, mut esc) = (false, false);
    let mut cuts: Vec<(usize, Vec<u8>)> = Vec::new();
    for (i, b) in s.bytes().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                stack.pop();
                if stack.is_empty() {
                    return serde_json::from_str(&s[..=i]).ok();
                }
                cuts.push((i + 1, stack.clone()));
            }
            _ => {}
        }
    }
    for (end, open) in cuts.iter().rev().take(64) {
        let mut candidate = s[..*end].trim_end().trim_end_matches(',').to_string();
        for c in open.iter().rev() {
            candidate.push(*c as char);
        }
        if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
            return Some(v);
        }
    }
    None
}

// --------------------------------------------------------------------------- materials ----

/// Applies a finish to a material: its colour (unless it keeps the model's), metal, roughness,
/// and for patterned finishes its projected textures (appended to `textures`).
pub fn apply_finish(m: &mut Material, fin: &Finish, textures: &mut Vec<crate::mesh_io::Texture>, slots: &mut HashMap<pt::Pattern, [Option<usize>; 4]>) {
    if let Some(c) = fin.color {
        m.base_color = [c[0], c[1], c[2], fin.alpha.min(1.0)];
        m.base_color_tex = None;
        m.metallic_roughness_tex = None;
        if m.triplanar {
            m.normal_tex = None;
            m.triplanar = false;
        }
    }
    if fin.metallic >= 0.0 {
        m.metallic = fin.metallic;
    }
    m.roughness = fin.roughness;
    m.alpha = if fin.alpha < 0.999 { AlphaMode::Blend } else if fin.color.is_some() { AlphaMode::Opaque } else { m.alpha };
    if fin.alpha < 0.999 {
        m.double_sided = true;
    }
    if fin.pattern != pt::Pattern::Plain {
        let idx = *slots.entry(fin.pattern).or_insert_with(|| {
            let Some(set) = pt::texture_set(fin.pattern, 1) else { return [None; 4] };
            let mut push = |t: crate::mesh_io::Texture| {
                textures.push(t);
                Some(textures.len() - 1)
            };
            [push(set.albedo.clone()), push(set.mr.clone()), push(set.normal.clone()), set.emissive.clone().and_then(&mut push)]
        });
        m.base_color_tex = idx[0];
        m.metallic_roughness_tex = idx[1];
        m.normal_tex = idx[2];
        m.normal_scale = 1.0;
        m.emissive_tex = idx[3];
        m.occlusion_tex = None;
        m.triplanar = true;
        m.tile = match fin.pattern {
            pt::Pattern::Carbon => 2.5,
            pt::Pattern::Hex => 2.0,
            pt::Pattern::Concrete | pt::Pattern::Leather => 1.6,
            _ => 1.2,
        };
        // The texture carries the metal and roughness (glTF multiplies the factors in).
        m.metallic = 1.0;
        m.roughness = 1.0;
        if fin.emissive > 0.0 {
            m.emissive = [fin.emissive; 3];
        }
    } else if fin.emissive > 0.0 {
        m.emissive = [fin.emissive; 3];
    }
}

fn material_of(name: &str, spec: &MatSpec, textures: &mut Vec<crate::mesh_io::Texture>, slots: &mut HashMap<pt::Pattern, [Option<usize>; 4]>) -> Material {
    let mut m = Material { name: name.trim_start_matches('~').to_string(), roughness: 0.5, metallic: 0.0, ..Material::default() };
    if name.starts_with('~') {
        m.name = "paint".into();
    }
    if let Some(f) = spec.finish {
        apply_finish(&mut m, f, textures, slots);
    }
    if let Some(c) = spec.color {
        // (On a patterned finish the colour tints the pattern: "red carbon".)
        let a = m.base_color[3];
        m.base_color = [c[0], c[1], c[2], a];
    }
    if let Some(x) = spec.metallic {
        if !m.triplanar {
            m.metallic = x;
        }
    }
    if let Some(x) = spec.roughness {
        if !m.triplanar {
            m.roughness = x;
        }
    }
    if spec.emissive > 0.0 {
        let c = spec.emissive_color.or(spec.color).unwrap_or([1.0; 3]);
        m.emissive = [c[0] * spec.emissive, c[1] * spec.emissive, c[2] * spec.emissive];
    }
    if let Some(a) = spec.alpha {
        m.base_color[3] = a;
        if a < 0.999 {
            m.alpha = AlphaMode::Blend;
            m.double_sided = true;
        }
    }
    m.double_sided |= spec.double_sided;
    m
}

/// The model a scene describes (each part its own named mesh).
pub fn build(doc: &SceneDoc) -> Model {
    let mut model = Model { name: doc.name.clone(), unit_m: 1.0, ..Model::default() };
    let mut slots = HashMap::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (name, spec) in &doc.materials {
        let m = material_of(name, spec, &mut model.textures, &mut slots);
        index.insert(name.to_lowercase(), model.materials.len());
        model.materials.push(m);
    }
    let fallback = model.materials.len();
    model.materials.push(Material { name: "default".into(), base_color: [0.6, 0.62, 0.66, 1.0], metallic: 0.6, roughness: 0.35, ..Material::default() });
    for sp in &doc.parts {
        let mut part = mesh(&sp.shape, sp.size, sp.detail);
        part.name = sp.name.clone();
        part.material = index.get(&sp.material.to_lowercase()).copied().unwrap_or(fallback);
        transform_part(&mut part, &sp.transform);
        model.parts.push(part);
    }
    model.finish();
    model
}

pub fn transform_part(part: &mut Part, m: &math::M4) {
    let nm = math::normal_matrix(m);
    for p in &mut part.positions {
        *p = math::point(m, *p);
    }
    for n in &mut part.normals {
        *n = math::normalize(math::dir(&nm, *n));
    }
    for t in &mut part.tangents {
        let d = math::normalize(math::dir(m, [t[0], t[1], t[2]]));
        *t = [d[0], d[1], d[2], t[3]];
    }
    if math::det3(m) < 0.0 {
        for t in part.indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
}

// ------------------------------------------------------------------------------ meshes ----

struct Builder {
    part: Part,
}

impl Builder {
    fn new() -> Self {
        Builder { part: Part::default() }
    }

    fn vertex(&mut self, p: [f32; 3], n: [f32; 3], uv: [f32; 2]) -> u32 {
        self.part.positions.push(p);
        self.part.normals.push(n);
        self.part.uvs.push(uv);
        (self.part.positions.len() - 1) as u32
    }

    fn tri(&mut self, a: u32, b: u32, c: u32) {
        self.part.indices.extend_from_slice(&[a, b, c]);
    }

    /// A `cols` × `rows` grid of vertices already pushed from `base`, row by row.
    fn grid(&mut self, base: u32, cols: u32, rows: u32) {
        for r in 0..rows - 1 {
            for c in 0..cols - 1 {
                let i = base + r * cols + c;
                self.tri(i, i + cols, i + 1);
                self.tri(i + 1, i + cols, i + cols + 1);
            }
        }
    }
}

/// The primitive's mesh at `size` (it fills a 1 × 1 × 1 box before the scale; round shapes are
/// built at their real size so they stay round).
pub fn mesh(shape: &Shape, size: [f32; 3], detail: u32) -> Part {
    let mut b = Builder::new();
    let seg = if detail > 0 { detail } else { 48 };
    match shape {
        Shape::Box => return rounded_box(size, 0.0),
        Shape::RoundedBox(r) => return rounded_box(size, r * size[0].min(size[1]).min(size[2])),
        Shape::Sphere => {
            let (cols, rows) = (seg + 1, seg / 2 + 1);
            for r in 0..rows {
                let v = r as f32 / (rows - 1) as f32;
                for c in 0..cols {
                    let u = c as f32 / (cols - 1) as f32;
                    let d = pt::sphere_dir(u, v);
                    b.vertex([d[0] * 0.5 * size[0], d[1] * 0.5 * size[1], d[2] * 0.5 * size[2]], math::normalize([d[0] / size[0], d[1] / size[1], d[2] / size[2]]), [u, v]);
                    let lon = (u - 0.5) * TAU;
                    b.part.tangents.push([lon.cos(), 0.0, -lon.sin(), 1.0]);
                }
            }
            // Rows run from the north pole down, columns eastwards: (down × east) faces out.
            b.grid(0, cols, rows);
        }
        Shape::Dome => {
            let prof: Vec<[f32; 2]> = (0..=12).map(|k| {
                let a = k as f32 / 12.0 * PI / 2.0;
                [0.5 * a.cos(), a.sin() - 0.5]
            }).collect();
            return lathe(&prof, size, seg, true);
        }
        Shape::Cylinder(top) => {
            let prof = vec![[0.0, -0.5], [0.5, -0.5], [0.5 * top, 0.5], [0.0, 0.5]];
            return lathe(&prof, size, seg, false);
        }
        Shape::Cone(sides) => {
            let prof = vec![[0.0, -0.5], [0.5, -0.5], [0.0, 0.5]];
            let mut p = lathe(&prof, size, *sides, false);
            if *sides <= 6 {
                flat_shade(&mut p);
            }
            return p;
        }
        Shape::Torus(k) => {
            let big = 0.5 / (1.0 + k);
            let small = big * k;
            let (cols, rows) = (seg + 1, (seg / 2).max(12) + 1);
            for r in 0..rows {
                let v = r as f32 / (rows - 1) as f32;
                let phi = v * TAU;
                for c in 0..cols {
                    let u = c as f32 / (cols - 1) as f32;
                    let theta = u * TAU;
                    let (ct, st, cp, sp) = (theta.cos(), theta.sin(), phi.cos(), phi.sin());
                    let p = [(big + small * cp) * ct, small * sp, (big + small * cp) * st];
                    let n = [cp * ct, sp, cp * st];
                    b.vertex([p[0] * size[0], p[1] * size[1], p[2] * size[2]], math::normalize([n[0] / size[0], n[1] / size[1], n[2] / size[2]]), [u, v]);
                }
            }
            // (v around the tube, u around the ring: (v × u) faces out.)
            b.grid(0, cols, rows);
        }
        Shape::Capsule => {
            let r = 0.5 * size[0].min(size[2]);
            let h = size[1].max(2.0 * r);
            let half = (h / 2.0 - r).max(0.0);
            let mut prof: Vec<[f32; 2]> = Vec::new();
            for k in 0..=10 {
                let a = -PI / 2.0 + k as f32 / 10.0 * PI / 2.0;
                prof.push([r * a.cos(), -half + r * a.sin()]);
            }
            for k in 0..=10 {
                let a = k as f32 / 10.0 * PI / 2.0;
                prof.push([r * a.cos(), half + r * a.sin()]);
            }
            let mut part = lathe(&prof, [1.0, 1.0, 1.0], seg, true);
            let (sx, sz) = (size[0] / (2.0 * r), size[2] / (2.0 * r));
            for p in &mut part.positions {
                p[0] *= sx;
                p[2] *= sz;
            }
            return part;
        }
        Shape::Extrude(points) => return extrude(points, size),
        Shape::Lathe(points) => {
            let prof: Vec<[f32; 2]> = points.iter().map(|p| [p[0].abs().min(0.5), p[1].clamp(-0.5, 0.5)]).collect();
            return lathe(&prof, size, seg, true);
        }
        Shape::Tube(path, radius) => return tube(path, *radius, 12, false),
        Shape::Struts(list) => return struts(list, if detail >= 96 { 8 } else if detail >= 64 { 6 } else { 4 }),
        Shape::Truss(top, bays) => {
            let bays = (*bays).clamp(1, 64);
            let sections: Vec<([f32; 3], f32, f32)> = (0..=bays).map(|k| {
                let t = k as f32 / bays as f32;
                let k = 1.0 + (top - 1.0) * t;
                ([0.0, size[1] * (t - 0.5), 0.0], 0.5 * size[0] * k, 0.5 * size[2] * k)
            }).collect();
            let r = 0.035 * size[0].min(size[2]).max(size[1] / 60.0);
            return struts(&lattice(&sections, r, r * 0.6), 4);
        }
        Shape::Annulus(inner) => {
            let cols = seg.max(64) + 1;
            for side in 0..2 {
                let base = b.part.positions.len() as u32;
                let up = if side == 0 { 1.0 } else { -1.0 };
                for c in 0..cols {
                    let a = c as f32 / (cols - 1) as f32 * TAU;
                    for (k, rad) in [0.5 * inner, 0.5].iter().enumerate() {
                        b.vertex([rad * a.cos() * size[0], 0.0, rad * a.sin() * size[2]], [0.0, up, 0.0], [k as f32, c as f32 / (cols - 1) as f32]);
                    }
                }
                for c in 0..cols - 1 {
                    let i = base + c * 2;
                    if side == 0 {
                        b.tri(i, i + 2, i + 1);
                        b.tri(i + 1, i + 2, i + 3);
                    } else {
                        b.tri(i, i + 1, i + 2);
                        b.tri(i + 1, i + 3, i + 2);
                    }
                }
            }
        }
    }
    b.part
}

/// Faceted shading (a pyramid's faces stay flat).
fn flat_shade(part: &mut Part) {
    crate::mesh_io::flat_normals(part);
}

/// A box with rounded edges of `radius` (0: sharp), centred, faces subdivided where they curve.
pub fn rounded_box(size: [f32; 3], radius: f32) -> Part {
    let h = [size[0] / 2.0, size[1] / 2.0, size[2] / 2.0];
    let r = radius.clamp(0.0, h[0].min(h[1]).min(h[2]));
    let band = if r > 0.0 { 5 } else { 0 };
    // Coordinates along an axis: the rounded band at both ends spaced by angle, flat between.
    let coords = |hh: f32| -> Vec<f32> {
        let inner = hh - r;
        let mut v: Vec<f32> = (1..=band).rev().map(|k| -inner - r * (k as f32 / band as f32 * PI / 4.0).tan()).collect();
        v.push(-inner);
        v.push(inner);
        v.extend((1..=band).map(|k| inner + r * (k as f32 / band as f32 * PI / 4.0).tan()));
        v
    };
    let mut b = Builder::new();
    // (normal axis, sign, u axis, v axis) with u × v = normal.
    let faces: [(usize, f32, usize, usize); 6] = [(0, 1.0, 2, 1), (0, -1.0, 1, 2), (1, 1.0, 0, 2), (1, -1.0, 2, 0), (2, 1.0, 1, 0), (2, -1.0, 0, 1)];
    for (axis, sign, ua, va) in faces {
        // Keep u × v = sign × axis by flipping u on the negative faces (handled by the order).
        let (us, vs) = (coords(h[ua]), coords(h[va]));
        let base = b.part.positions.len() as u32;
        for (j, &cv) in vs.iter().enumerate() {
            for (i, &cu) in us.iter().enumerate() {
                let mut q = [0.0f32; 3];
                q[axis] = sign * h[axis];
                q[ua] = cu;
                q[va] = cv;
                let inner = [q[0].clamp(-(h[0] - r), h[0] - r), q[1].clamp(-(h[1] - r), h[1] - r), q[2].clamp(-(h[2] - r), h[2] - r)];
                let d = math::sub(q, inner);
                let mut n = [0.0f32; 3];
                n[axis] = sign;
                let (p, n) = if r > 0.0 && math::length(d) > 1e-9 {
                    let dir = math::normalize(d);
                    (math::add(inner, math::scale(dir, r)), dir)
                } else {
                    (q, n)
                };
                b.vertex(p, n, [i as f32 / (us.len() - 1) as f32, 1.0 - j as f32 / (vs.len() - 1) as f32]);
            }
        }
        let (cols, rows) = (us.len() as u32, vs.len() as u32);
        let start = b.part.indices.len();
        b.grid(base, cols, rows);
        // The grid winds u → v; flip where u × v points inwards.
        let mut un = [0.0f32; 3];
        un[ua] = 1.0;
        let mut vn = [0.0f32; 3];
        vn[va] = 1.0;
        let mut out = [0.0f32; 3];
        out[axis] = sign;
        if math::dot(math::cross(un, vn), out) > 0.0 {
            for t in b.part.indices[start..].chunks_exact_mut(3) {
                t.swap(1, 2);
            }
        }
    }
    b.part
}

/// A surface of revolution around Y from a (radius, height) profile, scaled by `size`.
/// `smooth`: normals shared along the profile (else each segment of the profile is flat).
pub fn lathe(profile: &[[f32; 2]], size: [f32; 3], segments: u32, smooth: bool) -> Part {
    let mut b = Builder::new();
    let cols = segments.max(3) + 1;
    let n = profile.len();
    if n < 2 {
        return b.part;
    }
    // Per profile segment: its outward 2D normal (radius grows outward, profile runs upward).
    let seg_normal = |i: usize| -> [f32; 2] {
        let (a, c) = (profile[i], profile[i + 1]);
        let (dr, dy) = (c[0] - a[0], c[1] - a[1]);
        let l = (dr * dr + dy * dy).sqrt().max(1e-9);
        [dy / l, -dr / l]
    };
    let total: f32 = (0..n - 1).map(|i| ((profile[i + 1][0] - profile[i][0]).powi(2) + (profile[i + 1][1] - profile[i][1]).powi(2)).sqrt()).sum::<f32>().max(1e-6);
    let emit = |b: &mut Builder, rows: &[([f32; 2], [f32; 2], f32)]| {
        let base = b.part.positions.len() as u32;
        for (p, n2, v) in rows {
            for c in 0..cols {
                let u = c as f32 / (cols - 1) as f32;
                let a = u * TAU;
                let (ca, sa) = (a.cos(), a.sin());
                let pos = [p[0] * ca * size[0], p[1] * size[1], p[0] * sa * size[2]];
                let nrm = math::normalize([n2[0] * ca / size[0].max(1e-6), n2[1] / size[1].max(1e-6), n2[0] * sa / size[2].max(1e-6)]);
                b.vertex(pos, nrm, [u, 1.0 - v]);
            }
        }
        // (Along the profile × around from +X to +Z faces out for a profile drawn upwards.)
        b.grid(base, cols, rows.len() as u32);
    };
    let mut along = 0.0f32;
    let mut lengths = vec![0.0f32; n];
    for i in 1..n {
        along += ((profile[i][0] - profile[i - 1][0]).powi(2) + (profile[i][1] - profile[i - 1][1]).powi(2)).sqrt();
        lengths[i] = along / total;
    }
    if smooth {
        let rows: Vec<([f32; 2], [f32; 2], f32)> = (0..n)
            .map(|i| {
                let nn = match i {
                    0 => seg_normal(0),
                    _ if i == n - 1 => seg_normal(n - 2),
                    _ => {
                        let (a, c) = (seg_normal(i - 1), seg_normal(i));
                        let s = [a[0] + c[0], a[1] + c[1]];
                        let l = (s[0] * s[0] + s[1] * s[1]).sqrt().max(1e-9);
                        [s[0] / l, s[1] / l]
                    }
                };
                (profile[i], nn, lengths[i])
            })
            .collect();
        emit(&mut b, &rows);
    } else {
        for i in 0..n - 1 {
            let nn = seg_normal(i);
            emit(&mut b, &[(profile[i], nn, lengths[i]), (profile[i + 1], nn, lengths[i + 1])]);
        }
    }
    b.part
}

/// Ear clipping of a simple polygon (counter-clockwise); triangle corner indices.
fn triangulate(poly: &[[f32; 2]]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    let mut out = Vec::new();
    let cross = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    let inside = |p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]| cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0;
    let mut guard = 0;
    while idx.len() > 3 && guard < 10_000 {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        for i in 0..n {
            let (ia, ib, ic) = (idx[(i + n - 1) % n], idx[i], idx[(i + 1) % n]);
            let (a, b, c) = (poly[ia], poly[ib], poly[ic]);
            if cross(a, b, c) <= 1e-12 {
                continue;
            }
            if idx.iter().any(|&k| k != ia && k != ib && k != ic && inside(poly[k], a, b, c)) {
                continue;
            }
            out.push([ia, ib, ic]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            // Not simple: fan the rest.
            for k in 1..idx.len() - 1 {
                out.push([idx[0], idx[k], idx[k + 1]]);
            }
            return out;
        }
    }
    if idx.len() == 3 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    out
}

/// A polygon (XY, unit square) extruded along Z by `size[2]`, scaled to `size`.
pub fn extrude(points: &[[f32; 2]], size: [f32; 3]) -> Part {
    let mut poly: Vec<[f32; 2]> = points.iter().map(|p| [p[0] * size[0], p[1] * size[1]]).collect();
    poly.dedup();
    if poly.len() > 2 && poly.first() == poly.last() {
        poly.pop();
    }
    let area: f32 = (0..poly.len()).map(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        a[0] * b[1] - b[0] * a[1]
    }).sum();
    if area < 0.0 {
        poly.reverse();
    }
    let mut b = Builder::new();
    if poly.len() < 3 {
        return b.part;
    }
    let hz = size[2] / 2.0;
    let (min, max) = poly.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
    let uv = |p: [f32; 2]| [(p[0] - min[0]) / (max[0] - min[0]).max(1e-6), 1.0 - (p[1] - min[1]) / (max[1] - min[1]).max(1e-6)];
    let tris = triangulate(&poly);
    for (z, nz) in [(hz, 1.0f32), (-hz, -1.0)] {
        let base = b.part.positions.len() as u32;
        for p in &poly {
            b.vertex([p[0], p[1], z], [0.0, 0.0, nz], uv(*p));
        }
        for t in &tris {
            let (x, y, w) = (base + t[0] as u32, base + t[1] as u32, base + t[2] as u32);
            if nz > 0.0 {
                b.tri(x, y, w);
            } else {
                b.tri(x, w, y);
            }
        }
    }
    let mut run = 0.0f32;
    let perimeter: f32 = (0..poly.len()).map(|i| {
        let (a, c) = (poly[i], poly[(i + 1) % poly.len()]);
        ((c[0] - a[0]).powi(2) + (c[1] - a[1]).powi(2)).sqrt()
    }).sum::<f32>().max(1e-6);
    for i in 0..poly.len() {
        let (a, c) = (poly[i], poly[(i + 1) % poly.len()]);
        let len = ((c[0] - a[0]).powi(2) + (c[1] - a[1]).powi(2)).sqrt();
        let n = math::normalize([c[1] - a[1], -(c[0] - a[0]), 0.0]);
        let (u0, u1) = (run / perimeter, (run + len) / perimeter);
        run += len;
        let base = b.part.positions.len() as u32;
        b.vertex([a[0], a[1], hz], n, [u0, 0.0]);
        b.vertex([c[0], c[1], hz], n, [u1, 0.0]);
        b.vertex([c[0], c[1], -hz], n, [u1, 1.0]);
        b.vertex([a[0], a[1], -hz], n, [u0, 1.0]);
        b.tri(base, base + 3, base + 2);
        b.tri(base, base + 2, base + 1);
    }
    b.part
}

/// A tube of `radius` along a path (parallel-transport frames), optionally closed into a loop.
pub fn tube(path: &[[f32; 3]], radius: f32, sides: u32, closed: bool) -> Part {
    let mut b = Builder::new();
    let n = path.len();
    if n < 2 {
        return b.part;
    }
    let tangent = |i: usize| -> [f32; 3] {
        let (a, c) = if closed { (path[(i + n - 1) % n], path[(i + 1) % n]) } else { (path[i.saturating_sub(1)], path[(i + 1).min(n - 1)]) };
        math::normalize(math::sub(c, a))
    };
    let t0 = tangent(0);
    let helper = if t0[1].abs() < 0.9 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
    let mut normal = math::normalize(math::cross(math::cross(t0, helper), t0));
    let rows = if closed { n + 1 } else { n };
    let cols = sides + 1;
    for r in 0..rows {
        let i = r % n;
        let t = tangent(i);
        // Carry the frame along: remove the part of the normal along the new tangent.
        normal = math::normalize(math::sub(normal, math::scale(t, math::dot(normal, t))));
        let bin = math::cross(t, normal);
        for c in 0..cols {
            let a = c as f32 / sides as f32 * TAU;
            let d = math::add(math::scale(normal, a.cos()), math::scale(bin, a.sin()));
            b.vertex(math::add(path[i], math::scale(d, radius)), d, [r as f32 / (rows - 1) as f32, c as f32 / sides as f32]);
        }
    }
    b.grid(0, cols, rows as u32);
    // Rows along the path, columns around: check the winding on the first quad.
    if b.part.indices.len() >= 3 {
        let (ia, ib, ic) = (b.part.indices[0] as usize, b.part.indices[1] as usize, b.part.indices[2] as usize);
        let f = math::cross(math::sub(b.part.positions[ib], b.part.positions[ia]), math::sub(b.part.positions[ic], b.part.positions[ia]));
        if math::dot(f, b.part.normals[ia]) < 0.0 {
            for t in b.part.indices.chunks_exact_mut(3) {
                t.swap(1, 2);
            }
        }
    }
    b.part
}

/// Straight members as one mesh (open prisms of `sides` faces).
pub fn struts(list: &[([f32; 3], [f32; 3], f32)], sides: u32) -> Part {
    let mut out = Builder::new().part;
    for &(a, b, r) in list {
        if math::length(math::sub(b, a)) < 1e-5 || r <= 0.0 {
            continue;
        }
        let p = tube(&[a, b], r, sides, false);
        let base = out.positions.len() as u32;
        out.positions.extend(p.positions);
        out.normals.extend(p.normals);
        out.uvs.extend(p.uvs);
        out.indices.extend(p.indices.iter().map(|i| i + base));
    }
    out
}

/// A square lattice through its sections (centre, half-width x, half-width z), bottom to top:
/// the corner chords (radius `chord`), a ring at every section and X braces on every face of
/// every bay (radius `brace`).
pub fn lattice(sections: &[([f32; 3], f32, f32)], chord: f32, brace: f32) -> Vec<([f32; 3], [f32; 3], f32)> {
    let corners = |&(c, hx, hz): &([f32; 3], f32, f32)| [[c[0] - hx, c[1], c[2] - hz], [c[0] + hx, c[1], c[2] - hz], [c[0] + hx, c[1], c[2] + hz], [c[0] - hx, c[1], c[2] + hz]];
    let mut out = Vec::new();
    for (i, s) in sections.iter().enumerate() {
        let lo = corners(s);
        for k in 0..4 {
            out.push((lo[k], lo[(k + 1) % 4], brace));
        }
        let Some(next) = sections.get(i + 1) else { continue };
        let hi = corners(next);
        for k in 0..4 {
            let j = (k + 1) % 4;
            out.push((lo[k], hi[k], chord));
            out.push((lo[k], hi[j], brace));
            out.push((lo[j], hi[k], brace));
        }
    }
    out
}

// -------------------------------------------------------------------------- the library ----

/// One built-in model: the words that ask for it and how it is made.
pub struct Entry {
    pub key: &'static str,
    /// Lower-case words or phrases (English and German).
    pub words: &'static [&'static str],
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Planet(Planet),
    Scene(fn() -> SceneDoc),
    /// A model made at a detail level (1 = the first look, up to `MAX_LEVEL`).
    Detailed(fn(u32) -> SceneDoc),
}

/// The finest level a `Kind::Detailed` model is made at.
pub const MAX_LEVEL: u32 = 4;

impl Kind {
    /// The scene of a built-in model (a planet has none), at a detail level.
    pub fn scene(self, level: u32) -> Option<SceneDoc> {
        match self {
            Kind::Planet(_) => None,
            Kind::Scene(f) => Some(f()),
            Kind::Detailed(f) => Some(f(level.clamp(1, MAX_LEVEL))),
        }
    }
}

/// The Eiffel Tower at real size (330 m): four curved lattice legs with their arches up to the
/// second floor, one lattice shaft to the top, the three floors and the antenna. Each level
/// doubles the bays and adds the inner bracing.
fn eiffel_tower(level: u32) -> SceneDoc {
    // The outer half-width of the tower at a height (m), from its drawings.
    const PROFILE: [(f32, f32); 7] = [(0.0, 62.5), (57.6, 37.0), (115.7, 20.5), (180.0, 12.6), (240.0, 9.6), (276.1, 8.3), (300.0, 3.2)];
    let half = |y: f32| -> f32 {
        let i = PROFILE.windows(2).position(|w| y <= w[1].0).unwrap_or(PROFILE.len() - 2);
        let ((y0, w0), (y1, w1)) = (PROFILE[i], PROFILE[i + 1]);
        w0 + (w1 - w0) * ((y - y0) / (y1 - y0)).clamp(0.0, 1.0)
    };
    let l = level.clamp(1, MAX_LEVEL) as f32;
    let (chord, brace) = (1.5 / l.sqrt(), 0.8 / l.sqrt());
    let bays = |n: f32| (n * l) as u32;
    let mut parts = Vec::new();
    let strut = |name: &str, list: Vec<([f32; 3], [f32; 3], f32)>, mat: &str| ScenePart { name: name.into(), shape: Shape::Struts(list), size: [1.0; 3], transform: math::identity(), material: mat.into(), detail: 32 + 16 * level };
    // The legs: square box girders leaning in along the diagonals, 26 m wide at the ground.
    let second = 115.7;
    for (name, sx, sz) in [("north leg", 1.0, 1.0), ("east leg", 1.0, -1.0), ("south leg", -1.0, -1.0), ("west leg", -1.0, 1.0)] {
        let n = bays(6.0);
        let sections: Vec<([f32; 3], f32, f32)> = (0..=n).map(|k| {
            let y = second * k as f32 / n as f32;
            let h = 13.0 + (5.0 - 13.0) * y / second;
            let c = half(y) - h;
            ([sx * c, y, sz * c], h, h)
        }).collect();
        let mut list = lattice(&sections, chord, brace);
        if level >= 3 {
            // Inner diagonals of every leg's cross-section.
            for &(c, h, _) in &sections {
                list.push(([c[0] - h, c[1], c[2] - h], [c[0] + h, c[1], c[2] + h], brace * 0.8));
            }
        }
        parts.push(strut(name, list, "iron"));
    }
    // The arches under the first floor, one on each face.
    for (name, axis_x, s) in [("north arch", false, 1.0), ("south arch", false, -1.0), ("east arch", true, 1.0), ("west arch", true, -1.0)] {
        let steps = bays(12.0);
        let mut list = Vec::new();
        let point = |t: f32, r: f32| -> [f32; 3] {
            let (a, b) = (36.5 * r, 39.5 * r);
            let (x, y) = (a * (std::f32::consts::PI * t).cos(), b * (std::f32::consts::PI * t).sin());
            let face = half(y) - 1.0;
            if axis_x { [s * face, y, x] } else { [x, y, s * face] }
        };
        for k in 0..steps {
            let (t0, t1) = (k as f32 / steps as f32, (k + 1) as f32 / steps as f32);
            list.push((point(t0, 1.0), point(t1, 1.0), chord));
            list.push((point(t0, 0.9), point(t1, 0.9), brace));
            list.push((point(t0, 1.0), point(t0, 0.9), brace));
        }
        parts.push(strut(name, list, "iron"));
    }
    // The shaft: the joined legs from the second floor to the top floor, then the summit.
    let shaft = |from: f32, to: f32, n: u32| -> Vec<([f32; 3], f32, f32)> {
        (0..=n).map(|k| {
            let y = from + (to - from) * k as f32 / n as f32;
            let h = half(y);
            ([0.0, y, 0.0], h, h)
        }).collect()
    };
    let mut upper = lattice(&shaft(second, 276.1, bays(8.0)), chord, brace);
    if level >= 2 {
        // The inner columns of the joined legs.
        upper.extend(lattice(&shaft(second, 276.1, bays(8.0)).into_iter().map(|(c, h, _)| (c, h * 0.55, h * 0.55)).collect::<Vec<_>>(), brace, brace * 0.7));
    }
    parts.push(strut("upper tower", upper, "iron"));
    parts.push(strut("summit", lattice(&shaft(276.1, 300.0, bays(2.0)), chord, brace), "iron"));
    // The floors.
    let floor = |name: &str, y: f32, side: f32, height: f32, open: f32| -> Vec<ScenePart> {
        if open <= 0.0 {
            return vec![part(name, Shape::Box, [side, height, side], [0.0, y, 0.0], [0.0; 3], "deck")];
        }
        let band = (side - open) / 2.0;
        let off = (side - band) / 2.0;
        vec![
            part(&format!("{name} north"), Shape::Box, [side, height, band], [0.0, y, off], [0.0; 3], "deck"),
            part(&format!("{name} south"), Shape::Box, [side, height, band], [0.0, y, -off], [0.0; 3], "deck"),
            part(&format!("{name} east"), Shape::Box, [band, height, open], [off, y, 0.0], [0.0; 3], "deck"),
            part(&format!("{name} west"), Shape::Box, [band, height, open], [-off, y, 0.0], [0.0; 3], "deck"),
        ]
    };
    parts.extend(floor("first floor", 57.6, 2.0 * half(57.6) + 6.0, 6.0, 2.0 * half(57.6) - 18.0));
    parts.extend(floor("second floor", second, 2.0 * half(second) + 4.0, 3.5, 0.0));
    parts.extend(floor("top floor", 278.0, 2.0 * half(278.0) + 2.0, 7.0, 0.0));
    if level >= 2 {
        // Railings round the first and second floors.
        for (y, w) in [(57.6 + 4.2, half(57.6) + 3.0), (second + 2.6, half(second) + 2.0)] {
            let ring = [[-w, y, -w], [w, y, -w], [w, y, w], [-w, y, w]];
            parts.push(strut(&format!("railing at {y:.0} m"), (0..4).map(|k| (ring[k], ring[(k + 1) % 4], 0.35)).collect(), "iron"));
        }
    }
    parts.push(part("lantern", Shape::Cylinder(0.7), [5.0, 8.0, 5.0], [0.0, 304.0, 0.0], [0.0; 3], "iron"));
    parts.push(part("antenna", Shape::Cylinder(0.3), [1.6, 22.0, 1.6], [0.0, 319.0, 0.0], [0.0; 3], "iron"));
    parts.push(part("beacon", Shape::Sphere, [2.4, 2.4, 2.4], [0.0, 330.0, 0.0], [0.0; 3], "beacon"));
    doc("Eiffel Tower", vec![("iron", mat("#6F5A45", 0.55, 0.5)), ("deck", mat("#5C4A3A", 0.4, 0.6)), ("beacon", glow("#FFB347", 3.0))], parts)
}

fn part(name: &str, shape: Shape, size: [f32; 3], pos: [f32; 3], rot: [f32; 3], material: &str) -> ScenePart {
    ScenePart { name: name.into(), shape, size, transform: math::mul(&math::translate(pos), &math::euler_deg(rot)), material: material.into(), detail: 0 }
}

fn mat(color: &str, metallic: f32, roughness: f32) -> MatSpec {
    MatSpec { color: hex_colour(color).map(linear), metallic: Some(metallic), roughness: Some(roughness), ..MatSpec::default() }
}

fn glow(color: &str, k: f32) -> MatSpec {
    MatSpec { color: hex_colour(color).map(linear), metallic: Some(0.0), roughness: Some(0.3), emissive: k, ..MatSpec::default() }
}

fn doc(name: &str, materials: Vec<(&str, MatSpec)>, parts: Vec<ScenePart>) -> SceneDoc {
    SceneDoc { name: name.into(), materials: materials.into_iter().map(|(n, m)| (n.to_string(), m)).collect(), parts }
}

/// Both sides of a body: the part at +x as "left …" (the suit's own left) and mirrored.
fn pair(out: &mut Vec<ScenePart>, name: &str, shape: Shape, size: [f32; 3], pos: [f32; 3], rot: [f32; 3], material: &str) {
    out.push(part(&format!("left {name}"), shape.clone(), size, pos, rot, material));
    out.push(part(&format!("right {name}"), shape, size, [-pos[0], pos[1], pos[2]], [rot[0], -rot[1], -rot[2]], material));
}

fn iron_suit() -> SceneDoc {
    let mut p = Vec::new();
    let rb = |r: f32| Shape::RoundedBox(r);
    pair(&mut p, "boot", rb(0.3), [0.13, 0.12, 0.28], [0.12, 0.06, 0.03], [0.0; 3], "red");
    pair(&mut p, "shin", Shape::Capsule, [0.12, 0.42, 0.13], [0.12, 0.33, 0.0], [0.0; 3], "gold");
    pair(&mut p, "knee", Shape::Sphere, [0.13, 0.13, 0.13], [0.12, 0.55, 0.015], [0.0; 3], "red");
    pair(&mut p, "thigh", Shape::Capsule, [0.16, 0.42, 0.17], [0.12, 0.77, 0.0], [0.0; 3], "red");
    p.push(part("pelvis", rb(0.35), [0.36, 0.16, 0.22], [0.0, 1.0, 0.0], [0.0; 3], "red"));
    p.push(part("abdomen", rb(0.35), [0.3, 0.18, 0.19], [0.0, 1.15, 0.0], [0.0; 3], "gold"));
    p.push(part("chest", rb(0.3), [0.44, 0.3, 0.26], [0.0, 1.37, 0.0], [0.0; 3], "red"));
    p.push(part("arc reactor", Shape::Cylinder(1.0), [0.085, 0.02, 0.085], [0.0, 1.4, 0.13], [90.0, 0.0, 0.0], "reactor"));
    p.push(part("reactor ring", Shape::Torus(0.22), [0.12, 0.03, 0.12], [0.0, 1.4, 0.128], [90.0, 0.0, 0.0], "silver"));
    pair(&mut p, "shoulder", Shape::Sphere, [0.17, 0.15, 0.17], [0.29, 1.47, 0.0], [0.0; 3], "red");
    pair(&mut p, "upper arm", Shape::Capsule, [0.11, 0.3, 0.11], [0.31, 1.29, 0.0], [0.0, 0.0, -4.0], "red");
    pair(&mut p, "elbow", Shape::Sphere, [0.1, 0.1, 0.1], [0.32, 1.13, 0.0], [0.0; 3], "gold");
    pair(&mut p, "forearm", Shape::Capsule, [0.105, 0.28, 0.105], [0.33, 0.98, 0.0], [0.0, 0.0, -3.0], "gold");
    pair(&mut p, "hand", rb(0.3), [0.06, 0.12, 0.09], [0.34, 0.8, 0.0], [0.0; 3], "red");
    pair(&mut p, "repulsor", Shape::Cylinder(1.0), [0.045, 0.01, 0.045], [0.31, 0.8, 0.0], [0.0, 0.0, 90.0], "reactor");
    p.push(part("neck", Shape::Cylinder(1.0), [0.11, 0.07, 0.11], [0.0, 1.55, 0.0], [0.0; 3], "dark"));
    p.push(part("helmet", rb(0.42), [0.22, 0.27, 0.25], [0.0, 1.7, -0.01], [0.0; 3], "red"));
    p.push(part("faceplate", rb(0.3), [0.175, 0.2, 0.045], [0.0, 1.68, 0.108], [0.0; 3], "gold"));
    pair(&mut p, "eye", Shape::Box, [0.05, 0.012, 0.012], [0.04, 1.72, 0.131], [0.0, 0.0, -8.0], "eyes");
    doc(
        "Iron Man suit",
        vec![("red", mat("#9B111E", 0.85, 0.3)), ("gold", mat("#D4A52A", 1.0, 0.28)), ("silver", mat("#C8CCD2", 1.0, 0.2)), ("reactor", glow("#BFF4FF", 4.0)), ("eyes", glow("#E8FBFF", 5.0)), ("dark", mat("#2A2D33", 0.6, 0.5))],
        p,
    )
}

fn robot() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("body", Shape::RoundedBox(0.1), [0.5, 0.6, 0.35], [0.0, 0.85, 0.0], [0.0; 3], "steel"));
    p.push(part("chest panel", Shape::RoundedBox(0.2), [0.3, 0.2, 0.03], [0.0, 0.92, 0.18], [0.0; 3], "dark"));
    p.push(part("heart light", Shape::Cylinder(1.0), [0.07, 0.02, 0.07], [0.0, 0.92, 0.2], [90.0, 0.0, 0.0], "glow"));
    p.push(part("head", Shape::RoundedBox(0.2), [0.34, 0.26, 0.3], [0.0, 1.35, 0.0], [0.0; 3], "steel"));
    pair(&mut p, "eye", Shape::Sphere, [0.07, 0.07, 0.04], [0.08, 1.37, 0.15], [0.0; 3], "glow");
    p.push(part("mouth grille", Shape::Box, [0.16, 0.03, 0.02], [0.0, 1.28, 0.15], [0.0; 3], "dark"));
    p.push(part("antenna", Shape::Cylinder(0.6), [0.02, 0.18, 0.02], [0.0, 1.57, 0.0], [0.0; 3], "dark"));
    p.push(part("antenna tip", Shape::Sphere, [0.05, 0.05, 0.05], [0.0, 1.67, 0.0], [0.0; 3], "red light"));
    p.push(part("neck", Shape::Cylinder(1.0), [0.1, 0.1, 0.1], [0.0, 1.19, 0.0], [0.0; 3], "dark"));
    pair(&mut p, "shoulder", Shape::Sphere, [0.13, 0.13, 0.13], [0.31, 1.08, 0.0], [0.0; 3], "dark");
    pair(&mut p, "arm", Shape::Cylinder(1.0), [0.08, 0.42, 0.08], [0.33, 0.84, 0.0], [0.0, 0.0, -6.0], "steel");
    pair(&mut p, "claw", Shape::Torus(0.35), [0.12, 0.12, 0.05], [0.36, 0.6, 0.0], [90.0, 0.0, 90.0], "dark");
    pair(&mut p, "leg", Shape::Cylinder(1.0), [0.11, 0.42, 0.11], [0.13, 0.33, 0.0], [0.0; 3], "steel");
    pair(&mut p, "foot", Shape::RoundedBox(0.3), [0.16, 0.08, 0.26], [0.13, 0.04, 0.04], [0.0; 3], "dark");
    doc("Robot", vec![("steel", mat("#AEB4BC", 0.9, 0.32)), ("dark", mat("#30343A", 0.7, 0.45)), ("glow", glow("#7FE7FF", 3.5)), ("red light", glow("#FF3040", 4.0))], p)
}

fn car() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("body", Shape::RoundedBox(0.3), [1.8, 0.5, 4.3], [0.0, 0.55, 0.0], [0.0; 3], "paint"));
    p.push(part("cabin", Shape::RoundedBox(0.35), [1.55, 0.48, 2.2], [0.0, 0.98, -0.25], [0.0; 3], "glass"));
    p.push(part("roof", Shape::RoundedBox(0.4), [1.5, 0.06, 1.6], [0.0, 1.22, -0.35], [0.0; 3], "paint"));
    for (side, x) in [("left", 0.82f32), ("right", -0.82)] {
        for (end, z) in [("front", 1.35f32), ("rear", -1.35)] {
            p.push(part(&format!("{end} {side} wheel"), Shape::Cylinder(1.0), [0.68, 0.26, 0.68], [x, 0.34, z], [0.0, 0.0, 90.0], "tyre"));
            p.push(part(&format!("{end} {side} rim"), Shape::Cylinder(1.0), [0.42, 0.28, 0.42], [x, 0.34, z], [0.0, 0.0, 90.0], "chrome"));
        }
    }
    pair(&mut p, "headlight", Shape::RoundedBox(0.3), [0.4, 0.12, 0.05], [0.55, 0.62, 2.14], [0.0; 3], "headlight");
    pair(&mut p, "tail light", Shape::RoundedBox(0.3), [0.4, 0.1, 0.05], [0.55, 0.64, -2.14], [0.0; 3], "taillight");
    p.push(part("grille", Shape::RoundedBox(0.3), [0.7, 0.16, 0.04], [0.0, 0.5, 2.15], [0.0; 3], "dark"));
    doc("Car", vec![("paint", mat("#B5121B", 0.6, 0.25)), ("glass", MatSpec { alpha: Some(0.35), ..mat("#1A2430", 0.0, 0.05) }), ("tyre", mat("#151515", 0.0, 0.85)), ("chrome", mat("#D0D4DA", 1.0, 0.1)), ("headlight", glow("#FFF6DD", 3.0)), ("taillight", glow("#FF1A1A", 2.5)), ("dark", mat("#202226", 0.5, 0.5))], p)
}

fn rocket() -> SceneDoc {
    let mut p = Vec::new();
    let body: Vec<[f32; 2]> = vec![[0.0, -0.5], [0.36, -0.5], [0.4, -0.42], [0.42, -0.2], [0.42, 0.18], [0.38, 0.3], [0.28, 0.4], [0.14, 0.47], [0.0, 0.5]];
    p.push(ScenePart { detail: 64, ..part("hull", Shape::Lathe(body), [0.9, 3.2, 0.9], [0.0, 1.9, 0.0], [0.0; 3], "hull") });
    p.push(part("nose band", Shape::Torus(0.08), [0.78, 0.06, 0.78], [0.0, 2.8, 0.0], [0.0; 3], "red"));
    p.push(part("window frame", Shape::Torus(0.25), [0.26, 0.26, 0.06], [0.0, 2.35, 0.37], [90.0, 0.0, 0.0], "steel"));
    p.push(part("window", Shape::Cylinder(1.0), [0.2, 0.03, 0.2], [0.0, 2.35, 0.37], [90.0, 0.0, 0.0], "glass"));
    for k in 0..4 {
        let a = k as f32 * 90.0;
        let (s, c) = (a.to_radians().sin(), a.to_radians().cos());
        let fin = vec![[-0.5, -0.5], [0.5, -0.5], [-0.5, 0.5]];
        p.push(ScenePart {
            name: format!("fin {}", k + 1),
            shape: Shape::Extrude(fin),
            size: [0.55, 0.9, 0.05],
            transform: math::mul(&math::translate([s * 0.58, 0.72, c * 0.58]), &math::euler_deg([0.0, a - 90.0, 0.0])),
            material: "red".into(),
            detail: 0,
        });
    }
    p.push(part("nozzle", Shape::Cylinder(1.6), [0.42, 0.32, 0.42], [0.0, 0.2, 0.0], [0.0; 3], "steel"));
    p.push(part("flame", Shape::Cone(32), [0.5, 0.7, 0.5], [0.0, -0.3, 0.0], [180.0, 0.0, 0.0], "flame"));
    doc("Rocket", vec![("hull", mat("#E8ECF0", 0.3, 0.35)), ("red", mat("#C8102E", 0.4, 0.35)), ("steel", mat("#8C939C", 1.0, 0.3)), ("glass", glow("#7FD8FF", 0.8)), ("flame", MatSpec { alpha: Some(0.85), ..glow("#FF9A2A", 5.0) })], p)
}

fn airplane() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("fuselage", Shape::Capsule, [0.7, 6.0, 0.7], [0.0, 1.2, 0.0], [90.0, 0.0, 0.0], "white"));
    p.push(part("cockpit", Shape::Sphere, [0.5, 0.35, 0.9], [0.0, 1.45, 2.3], [0.0; 3], "glass"));
    p.push(part("wings", Shape::Extrude(vec![[-0.5, -0.1], [0.5, -0.1], [0.12, 0.5], [-0.12, 0.5]]), [7.0, 1.6, 0.08], [0.0, 1.1, 0.2], [-90.0, 0.0, 0.0], "white"));
    p.push(part("tail fin", Shape::Extrude(vec![[-0.5, -0.5], [0.5, -0.5], [0.3, 0.5], [0.0, 0.5]]), [1.2, 1.3, 0.06], [0.0, 1.9, -2.5], [0.0, 90.0, 0.0], "accent"));
    p.push(part("stabilizers", Shape::Extrude(vec![[-0.5, -0.2], [0.5, -0.2], [0.2, 0.5], [-0.2, 0.5]]), [2.4, 0.7, 0.05], [0.0, 1.25, -2.6], [-90.0, 0.0, 0.0], "white"));
    pair(&mut p, "engine", Shape::Cylinder(0.85), [0.42, 1.1, 0.42], [1.4, 0.85, 0.6], [90.0, 0.0, 0.0], "steel");
    pair(&mut p, "engine glow", Shape::Cylinder(1.0), [0.3, 0.02, 0.3], [1.4, 0.85, 0.04], [90.0, 0.0, 0.0], "glow");
    doc("Airplane", vec![("white", mat("#EEF1F4", 0.2, 0.3)), ("glass", MatSpec { alpha: Some(0.5), ..mat("#1B2633", 0.0, 0.05) }), ("accent", mat("#1F5FFF", 0.3, 0.35)), ("steel", mat("#9AA1AA", 1.0, 0.3)), ("glow", glow("#FFB060", 2.0))], p)
}

fn house() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("walls", Shape::Box, [6.0, 3.0, 5.0], [0.0, 1.5, 0.0], [0.0; 3], "walls"));
    p.push(part("roof", Shape::Extrude(vec![[-0.5, -0.5], [0.5, -0.5], [0.0, 0.5]]), [7.0, 2.2, 5.6], [0.0, 4.1, 0.0], [0.0; 3], "roof"));
    p.push(part("door", Shape::Box, [1.1, 2.1, 0.08], [0.0, 1.05, 2.52], [0.0; 3], "wood"));
    pair(&mut p, "window", Shape::Box, [1.1, 1.0, 0.06], [1.9, 1.8, 2.52], [0.0; 3], "window");
    p.push(part("chimney", Shape::Box, [0.6, 1.6, 0.6], [1.8, 4.6, -0.8], [0.0; 3], "brick"));
    p.push(part("ground", Shape::Box, [9.0, 0.1, 8.0], [0.0, 0.05, 0.5], [0.0; 3], "grass"));
    doc("House", vec![("walls", mat("#E9E2D3", 0.0, 0.85)), ("roof", mat("#8A2B1E", 0.0, 0.7)), ("wood", MatSpec { finish: pt::finish_named("wood"), ..MatSpec::default() }), ("window", glow("#FFE7A8", 1.2)), ("brick", mat("#7A3B2A", 0.0, 0.9)), ("grass", mat("#3D7A2A", 0.0, 0.95))], p)
}

fn tree() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("trunk", Shape::Cylinder(0.7), [0.4, 2.6, 0.4], [0.0, 1.3, 0.0], [0.0; 3], "bark"));
    for (i, (pos, s)) in [([0.0, 3.0, 0.0], 2.2), ([0.7, 2.6, 0.3], 1.5), ([-0.6, 2.7, -0.2], 1.6), ([0.1, 3.7, -0.3], 1.4), ([-0.2, 2.5, 0.7], 1.3)].iter().enumerate() {
        p.push(part(&format!("leaves {}", i + 1), Shape::Sphere, [*s, *s * 0.9, *s], *pos, [0.0; 3], "leaves"));
    }
    doc("Tree", vec![("bark", mat("#5A3A22", 0.0, 0.9)), ("leaves", mat("#2E7D32", 0.0, 0.8))], p)
}

fn arc_reactor() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("outer ring", Shape::Torus(0.14), [1.0, 1.0, 0.14], [0.0, 0.6, 0.0], [90.0, 0.0, 0.0], "steel"));
    p.push(part("housing", Shape::Cylinder(1.0), [0.9, 0.12, 0.9], [0.0, 0.6, -0.06], [90.0, 0.0, 0.0], "dark"));
    p.push(part("inner ring", Shape::Torus(0.2), [0.48, 0.48, 0.1], [0.0, 0.6, 0.04], [90.0, 0.0, 0.0], "steel"));
    p.push(part("core", Shape::Cylinder(1.0), [0.3, 0.1, 0.3], [0.0, 0.6, 0.04], [90.0, 0.0, 0.0], "glow"));
    for k in 0..10 {
        let a = k as f32 * 36.0;
        let r = a.to_radians();
        p.push(ScenePart {
            name: format!("coil {}", k + 1),
            shape: Shape::RoundedBox(0.25),
            size: [0.1, 0.17, 0.06],
            transform: math::mul(&math::translate([0.36 * r.sin(), 0.6 + 0.36 * r.cos(), 0.03]), &math::euler_deg([0.0, 0.0, -a])),
            material: "copper".into(),
            detail: 0,
        });
    }
    doc("Arc reactor", vec![("steel", mat("#C9CED6", 1.0, 0.18)), ("dark", mat("#22262C", 0.7, 0.4)), ("glow", glow("#BFF4FF", 6.0)), ("copper", mat("#C77B4A", 1.0, 0.3))], p)
}

fn gear_profile(teeth: u32, outer: f32, inner: f32) -> Vec<[f32; 2]> {
    let mut pts = Vec::new();
    for t in 0..teeth {
        let base = t as f32 / teeth as f32 * TAU;
        let step = TAU / teeth as f32;
        for (frac, r) in [(0.0, inner), (0.18, inner), (0.3, outer), (0.62, outer), (0.74, inner)] {
            let a = base + frac * step;
            pts.push([r * a.cos(), r * a.sin()]);
        }
    }
    pts
}

fn gear() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("gear", Shape::Extrude(gear_profile(18, 0.5, 0.43)), [1.0, 1.0, 0.18], [0.0, 0.5, 0.0], [0.0; 3], "steel"));
    p.push(part("hub", Shape::Cylinder(1.0), [0.32, 0.26, 0.32], [0.0, 0.5, 0.0], [90.0, 0.0, 0.0], "brass"));
    p.push(part("axle", Shape::Cylinder(1.0), [0.1, 0.6, 0.1], [0.0, 0.5, 0.0], [90.0, 0.0, 0.0], "dark"));
    doc("Gear", vec![("steel", MatSpec { finish: pt::finish_named("brushed steel"), ..MatSpec::default() }), ("brass", mat("#C9A74A", 1.0, 0.3)), ("dark", mat("#2A2D33", 0.8, 0.4))], p)
}

fn pulley() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("wheel", Shape::Lathe(vec![[0.0, -0.5], [0.5, -0.5], [0.5, -0.35], [0.36, 0.0], [0.5, 0.35], [0.5, 0.5], [0.0, 0.5]]), [0.6, 0.18, 0.6], [0.0, 1.0, 0.0], [90.0, 0.0, 0.0], "steel"));
    p.push(part("axle", Shape::Cylinder(1.0), [0.06, 0.3, 0.06], [0.0, 1.0, 0.0], [90.0, 0.0, 0.0], "dark"));
    p.push(part("bracket", Shape::Box, [0.08, 0.5, 0.26], [0.0, 1.25, 0.0], [0.0; 3], "dark"));
    p.push(part("rope left", Shape::Cylinder(1.0), [0.03, 0.9, 0.03], [0.22, 0.55, 0.0], [0.0; 3], "rope"));
    p.push(part("rope right", Shape::Cylinder(1.0), [0.03, 0.5, 0.03], [-0.22, 0.75, 0.0], [0.0; 3], "rope"));
    p.push(part("weight", Shape::Box, [0.24, 0.24, 0.24], [0.22, 0.0 + 0.12, 0.0], [0.0; 3], "iron"));
    doc("Pulley", vec![("steel", mat("#B8BEC6", 1.0, 0.3)), ("dark", mat("#2A2D33", 0.7, 0.4)), ("rope", mat("#B08850", 0.0, 0.9)), ("iron", mat("#4A4E55", 1.0, 0.55))], p)
}

fn lever() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("fulcrum", Shape::Extrude(vec![[-0.5, -0.5], [0.5, -0.5], [0.0, 0.5]]), [0.5, 0.45, 0.4], [0.0, 0.225, 0.0], [0.0; 3], "steel"));
    p.push(part("beam", Shape::Box, [3.0, 0.08, 0.25], [0.0, 0.49, 0.0], [0.0, 0.0, 6.0], "wood"));
    p.push(part("load", Shape::Box, [0.4, 0.4, 0.4], [-1.25, 0.56, 0.0], [0.0, 0.0, 6.0], "iron"));
    doc("Lever", vec![("steel", mat("#8C939C", 1.0, 0.35)), ("wood", MatSpec { finish: pt::finish_named("wood"), ..MatSpec::default() }), ("iron", mat("#4A4E55", 1.0, 0.55))], p)
}

fn screw() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("shaft", Shape::Cylinder(1.0), [0.16, 1.2, 0.16], [0.0, 0.6, 0.0], [0.0; 3], "steel"));
    let helix: Vec<[f32; 3]> = (0..=240).map(|k| {
        let t = k as f32 / 240.0;
        let a = t * TAU * 8.0;
        [0.09 * a.cos(), 0.05 + t * 1.05, 0.09 * a.sin()]
    }).collect();
    p.push(part("thread", Shape::Tube(helix, 0.025), [1.0; 3], [0.0; 3], [0.0; 3], "steel"));
    p.push(part("head", Shape::Cylinder(0.9), [0.38, 0.12, 0.38], [0.0, 1.26, 0.0], [0.0; 3], "steel"));
    p.push(part("slot", Shape::Box, [0.3, 0.04, 0.05], [0.0, 1.32, 0.0], [0.0; 3], "dark"));
    doc("Screw", vec![("steel", mat("#B8BEC6", 1.0, 0.25)), ("dark", mat("#1E2024", 0.5, 0.5))], p)
}

fn inclined_plane() -> SceneDoc {
    doc(
        "Inclined plane",
        vec![("wood", MatSpec { finish: pt::finish_named("wood"), ..MatSpec::default() }), ("box", mat("#C8102E", 0.1, 0.5))],
        vec![
            part("ramp", Shape::Extrude(vec![[-0.5, -0.5], [0.5, -0.5], [-0.5, 0.5]]), [3.0, 1.2, 1.0], [0.0, 0.6, 0.0], [0.0; 3], "wood"),
            part("box", Shape::Box, [0.4, 0.4, 0.4], [0.2, 0.58, 0.0], [0.0, 0.0, -21.8], "box"),
        ],
    )
}

fn wheel_and_axle() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("wheel", Shape::Torus(0.12), [1.2, 1.2, 0.15], [0.0, 0.8, 0.0], [90.0, 0.0, 0.0], "steel"));
    for k in 0..6 {
        p.push(part(&format!("spoke {}", k + 1), Shape::Cylinder(1.0), [0.04, 1.05, 0.04], [0.0, 0.8, 0.0], [0.0, 0.0, k as f32 * 30.0], "steel"));
    }
    p.push(part("axle", Shape::Cylinder(1.0), [0.1, 1.4, 0.1], [0.0, 0.8, 0.0], [90.0, 0.0, 0.0], "dark"));
    p.push(part("crank", Shape::Box, [0.06, 0.5, 0.06], [0.0, 0.55, -0.7], [0.0; 3], "dark"));
    doc("Wheel and axle", vec![("steel", mat("#B8BEC6", 1.0, 0.3)), ("dark", mat("#2A2D33", 0.7, 0.45))], p)
}

fn satellite() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("bus", Shape::Box, [0.8, 0.8, 1.0], [0.0, 1.2, 0.0], [0.0; 3], "gold foil"));
    pair(&mut p, "solar panel", Shape::Box, [2.2, 0.03, 0.9], [1.6, 1.2, 0.0], [0.0; 3], "solar");
    pair(&mut p, "panel arm", Shape::Cylinder(1.0), [0.04, 0.5, 0.04], [0.45, 1.2, 0.0], [0.0, 0.0, 90.0], "steel");
    p.push(part("dish", Shape::Dome, [0.9, 0.25, 0.9], [0.0, 1.85, 0.0], [180.0, 0.0, 0.0], "white"));
    p.push(part("feed", Shape::Cylinder(0.4), [0.06, 0.3, 0.06], [0.0, 1.85, 0.0], [0.0; 3], "steel"));
    p.push(part("thruster", Shape::Cone(24), [0.2, 0.2, 0.2], [0.0, 0.7, 0.0], [180.0, 0.0, 0.0], "steel"));
    doc("Satellite", vec![("gold foil", mat("#C9A13A", 1.0, 0.45)), ("solar", mat("#14254F", 0.6, 0.2)), ("steel", mat("#B8BEC6", 1.0, 0.3)), ("white", MatSpec { double_sided: true, ..mat("#EDEFF2", 0.1, 0.4) })], p)
}

fn tower() -> SceneDoc {
    let mut p = Vec::new();
    p.push(part("tower", Shape::Cylinder(0.92), [2.0, 6.0, 2.0], [0.0, 3.0, 0.0], [0.0; 3], "stone"));
    p.push(part("crown", Shape::Cylinder(1.0), [2.3, 0.6, 2.3], [0.0, 6.3, 0.0], [0.0; 3], "stone"));
    for k in 0..8 {
        let a = (k as f32 * 45.0).to_radians();
        p.push(part(&format!("merlon {}", k + 1), Shape::Box, [0.4, 0.5, 0.3], [1.0 * a.sin(), 6.85, 1.0 * a.cos()], [0.0, k as f32 * 45.0, 0.0], "stone"));
    }
    p.push(part("door", Shape::Box, [0.7, 1.3, 0.1], [0.0, 0.65, 0.95], [0.0; 3], "wood"));
    p.push(part("flag pole", Shape::Cylinder(1.0), [0.05, 1.6, 0.05], [0.0, 7.4, 0.0], [0.0; 3], "wood"));
    p.push(part("flag", Shape::Box, [0.7, 0.4, 0.02], [0.37, 7.95, 0.0], [0.0; 3], "flag"));
    doc("Tower", vec![("stone", MatSpec { finish: pt::finish_named("concrete"), ..MatSpec::default() }), ("wood", mat("#6B4226", 0.0, 0.8)), ("flag", mat("#C8102E", 0.0, 0.7))], p)
}

fn torus_knot() -> SceneDoc {
    let (pp, q) = (2.0f32, 3.0f32);
    let path: Vec<[f32; 3]> = (0..400).map(|k| {
        let t = k as f32 / 400.0 * TAU;
        let r = 0.35 + 0.15 * (q * t).cos();
        [r * (pp * t).cos(), 0.7 + 0.15 * (q * t).sin(), r * (pp * t).sin()]
    }).collect();
    let mut knot = part("knot", Shape::Box, [1.0; 3], [0.0; 3], [0.0; 3], "chrome");
    knot.shape = Shape::Tube(path, 0.07);
    doc("Torus knot", vec![("chrome", mat("#D8DCE2", 1.0, 0.08))], vec![knot])
}

fn primitive(name: &str, shape: Shape, size: [f32; 3]) -> SceneDoc {
    let y = size[1] / 2.0;
    doc(name, vec![("surface", mat("#9EC9FF", 0.6, 0.25))], vec![part(&name.to_lowercase(), shape, size, [0.0, y, 0.0], [0.0; 3], "surface")])
}

fn cube() -> SceneDoc {
    primitive("Cube", Shape::RoundedBox(0.04), [1.0, 1.0, 1.0])
}

fn sphere() -> SceneDoc {
    primitive("Sphere", Shape::Sphere, [1.0, 1.0, 1.0])
}

fn cylinder() -> SceneDoc {
    primitive("Cylinder", Shape::Cylinder(1.0), [0.8, 1.2, 0.8])
}

fn cone() -> SceneDoc {
    primitive("Cone", Shape::Cone(48), [1.0, 1.2, 1.0])
}

fn pyramid() -> SceneDoc {
    primitive("Pyramid", Shape::Cone(4), [1.2, 1.0, 1.2])
}

fn torus() -> SceneDoc {
    primitive("Torus", Shape::Torus(0.35), [1.2, 0.42, 1.2])
}

fn capsule() -> SceneDoc {
    primitive("Capsule", Shape::Capsule, [0.5, 1.4, 0.5])
}

/// The library, checked in order (the first entry with a matching word wins).
pub static LIBRARY: &[Entry] = &[
    Entry { key: "earth", words: &["planet earth", "the earth", "earth", "the world", "our planet", "globe", "erde", "die erde", "weltkugel", "erdkugel"], kind: Kind::Planet(Planet::Earth) },
    Entry { key: "moon", words: &["the moon", "moon", "luna", "der mond", "mond"], kind: Kind::Planet(Planet::Moon) },
    Entry { key: "mars", words: &["mars", "red planet", "roter planet"], kind: Kind::Planet(Planet::Mars) },
    Entry { key: "mercury", words: &["mercury", "merkur"], kind: Kind::Planet(Planet::Mercury) },
    Entry { key: "venus", words: &["venus"], kind: Kind::Planet(Planet::Venus) },
    Entry { key: "jupiter", words: &["jupiter"], kind: Kind::Planet(Planet::Jupiter) },
    Entry { key: "saturn", words: &["saturn"], kind: Kind::Planet(Planet::Saturn) },
    Entry { key: "uranus", words: &["uranus"], kind: Kind::Planet(Planet::Uranus) },
    Entry { key: "neptune", words: &["neptune", "neptun"], kind: Kind::Planet(Planet::Neptune) },
    Entry { key: "pluto", words: &["pluto"], kind: Kind::Planet(Planet::Pluto) },
    Entry { key: "sun", words: &["the sun", "sun", "our star", "die sonne", "sonne"], kind: Kind::Planet(Planet::Sun) },
    Entry { key: "arc reactor", words: &["arc reactor", "arc-reactor", "arkreaktor", "ark reaktor", "lichtbogenreaktor"], kind: Kind::Scene(arc_reactor) },
    Entry { key: "iron man suit", words: &["iron man suit", "ironman suit", "iron man armor", "iron man armour", "iron man", "ironman", "iron-man", "mark 85", "stark suit", "iron man anzug", "iron-man-anzug", "ironman anzug", "iron man rüstung"], kind: Kind::Scene(iron_suit) },
    Entry { key: "robot", words: &["robot", "roboter", "droid", "android"], kind: Kind::Scene(robot) },
    Entry { key: "car", words: &["sports car", "car", "auto", "sportwagen", "wagen"], kind: Kind::Scene(car) },
    Entry { key: "rocket", words: &["rocket", "rakete", "spaceship", "raumschiff"], kind: Kind::Scene(rocket) },
    Entry { key: "airplane", words: &["airplane", "aeroplane", "plane", "jet", "airliner", "flugzeug"], kind: Kind::Scene(airplane) },
    Entry { key: "house", words: &["house", "home", "haus"], kind: Kind::Scene(house) },
    Entry { key: "tree", words: &["tree", "baum"], kind: Kind::Scene(tree) },
    Entry { key: "gear", words: &["gear", "cog", "cogwheel", "zahnrad"], kind: Kind::Scene(gear) },
    Entry { key: "pulley", words: &["pulley", "flaschenzug", "rolle"], kind: Kind::Scene(pulley) },
    Entry { key: "lever", words: &["lever", "seesaw", "hebel", "wippe"], kind: Kind::Scene(lever) },
    Entry { key: "screw", words: &["screw", "bolt", "schraube"], kind: Kind::Scene(screw) },
    Entry { key: "inclined plane", words: &["inclined plane", "ramp", "schiefe ebene", "rampe"], kind: Kind::Scene(inclined_plane) },
    Entry { key: "wheel and axle", words: &["wheel and axle", "rad und achse"], kind: Kind::Scene(wheel_and_axle) },
    Entry { key: "satellite", words: &["satellite", "satellit"], kind: Kind::Scene(satellite) },
    Entry { key: "eiffel tower", words: &["eiffel tower", "eiffel", "eifel tower", "eiffelturm", "eifelturm", "tour eiffel"], kind: Kind::Detailed(eiffel_tower) },
    Entry { key: "tower", words: &["castle tower", "tower", "castle", "turm", "burg"], kind: Kind::Scene(tower) },
    Entry { key: "torus knot", words: &["torus knot", "knot", "knoten"], kind: Kind::Scene(torus_knot) },
    Entry { key: "cube", words: &["cube", "box", "würfel", "kiste", "quader"], kind: Kind::Scene(cube) },
    Entry { key: "sphere", words: &["sphere", "ball", "kugel"], kind: Kind::Scene(sphere) },
    Entry { key: "cylinder", words: &["cylinder", "zylinder"], kind: Kind::Scene(cylinder) },
    Entry { key: "cone", words: &["cone", "kegel"], kind: Kind::Scene(cone) },
    Entry { key: "pyramid", words: &["pyramid", "pyramide"], kind: Kind::Scene(pyramid) },
    Entry { key: "torus", words: &["torus", "donut", "doughnut", "ring"], kind: Kind::Scene(torus) },
    Entry { key: "capsule", words: &["capsule", "pill", "kapsel"], kind: Kind::Scene(capsule) },
];

/// Words that say nothing about what the model is.
pub const FILLER: &[&str] = &[
    "a", "an", "the", "of", "some", "my", "me", "little", "small", "big", "large", "huge", "tiny", "simple", "basic", "nice", "cool", "cute", "real", "realistic", "detailed", "model", "3d", "please",
    "in", "with", "and", "made", "out", "finish", "texture", "colour", "color", "paint", "painted",
    "ein", "eine", "einen", "einer", "der", "die", "das", "des", "dem", "den", "modell", "schönes", "schöne", "kleine", "kleinen", "großes", "große", "bitte", "mal", "mit", "und", "aus", "im", "farbe",
];

/// The built-in model a request names: the entry whose longest word appears in it as whole
/// words. A scene counts only when nothing beyond it, its colours and filler was asked for ("a
/// red car" is the built-in car, "a 1967 Mustang car" is not); a planet always counts.
pub fn lookup(query: &str) -> Option<&'static Entry> {
    let q = norm(&query.replace('\'', " "));
    let mut best: Option<(&'static Entry, &'static str)> = None;
    for e in LIBRARY {
        for w in e.words {
            if q.contains(&format!(" {w} ")) && best.is_none_or(|(_, b)| w.len() > b.len()) {
                best = Some((e, w));
            }
        }
    }
    let (entry, word) = best?;
    if matches!(entry.kind, Kind::Planet(_)) {
        return Some(entry);
    }
    let mut rest = q.replacen(&format!(" {word} "), " ", 1);
    for (at, len, _) in paints_at(&rest).into_iter().rev() {
        rest.replace_range(at..at + len, " ");
    }
    rest.split_whitespace().all(|w| FILLER.contains(&w)).then_some(entry)
}

/// A planet: radius 1 unit, `unit_m` its real radius; Saturn with its rings.
pub fn planet_model(p: Planet, maps: pt::PlanetMaps) -> Model {
    let name = format!("{p:?}");
    let mut model = Model { name: name.clone(), unit_m: 1.0, ..Model::default() };
    model.textures.push(maps.albedo);
    model.textures.push(maps.mr);
    let mut m = Material { name: name.to_lowercase(), base_color: [1.0; 4], metallic: 0.0, roughness: 1.0, base_color_tex: Some(0), metallic_roughness_tex: Some(1), ..Material::default() };
    if let Some(n) = maps.normal {
        model.textures.push(n);
        m.normal_tex = Some(model.textures.len() - 1);
    }
    if let Some(e) = maps.emissive {
        model.textures.push(e);
        m.emissive_tex = Some(model.textures.len() - 1);
        m.emissive = [1.0; 3];
    }
    model.materials.push(m);
    let mut sphere = mesh(&Shape::Sphere, [2.0, 2.0, 2.0], 160);
    sphere.name = name.clone();
    model.parts.push(sphere);
    if p == Planet::Saturn {
        model.textures.push(pt::saturn_rings(1024));
        model.materials.push(Material { name: "rings".into(), base_color: [1.0; 4], roughness: 0.8, base_color_tex: Some(model.textures.len() - 1), alpha: AlphaMode::Blend, double_sided: true, ..Material::default() });
        let mut rings = mesh(&Shape::Annulus(0.53), [4.6, 1.0, 4.6], 160);
        rings.name = "rings".into();
        rings.material = 1;
        model.parts.push(rings);
    }
    // Real radius in metres (measure: "12,742 km across").
    model.unit_m = match p {
        Planet::Earth => 6.371e6,
        Planet::Moon => 1.737e6,
        Planet::Mercury => 2.44e6,
        Planet::Venus => 6.052e6,
        Planet::Mars => 3.39e6,
        Planet::Jupiter => 6.991e7,
        Planet::Saturn => 5.823e7,
        Planet::Uranus => 2.536e7,
        Planet::Neptune => 2.462e7,
        Planet::Pluto => 1.188e6,
        Planet::Sun => 6.957e8,
    };
    model.finish();
    model
}

/// Axial tilt (degrees) and the atmosphere's colour, for the studio's planet view.
pub fn planet_look(p: Planet) -> (f32, Option<[f32; 3]>) {
    match p {
        Planet::Earth => (23.4, Some([0.35, 0.62, 1.0])),
        Planet::Moon => (6.7, None),
        Planet::Mercury => (0.0, None),
        Planet::Venus => (2.6, Some([1.0, 0.86, 0.6])),
        Planet::Mars => (25.2, Some([0.95, 0.6, 0.45])),
        Planet::Jupiter => (3.1, Some([0.95, 0.85, 0.7])),
        Planet::Saturn => (26.7, Some([0.95, 0.88, 0.7])),
        Planet::Uranus => (97.8, Some([0.6, 0.95, 1.0])),
        Planet::Neptune => (28.3, Some([0.4, 0.6, 1.0])),
        Planet::Pluto => (57.0, None),
        Planet::Sun => (7.25, Some([1.0, 0.6, 0.2])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scene_from_the_model_parses_with_groups_and_loose_keys() {
        let text = r##"Sure! {"name": "Lamp", "materials": {"brass": {"color": "#c9a74a", "metallic": 1, "roughness": 0.3}, "bulb": {"color": "white", "emissive": 3}},
            "parts": [
              {"name": "base", "shape": "cylinder", "size": [0.3, 0.05, 0.3], "position": [0, 0.025, 0], "material": "brass"},
              {"name": "arm", "type": "capsule", "scale": 0.4, "pos": {"x": 0, "y": 0.25, "z": 0}, "rotation": [0, 0, 10], "color": [255, 0, 0]},
              {"name": "head", "position": [0, 0.5, 0], "rotation": [0, 90, 0], "parts": [
                 {"name": "shade", "shape": "cone", "size": [0.25, 0.2, 0.25], "position": [0.1, 0, 0], "material": "brass"},
                 {"name": "bulb", "shape": "sphere", "radius": 0.04, "position": [0.1, -0.05, 0], "material": "bulb"}]},
              {"name": "mystery", "shape": "hyperboloid", "size": 0.1, "material": "carbon fibre"}
            ]}"##;
        let doc = parse_scene(text).unwrap();
        assert_eq!(doc.name, "Lamp");
        let names: Vec<&str> = doc.parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["base", "arm", "shade", "bulb", "mystery"]);
        assert_eq!(doc.parts[1].shape, Shape::Capsule);
        assert_eq!(doc.parts[1].size, [0.4, 0.4, 0.4]);
        assert_eq!(doc.parts[4].shape, Shape::Box, "an unknown shape is a box");
        assert_eq!(doc.parts[3].size, [0.08, 0.08, 0.08]);
        // The group's 90° turn moves the shade's +x offset to -z.
        let shade = math::point(&doc.parts[2].transform, [0.0; 3]);
        assert!((shade[0]).abs() < 1e-5 && (shade[1] - 0.5).abs() < 1e-5 && (shade[2] + 0.1).abs() < 1e-5, "{shade:?}");
        let model = build(&doc);
        assert_eq!(model.parts.len(), 5);
        let bulb = &model.materials[model.parts[3].material];
        assert!(bulb.emissive[0] > 2.0, "{bulb:?}");
        let arm = &model.materials[model.parts[1].material];
        assert!(arm.base_color[0] > 0.9 && arm.base_color[1] < 0.01, "an inline colour: {arm:?}");
        let mystery = &model.materials[model.parts[4].material];
        assert!(mystery.triplanar && mystery.base_color_tex.is_some(), "a finish as the material");
        // The box left at the origin reaches 5 cm down (the studio sets the lowest point on its floor).
        let b = model.bounds();
        assert!(b.is_valid() && b.min[1] > -0.051 && (b.max[1] - 0.6).abs() < 0.01, "{b:?}");
    }

    #[test]
    fn cut_off_json_is_repaired_at_the_last_complete_part() {
        let full = r#"{"name":"Rocket","parts":[{"name":"hull","shape":"cylinder","size":[1,3,1],"position":[0,1.5,0]},{"name":"nose","shape":"cone","size":[1,1,1],"position":[0,3.5,0]},{"name":"fin","shape":"bo"#;
        let doc = parse_scene(full).unwrap();
        assert_eq!(doc.parts.len(), 2);
        assert_eq!(doc.parts[1].shape, Shape::Cone(40));
        assert!(parse_scene("I can't do that.").is_err());
        assert!(parse_scene(r#"{"name": "x", "parts": []}"#).is_err());
        assert_eq!(repair_json(r#"{"a": [1, 2, {"b": "c\"}"}]"#).unwrap()["a"][2]["b"], "c\"}");
    }

    #[test]
    fn colours_and_finishes_are_read_in_order_in_both_languages() {
        let names = |t: &str| paints_in(t).iter().map(Paint::name).collect::<Vec<_>>();
        assert_eq!(names("paint it red and gold"), vec!["red", "gold"]);
        assert_eq!(names("mach es rot und golden"), vec!["red", "gold"]);
        assert_eq!(names("make the helmet dark blue"), vec!["navy"]);
        assert_eq!(names("mach den Helm blau"), vec!["blue"]);
        assert_eq!(names("die roten Teile"), vec!["red"]);
        assert_eq!(names("brushed steel with a black visor"), vec!["brushed steel", "black"]);
        assert_eq!(names("paint it #ff8800"), vec!["that colour"]);
        assert!(names("make it bigger").is_empty());
        assert_eq!(hex_colour("#f80"), Some([255, 136, 0]));
    }

    #[test]
    fn every_library_model_builds_closed_and_sane() {
        for e in LIBRARY {
            for level in 1..=MAX_LEVEL {
                let Some(doc) = e.kind.scene(level) else { break };
                let m = build(&doc);
                assert!(m.triangles() > 10, "{}", e.key);
                let b = m.bounds();
                // (Landmarks are at their real size.)
                let limit = if e.key == "eiffel tower" { 400.0 } else { 20.0 };
                assert!(b.is_valid() && b.min[1] > -1.0 && b.radius() < limit, "{}: {b:?}", e.key);
                for part in &m.parts {
                    assert_eq!(part.normals.len(), part.positions.len(), "{} {}", e.key, part.name);
                    assert!(part.normals.iter().all(|n| (math::length(*n) - 1.0).abs() < 1e-3), "{} {}", e.key, part.name);
                }
                if matches!(e.kind, Kind::Scene(_)) {
                    break;
                }
            }
        }
        // The Eiffel Tower: 330 m to the beacon, 125 m across the legs, more members per level.
        let tower = |level| build(&eiffel_tower(level));
        let (one, four) = (tower(1), tower(4));
        let size = one.size_m();
        assert!((size[1] - 331.2).abs() < 2.0 && (size[0] - 125.0).abs() < 8.0, "{size:?}");
        assert!(four.triangles() > 3 * one.triangles(), "{} → {}", one.triangles(), four.triangles());
        assert_eq!(lookup("the Eiffel Tower").map(|e| e.key), Some("eiffel tower"));
        assert_eq!(lookup("eiffelturm").map(|e| e.key), Some("eiffel tower"));
        assert_eq!(lookup("an iron man suit").map(|e| e.key), Some("iron man suit"));
        assert_eq!(lookup("planet earth").map(|e| e.key), Some("earth"));
        assert_eq!(lookup("die Erde").map(|e| e.key), Some("earth"));
        assert_eq!(lookup("the moon").map(|e| e.key), Some("moon"));
        assert_eq!(lookup("a car").map(|e| e.key), Some("car"));
        assert!(lookup("a vintage racing car from the fifties with chrome bumpers").is_none(), "a detailed request is more than the generic car");
        assert!(lookup("the millennium falcon").is_none());
    }

    #[test]
    fn primitives_face_outwards() {
        // For a closed convex shape around the origin every face's normal points away from it.
        for (name, shape, size) in [("box", Shape::Box, [1.0, 2.0, 0.5]), ("rounded", Shape::RoundedBox(0.3), [1.0, 1.0, 1.0]), ("sphere", Shape::Sphere, [1.0; 3]), ("cylinder", Shape::Cylinder(1.0), [1.0; 3]), ("cone", Shape::Cone(24), [1.0; 3]), ("pyramid", Shape::Cone(4), [1.0; 3]), ("capsule", Shape::Capsule, [0.5, 1.5, 0.5]), ("extrude", Shape::Extrude(vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]), [1.0; 3])] {
            let p = mesh(&shape, size, 0);
            let mut wrong = 0;
            for t in p.indices.chunks_exact(3) {
                let (a, b, c) = (p.positions[t[0] as usize], p.positions[t[1] as usize], p.positions[t[2] as usize]);
                let f = math::cross(math::sub(b, a), math::sub(c, a));
                if math::length(f) < 1e-9 {
                    continue;
                }
                let centre = math::scale(math::add(math::add(a, b), c), 1.0 / 3.0);
                if math::dot(f, centre) < 0.0 {
                    wrong += 1;
                }
                // The vertex normals agree with the winding.
                assert!(math::dot(f, p.normals[t[0] as usize]) > -1e-6 || math::dot(f, p.normals[t[1] as usize]) > -1e-6, "{name}: normals against the winding");
            }
            assert_eq!(wrong, 0, "{name}: {wrong} faces point inwards");
        }
        // The torus: outward from its ring.
        let p = mesh(&Shape::Torus(0.3), [1.0; 3], 24);
        let big = 0.5 / 1.3;
        for t in p.indices.chunks_exact(3) {
            let (a, b, c) = (p.positions[t[0] as usize], p.positions[t[1] as usize], p.positions[t[2] as usize]);
            let f = math::cross(math::sub(b, a), math::sub(c, a));
            let centre = math::scale(math::add(math::add(a, b), c), 1.0 / 3.0);
            let ring = math::scale(math::normalize([centre[0], 0.0, centre[2]]), big);
            if math::length(f) > 1e-9 {
                assert!(math::dot(f, math::sub(centre, ring)) > 0.0, "torus face inwards");
            }
        }
    }
}
