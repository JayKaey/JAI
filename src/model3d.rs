//! THE 3D STUDIO ("render me an Iron Man suit in 3D", "show me a 3D model of the Earth", "zeig
//! mir den Mars in 3D"): any model on a block of the board (or its own window), lit like a
//! product shot or shown as a hologram, turned with the mouse and changed by voice ("make it
//! gold", "paint it red and gold", "make the helmet blue", "carbon fibre", "explode it", "save it
//! as STL for my 3D printer").
//!
//! Where a model comes from, fastest first:
//! 1. the built-in library (`scene3d`): the planets with real or computed maps (the Earth wears
//!    the Blue Marble the Earth panel cached in `config/earth_cache`), primitives, the simple
//!    machines and a few showpieces: on screen at once;
//! 2. a curated index of keyless repositories (NASA 3D Resources, the Khronos glTF samples);
//! 3. Sketchfab: the search needs no key; downloading needs Sir's own API token, which he
//!    pastes into the Integrations hub (vault service `sketchfab`; never logged, never asked
//!    for). Without it the best named match comes keyless from the Objaverse mirror on Hugging
//!    Face (Sketchfab's free models, author and licence credited); only when the mirror has none
//!    does the best match show as a preview card while JAI rebuilds or composes one;
//! 4. a model rebuilt from a photograph (`photo3d`: TripoSR on a cut-out picture from the picture
//!    search, photo credited), when its networks are installed;
//! 5. else a model composed by the language model as a JSON scene of primitives, streamed in
//!    part by part over a wireframe placeholder;
//! 6. Sir's own files: a path, "the model in my Downloads", or a file dropped on the studio.
//!
//! Downloads are cached in `config/model_cache` (about 2 GB, the oldest go first). Edits are
//! deterministic phrase parsing (English and German), never the model. Unit tests, the verifier
//! and dry runs never touch the network or the language model: they get the built-in library or
//! a stand-in, and exports land in the temp folder.

use crate::intents::Intent;
use crate::mesh_io::{self, math, AlphaMode, Bounds, Credit, Material, Model, Part, Texture};
use crate::procedural_textures::{self as pt, Planet};
use crate::scene3d::{self, Paint};
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};

pub const TOOL: &str = "show_3d_model";
/// The vault service of Sir's Sketchfab API token.
pub const SKETCHFAB_SERVICE: &str = "sketchfab";
const CACHE_DIR: &str = "config/model_cache";
/// The cache's size bound; the oldest files go first.
const CACHE_LIMIT: u64 = 2 << 30;
/// No single download larger than this.
const MAX_DOWNLOAD: u64 = 150 << 20;
/// Sketchfab models above this size are passed over for a lighter match.
const SKETCHFAB_PREFERRED: u64 = 50 << 20;
/// The turntable's default speed (radians per second).
pub const DEFAULT_SPIN: f32 = 0.35;
/// What a composed model may take before the stand-in is shown instead.
const COMPOSE_TIMEOUT: Duration = Duration::from_secs(60);

// ------------------------------------------------------------------------------- state ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Realistic,
    Hologram,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    pub look: Look,
    pub wireframe: bool,
    /// Turntable speed in radians per second (0: still).
    pub spin: f32,
    /// 0: together; 1: the parts pulled apart from the centre.
    pub explode: f32,
    /// Display scale (1: fills the view).
    pub scale: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState { look: Look::Realistic, wireframe: false, spin: DEFAULT_SPIN, explode: 0.0, scale: 1.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraCmd {
    Reset,
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
    /// Distance factor (below 1: closer).
    Zoom(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Working,
    Ready,
    Failed,
}

/// Which parts an edit changes.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    All,
    /// Parts by name ("helmet"), resolved against the model when the scene is built.
    Named(String),
    /// The larger share of the surface (the first colour of "red and gold").
    Primary,
    /// The rest.
    Secondary,
}

#[derive(Clone, Debug, PartialEq)]
enum Change {
    Paint(Paint),
    /// Self-lit, in this colour (None: its own colour).
    Glow(Option<[f32; 3]>),
    NoGlow,
}

#[derive(Clone, Debug, PartialEq)]
struct Edit {
    target: Target,
    change: Change,
}

/// What the renderer and the panel draw: the model, its materials after Sir's edits, and how it
/// sits in the view. Immutable; a new one is made for every change.
pub struct Scene {
    /// Changes when the geometry does (the renderer re-uploads it).
    pub model_id: u64,
    pub model: Arc<Model>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    /// Each part's material in `materials`.
    pub part_material: Vec<usize>,
    pub bounds: Bounds,
    /// Each part's centre (explode pulls them away from the model's centre).
    pub part_centres: Vec<[f32; 3]>,
    /// A planet: no ground, the turntable turns about its tilted axis.
    pub planet: bool,
    pub tilt_deg: f32,
    pub atmosphere: Option<[f32; 3]>,
    /// The model's edges for the wireframe fallback, in the normalised frame (centre at the
    /// origin, radius 1).
    pub wire: Arc<Vec<[[f32; 3]; 2]>>,
    /// Up to 40 part names.
    pub part_names: Vec<String>,
}

impl Scene {
    /// Model space → the view's normalised frame (centred, radius 1).
    pub fn normalise(&self) -> math::M4 {
        let c = self.bounds.centre();
        let r = self.bounds.radius().max(1e-9);
        math::mul(&math::scaling([1.0 / r; 3]), &math::translate([-c[0], -c[1], -c[2]]))
    }

    /// The ground's height in the normalised frame.
    pub fn ground_y(&self) -> f32 {
        (self.bounds.min[1] - self.bounds.centre()[1]) / self.bounds.radius().max(1e-9)
    }
}

/// The Sketchfab match shown while there is no token (or while it downloads).
#[derive(Clone, Debug)]
pub struct Preview {
    pub title: String,
    pub author: String,
    pub license: String,
    pub url: String,
    pub thumbnail: Option<Texture>,
    /// Changes when the thumbnail arrives (the panel uploads it again).
    pub id: u64,
}

/// Everything the panel shows, as one consistent copy.
#[derive(Clone)]
pub struct Snapshot {
    pub open: bool,
    pub title: String,
    /// "Built-in", "Sketchfab", "NASA 3D Resources", "Composed", "Your file" …
    pub source: String,
    pub phase: Phase,
    pub status: String,
    pub progress: Option<f32>,
    pub started: Option<Instant>,
    pub scene: Option<Arc<Scene>>,
    pub view: ViewState,
    pub preview: Option<Arc<Preview>>,
    pub placeholder: Option<Arc<Vec<[[f32; 3]; 2]>>>,
    pub note: Option<String>,
    pub credit: Option<Credit>,
    pub size_m: Option<[f32; 3]>,
    pub triangles: usize,
    pub demo: bool,
}

struct Studio {
    version: u64,
    job: u64,
    title: String,
    source: String,
    phase: Phase,
    status: String,
    progress: Option<f32>,
    started: Option<Instant>,
    base: Option<Arc<Model>>,
    /// The base model's real size (computed once per model).
    size_m: Option<[f32; 3]>,
    model_id: u64,
    planet: Option<Planet>,
    wire: Arc<Vec<[[f32; 3]; 2]>>,
    edits: Vec<Edit>,
    scene: Option<Arc<Scene>>,
    view: ViewState,
    camera: VecDeque<CameraCmd>,
    preview: Option<Arc<Preview>>,
    placeholder: Option<Arc<Vec<[[f32; 3]; 2]>>>,
    note: Option<String>,
    credit: Option<Credit>,
    demo: bool,
    cached: Option<Arc<Snapshot>>,
    /// What Sir asked for, the detail level it was made at and, when the language model
    /// composed it, its scene (so "make it more detailed" knows where to go from).
    query: String,
    level: u32,
    doc: Option<scene3d::SceneDoc>,
}

impl Default for Studio {
    fn default() -> Self {
        Studio {
            version: 1,
            job: 0,
            title: String::new(),
            source: String::new(),
            phase: Phase::Idle,
            status: String::new(),
            progress: None,
            started: None,
            base: None,
            size_m: None,
            model_id: 0,
            planet: None,
            wire: Arc::new(Vec::new()),
            edits: Vec::new(),
            scene: None,
            view: ViewState::default(),
            camera: VecDeque::new(),
            preview: None,
            placeholder: None,
            note: None,
            credit: None,
            demo: false,
            cached: None,
            query: String::new(),
            level: 1,
            doc: None,
        }
    }
}

static OPEN: AtomicBool = AtomicBool::new(false);
static IDS: AtomicU64 = AtomicU64::new(1);
static STUDIO: LazyLock<Mutex<Studio>> = LazyLock::new(|| Mutex::new(Studio::default()));

fn studio() -> MutexGuard<'static, Studio> {
    STUDIO.lock().unwrap_or_else(|e| e.into_inner())
}

fn next_id() -> u64 {
    IDS.fetch_add(1, Ordering::Relaxed)
}

/// Something changed: the panel's copy is stale and the HUD draws again.
fn touched(s: &mut Studio) {
    s.version += 1;
    s.cached = None;
    crate::gui::refresh();
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Closes the studio and lets the model go (the renderer frees its GPU memory on its next frame).
pub fn close() {
    OPEN.store(false, Ordering::Release);
    let mut s = studio();
    s.job += 1;
    let keep_look = s.view.look;
    *s = Studio { version: s.version + 1, job: s.job, ..Studio::default() };
    s.view.look = keep_look;
    drop(s);
    crate::gpu3d::release();
    crate::gui::refresh();
}

/// The verifier, dry runs and unit tests never touch the network, the language model or Sir's
/// folders.
fn demo_mode() -> bool {
    cfg!(test) || crate::live_self_tester::running() || crate::live_self_tester::dry()
}

pub fn snapshot() -> Arc<Snapshot> {
    let mut s = studio();
    if let Some(c) = &s.cached {
        return Arc::clone(c);
    }
    let size_m = s.size_m;
    let snap = Arc::new(Snapshot {
        open: is_open(),
        title: s.title.clone(),
        source: s.source.clone(),
        phase: s.phase,
        status: s.status.clone(),
        progress: s.progress,
        started: s.started,
        scene: s.scene.clone(),
        view: s.view.clone(),
        preview: s.preview.clone(),
        placeholder: s.placeholder.clone(),
        note: s.note.clone(),
        credit: s.credit.clone(),
        size_m,
        triangles: s.base.as_ref().map_or(0, |m| m.triangles()),
        demo: s.demo,
    });
    s.cached = Some(Arc::clone(&snap));
    snap
}

/// The camera moves asked for since the last frame (the panel owns the camera).
pub fn take_camera() -> Vec<CameraCmd> {
    studio().camera.drain(..).collect()
}

/// A change from the panel's own controls (the chips).
pub fn set_view(f: impl FnOnce(&mut ViewState)) {
    let mut s = studio();
    f(&mut s.view);
    touched(&mut s);
}

/// Starts a new model: the studio opens with a placeholder until `deliver`. Returns the job.
fn begin(title: &str, source: &str, status: &str, placeholder: Option<Arc<Vec<[[f32; 3]; 2]>>>) -> u64 {
    OPEN.store(true, Ordering::Release);
    let mut s = studio();
    s.job += 1;
    s.title = title.to_string();
    s.source = source.to_string();
    s.phase = Phase::Working;
    s.status = status.to_string();
    s.progress = None;
    s.started = Some(Instant::now());
    s.base = None;
    s.size_m = None;
    s.scene = None;
    s.planet = None;
    s.edits.clear();
    s.preview = None;
    s.note = None;
    s.credit = None;
    s.placeholder = placeholder.or_else(|| Some(Arc::new(scan_sphere())));
    s.demo = demo_mode();
    let look = s.view.look;
    s.view = ViewState { look, ..ViewState::default() };
    s.camera.clear();
    s.camera.push_back(CameraCmd::Reset);
    touched(&mut s);
    let job = s.job;
    drop(s);
    crate::gui::show();
    job
}

fn current(job: u64) -> bool {
    is_open() && studio().job == job
}

fn progress(job: u64, p: Option<f32>, status: Option<&str>) {
    let mut s = studio();
    if s.job != job {
        return;
    }
    s.progress = p.map(|p| p.clamp(0.0, 1.0));
    if let Some(st) = status {
        s.status = st.to_string();
    }
    touched(&mut s);
}

fn set_note(job: u64, note: Option<String>) {
    let mut s = studio();
    if s.job == job {
        s.note = note;
        touched(&mut s);
    }
}

fn set_preview(job: u64, preview: Option<Preview>) {
    let mut s = studio();
    if s.job == job {
        s.preview = preview.map(Arc::new);
        touched(&mut s);
    }
}

/// What a model brings besides its mesh.
#[derive(Default, Clone)]
struct Delivery {
    source: String,
    credit: Option<Credit>,
    planet: Option<Planet>,
    /// Still being composed (more parts follow).
    partial: bool,
    /// The Sketchfab card stays up (the composed model stands in for it).
    keep_preview: bool,
    /// The studio's title (None: keep the one it has).
    title: Option<String>,
    /// The scene it was built from (built-in or composed), for "make it more detailed".
    doc: Option<scene3d::SceneDoc>,
}

/// Shows a model for `job`; false when a newer request or closing the studio came first.
fn deliver(job: u64, model: Model, d: Delivery) -> bool {
    let wire = Arc::new(wire_of(&model, 3000));
    let size_m = model.size_m();
    let model = Arc::new(model);
    let mut s = studio();
    if s.job != job || !is_open() {
        return false;
    }
    s.base = Some(model);
    // A downloaded model's own unit is anybody's guess (an apple came 79 cm tall): its size is
    // that of the kind of thing Sir asked for, and is left unsaid for a kind there is no size
    // for. What JAI builds himself (built-in, composed, a planet) knows its own size.
    let downloaded = d.credit.is_some() && d.planet.is_none();
    s.size_m = if downloaded { crate::object_sizes::real_size(&s.query, size_m) } else { Some(size_m) };
    s.model_id = next_id();
    s.wire = wire;
    s.planet = d.planet;
    if !d.source.is_empty() {
        s.source = d.source;
    }
    if let Some(t) = d.title {
        s.title = t;
    }
    s.credit = d.credit;
    if !d.partial {
        s.doc = d.doc;
    }
    s.phase = if d.partial { Phase::Working } else { Phase::Ready };
    if !d.partial {
        s.progress = None;
        s.status.clear();
        s.placeholder = None;
    }
    if !d.keep_preview {
        s.preview = None;
    }
    if d.planet.is_some() && !d.partial {
        s.view.spin = 0.15;
    }
    rebuild(&mut s);
    touched(&mut s);
    true
}

fn fail(job: u64, why: &str) {
    let mut s = studio();
    if s.job != job {
        return;
    }
    s.phase = if s.base.is_some() { Phase::Ready } else { Phase::Failed };
    s.status = why.to_string();
    s.progress = None;
    if s.base.is_some() {
        s.placeholder = None;
    }
    touched(&mut s);
}

/// The scene again from the base model and the edits.
fn rebuild(s: &mut Studio) {
    let Some(model) = s.base.clone() else {
        s.scene = None;
        return;
    };
    let (materials, textures, part_material) = effective(&model, &s.edits);
    let bounds = model.bounds();
    let part_centres = model.parts.iter().map(|p| p.bounds().centre()).collect();
    let (tilt_deg, atmosphere) = s.planet.map_or((0.0, None), scene3d::planet_look);
    let mut part_names: Vec<String> = Vec::new();
    for p in &model.parts {
        if part_names.len() >= 40 {
            break;
        }
        if !part_names.contains(&p.name) {
            part_names.push(p.name.clone());
        }
    }
    s.scene = Some(Arc::new(Scene {
        model_id: s.model_id,
        model,
        materials,
        textures,
        part_material,
        bounds,
        part_centres,
        planet: s.planet.is_some(),
        tilt_deg,
        atmosphere,
        wire: Arc::clone(&s.wire),
        part_names,
    }));
}

/// The parts an edit's target means.
fn resolve(model: &Model, target: &Target) -> Vec<usize> {
    match target {
        Target::All => (0..model.parts.len()).collect(),
        Target::Named(name) => named_parts(model, name),
        Target::Primary | Target::Secondary => {
            let (primary, secondary) = split_by_area(model);
            if *target == Target::Primary {
                primary
            } else {
                secondary
            }
        }
    }
}

/// German and English part words that mean the same part.
const PART_SYNONYMS: &[(&str, &[&str])] = &[
    ("helmet", &["helm", "head", "kopf", "mask", "maske"]),
    ("head", &["helmet", "helm", "kopf"]),
    ("arm", &["arms", "arme", "arm", "forearm", "upper arm", "unterarm", "oberarm"]),
    ("leg", &["legs", "beine", "bein", "thigh", "shin", "oberschenkel", "schienbein"]),
    ("eye", &["eyes", "augen", "auge"]),
    ("hand", &["hands", "hände", "haende", "glove", "handschuh"]),
    ("chest", &["brust", "torso", "body", "rumpf", "oberkörper"]),
    ("boot", &["boots", "feet", "foot", "füße", "fuesse", "stiefel", "fuß"]),
    ("wheel", &["wheels", "räder", "raeder", "rad", "tyres", "tires", "reifen", "tyre", "tire"]),
    ("wing", &["wings", "flügel", "fluegel", "tragfläche"]),
    ("shoulder", &["shoulders", "schulter", "schultern"]),
    ("reactor", &["arc reactor", "reaktor", "core", "kern"]),
    ("window", &["windows", "fenster", "glass", "scheibe", "windshield", "windscreen"]),
    ("roof", &["dach"]),
    ("door", &["tür", "tuer", "doors", "türen"]),
    ("ring", &["rings", "ringe"]),
    ("fin", &["fins", "flossen", "finne"]),
    ("light", &["lights", "lichter", "headlights", "scheinwerfer", "lamp"]),
    ("panel", &["panels", "solar panel", "solar panels", "solarpanel", "paneele"]),
    ("faceplate", &["face", "gesicht", "visor", "visier", "face plate"]),
];

fn named_parts(model: &Model, name: &str) -> Vec<usize> {
    let n = name.trim().to_lowercase();
    let direct = model.parts_named(&n);
    if !direct.is_empty() {
        return direct;
    }
    for (canonical, others) in PART_SYNONYMS {
        if *canonical == n || others.contains(&n.as_str()) {
            for alt in std::iter::once(*canonical).chain(others.iter().copied()) {
                let hit = model.parts_named(alt);
                if !hit.is_empty() {
                    return hit;
                }
            }
        }
    }
    // "the suit", "the car": the model's own name means all of it.
    let title = model.name.to_lowercase();
    if !title.is_empty() && (title.contains(&n) || n.contains(&title)) {
        return (0..model.parts.len()).collect();
    }
    Vec::new()
}

/// A zone of a model by where it lies, for a name no part carries ("the helmet" of a suit that
/// came as one mesh): a band of the height and a reach from the upright axis, both in heights.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Zone {
    /// From the feet (0) to the crown (1).
    y: (f32, f32),
    /// Distance from the upright axis.
    reach: (f32, f32),
    /// A figure's (a head, arms, legs); the rest any model has (a top, a base).
    figure: bool,
}

/// The zone a part word means and the name its carved part gets.
fn zone_of(word: &str) -> Option<(&'static str, Zone)> {
    const FAR: f32 = 99.0;
    const ZONES: &[(&str, &[&str], Zone)] = &[
        ("helmet", &["helmet", "helm", "head", "kopf", "mask", "maske", "face", "faceplate", "face plate", "gesicht", "visor", "visier"], Zone { y: (0.865, 1.01), reach: (0.0, 0.115), figure: true }),
        ("chest", &["chest", "torso", "brust", "rumpf", "oberkörper", "upper body", "chest plate", "chestplate", "brustplatte"], Zone { y: (0.56, 0.84), reach: (0.0, 0.115), figure: true }),
        ("arms", &["arm", "arms", "arme", "sleeves", "ärmel"], Zone { y: (0.36, 0.86), reach: (0.115, FAR), figure: true }),
        ("legs", &["leg", "legs", "bein", "beine", "trousers", "pants", "hose"], Zone { y: (0.09, 0.48), reach: (0.0, FAR), figure: true }),
        ("boots", &["boot", "boots", "foot", "feet", "shoe", "shoes", "stiefel", "fuß", "füße", "fuesse", "schuhe"], Zone { y: (-0.01, 0.09), reach: (0.0, FAR), figure: true }),
        ("top", &["top", "top part", "upper part", "top third", "oberteil", "spitze", "tip"], Zone { y: (0.66, 1.01), reach: (0.0, FAR), figure: false }),
        ("top half", &["top half", "upper half", "obere hälfte"], Zone { y: (0.5, 1.01), reach: (0.0, FAR), figure: false }),
        ("bottom half", &["bottom half", "lower half", "untere hälfte"], Zone { y: (-0.01, 0.5), reach: (0.0, FAR), figure: false }),
        ("base", &["base", "bottom", "bottom part", "lower part", "bottom third", "unterteil", "sockel", "boden"], Zone { y: (-0.01, 0.34), reach: (0.0, FAR), figure: false }),
        ("middle", &["middle", "middle part", "centre", "center", "mitte", "mittelteil"], Zone { y: (0.34, 0.66), reach: (0.0, FAR), figure: false }),
    ];
    let w = word.trim().to_lowercase();
    ZONES.iter().find(|(_, words, _)| words.contains(&w.as_str())).map(|(name, _, zone)| (*name, *zone))
}

/// A standing figure: taller than it is deep, and no wider than its arms can make it.
fn is_figure(model: &Model) -> bool {
    let s = model.bounds().size();
    let (wide, narrow) = (s[0].max(s[2]), s[0].min(s[2]));
    s[1] > 0.0 && s[1] >= 0.8 * wide && s[1] >= 1.3 * narrow
}

/// Cuts the triangles lying in `zone` out of their parts into parts of their own called `name`
/// (same materials, so nothing looks different until they are painted). How many it took.
fn carve(model: &mut Model, name: &str, zone: Zone) -> usize {
    let b = model.bounds();
    let h = (b.max[1] - b.min[1]).max(1e-9);
    // The upright axis: a figure's runs through its crown (it may hold something to one side).
    let mut axis = [b.centre()[0], b.centre()[2]];
    if zone.figure {
        let crown = b.max[1] - 0.06 * h;
        let (mut sum, mut n) = ([0.0f64; 2], 0u64);
        for p in model.parts.iter().flat_map(|part| &part.positions).filter(|p| p[1] >= crown) {
            sum[0] += p[0] as f64;
            sum[1] += p[2] as f64;
            n += 1;
        }
        if n > 0 {
            axis = [(sum[0] / n as f64) as f32, (sum[1] / n as f64) as f32];
        }
    }
    let inside = |p: [f32; 3]| {
        let y = (p[1] - b.min[1]) / h;
        let r = ((p[0] - axis[0]).powi(2) + (p[2] - axis[1]).powi(2)).sqrt() / h;
        y >= zone.y.0 && y <= zone.y.1 && r >= zone.reach.0 && r <= zone.reach.1
    };
    let mut parts = Vec::with_capacity(model.parts.len() + 4);
    let mut taken = 0usize;
    for part in std::mem::take(&mut model.parts) {
        let in_zone: Vec<bool> = part
            .indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (part.positions[t[0] as usize], part.positions[t[1] as usize], part.positions[t[2] as usize]);
                inside([(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0])
            })
            .collect();
        let count = in_zone.iter().filter(|x| **x).count();
        if count == 0 {
            parts.push(part);
            continue;
        }
        taken += count;
        let mut cut = triangles_of(&part, |i| in_zone[i]);
        cut.name = name.to_string();
        if count < in_zone.len() {
            parts.push(triangles_of(&part, |i| !in_zone[i]));
        }
        parts.push(cut);
    }
    model.parts = parts;
    taken
}

/// The part with only the triangles `keep` says, and only the vertices they use.
fn triangles_of(part: &Part, keep: impl Fn(usize) -> bool) -> Part {
    let mut out = Part { name: part.name.clone(), material: part.material, ..Part::default() };
    let mut slot = vec![u32::MAX; part.positions.len()];
    for (i, t) in part.indices.chunks_exact(3).enumerate() {
        if !keep(i) {
            continue;
        }
        for &v in t {
            let v = v as usize;
            if slot[v] == u32::MAX {
                slot[v] = out.positions.len() as u32;
                out.positions.push(part.positions[v]);
                if let Some(n) = part.normals.get(v) {
                    out.normals.push(*n);
                }
                if let Some(uv) = part.uvs.get(v) {
                    out.uvs.push(*uv);
                }
                if let Some(t) = part.tangents.get(v) {
                    out.tangents.push(*t);
                }
                if let Some(c) = part.colors.get(v) {
                    out.colors.push(*c);
                }
            }
            out.indices.push(slot[v]);
        }
    }
    out
}

/// "The helmet" on a model that has no such part: carved out by where a helmet is. The name the
/// edit then targets, or why there is none.
fn carve_zone(word: &str) -> Result<String, String> {
    let (name, zone) = zone_of(word).ok_or_else(|| format!("I can't find a part called \"{word}\""))?;
    let mut s = studio();
    let Some(base) = s.base.clone() else { return Err("nothing is in the studio".into()) };
    if zone.figure && !is_figure(&base) {
        return Err(format!("this model has no {word} I can single out"));
    }
    let mut model = (*base).clone();
    let total = model.triangles().max(1);
    let taken = carve(&mut model, name, zone);
    // Nothing there, or all of it: where the zone lies says nothing about this model.
    if taken == 0 || taken * 100 >= total * 97 {
        return Err(format!("I can't single out the {word} on this model"));
    }
    info!("[3D] Carved the {name} out of '{}' by its place: {taken} of {total} triangles", s.title);
    s.base = Some(Arc::new(model));
    s.model_id = next_id();
    Ok(name.to_string())
}

/// The parts that make up at least 60 % of the surface (largest first), and the rest.
fn split_by_area(model: &Model) -> (Vec<usize>, Vec<usize>) {
    if model.parts.len() < 2 {
        return ((0..model.parts.len()).collect(), Vec::new());
    }
    // By material where materials are shared (a suit's red plates, its gold ones).
    let mut by_material: HashMap<usize, (f32, Vec<usize>)> = HashMap::new();
    for (i, p) in model.parts.iter().enumerate() {
        let e = by_material.entry(p.material).or_insert((0.0, Vec::new()));
        e.0 += p.area();
        e.1.push(i);
    }
    let mut groups: Vec<(f32, Vec<usize>)> = if by_material.len() >= 2 { by_material.into_values().collect() } else { model.parts.iter().enumerate().map(|(i, p)| (p.area(), vec![i])).collect() };
    groups.sort_by(|a, b| b.0.total_cmp(&a.0));
    let total: f32 = groups.iter().map(|g| g.0).sum::<f32>().max(1e-12);
    let (mut primary, mut secondary, mut acc) = (Vec::new(), Vec::new(), 0.0);
    for (k, (area, parts)) in groups.into_iter().enumerate() {
        if k == 0 || acc / total < 0.6 {
            primary.extend(parts);
            acc += area;
        } else {
            secondary.extend(parts);
        }
    }
    if secondary.is_empty() && primary.len() > 1 {
        // One material holds everything: the largest parts carry the first colour.
        let mut by_part: Vec<(f32, usize)> = model.parts.iter().enumerate().map(|(i, p)| (p.area(), i)).collect();
        by_part.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (mut a, mut p2, mut s2) = (0.0, Vec::new(), Vec::new());
        for (area, i) in by_part {
            if p2.is_empty() || a / total < 0.6 {
                p2.push(i);
                a += area;
            } else {
                s2.push(i);
            }
        }
        return (p2, s2);
    }
    (primary, secondary)
}

/// The materials after the edits: (materials, textures, each part's material).
fn effective(model: &Model, edits: &[Edit]) -> (Vec<Material>, Vec<Texture>, Vec<usize>) {
    let mut materials = model.materials.clone();
    let mut textures = model.textures.clone();
    let mut part_material: Vec<usize> = model.parts.iter().map(|p| p.material).collect();
    let mut slots = HashMap::new();
    for edit in edits {
        let parts = resolve(model, &edit.target);
        let mut remap: HashMap<usize, usize> = HashMap::new();
        for p in parts {
            let old = part_material[p];
            let new = *remap.entry(old).or_insert_with(|| {
                let mut m = materials[old].clone();
                apply_change(&mut m, &edit.change, &mut textures, &mut slots);
                materials.push(m);
                materials.len() - 1
            });
            part_material[p] = new;
        }
    }
    (materials, textures, part_material)
}

fn apply_change(m: &mut Material, change: &Change, textures: &mut Vec<Texture>, slots: &mut HashMap<pt::Pattern, [Option<usize>; 4]>) {
    if matches!(change, Change::Paint(_)) {
        // Paint covers a photo's vertex colours too.
        m.vertex_colors = false;
    }
    match change {
        Change::Paint(Paint::Finish(f)) => scene3d::apply_finish(m, f, textures, slots),
        Change::Paint(Paint::Colour(_, c)) => {
            // Paint covers a photo texture; a pattern ("carbon fibre") is tinted instead.
            if !m.triplanar {
                m.base_color_tex = None;
            }
            let alpha = if m.alpha == AlphaMode::Blend { m.base_color[3] } else { 1.0 };
            m.base_color = [c[0], c[1], c[2], alpha];
        }
        Change::Glow(colour) => {
            let base = [m.base_color[0], m.base_color[1], m.base_color[2]];
            let bright = base.iter().cloned().fold(0.0f32, f32::max);
            let c = colour.unwrap_or(if bright < 0.08 { [0.55, 0.9, 1.0] } else { [base[0] / bright, base[1] / bright, base[2] / bright] });
            m.emissive = [c[0] * 3.0, c[1] * 3.0, c[2] * 3.0];
            m.emissive_tex = None;
        }
        Change::NoGlow => {
            m.emissive = [0.0; 3];
            m.emissive_tex = None;
        }
    }
}

/// Up to `max` edges of the model in the normalised frame (centred, radius 1).
fn wire_of(model: &Model, max: usize) -> Vec<[[f32; 3]; 2]> {
    let b = model.bounds();
    let (c, r) = (b.centre(), b.radius().max(1e-9));
    let n = |p: [f32; 3]| [(p[0] - c[0]) / r, (p[1] - c[1]) / r, (p[2] - c[2]) / r];
    let tris = model.triangles().max(1);
    let stride = (tris * 3).div_ceil(max).max(1);
    let mut out = Vec::with_capacity(max.min(tris * 3));
    let mut k = 0usize;
    for part in &model.parts {
        for t in part.indices.chunks_exact(3) {
            k += 1;
            if k % stride != 0 {
                continue;
            }
            let v = [n(part.positions[t[0] as usize]), n(part.positions[t[1] as usize]), n(part.positions[t[2] as usize])];
            out.push([v[0], v[1]]);
            out.push([v[1], v[2]]);
            out.push([v[2], v[0]]);
        }
    }
    out
}

/// A scanning lattice for "working on it" with nothing better to show.
fn scan_sphere() -> Vec<[[f32; 3]; 2]> {
    let mut out = Vec::new();
    let ring = |out: &mut Vec<[[f32; 3]; 2]>, f: &dyn Fn(f32) -> [f32; 3]| {
        let n = 48;
        for k in 0..n {
            let (a, b) = (k as f32 / n as f32 * std::f32::consts::TAU, (k + 1) as f32 / n as f32 * std::f32::consts::TAU);
            out.push([f(a), f(b)]);
        }
    };
    for lat in [-60.0f32, -30.0, 0.0, 30.0, 60.0] {
        let (y, r) = (lat.to_radians().sin() * 0.8, lat.to_radians().cos() * 0.8);
        ring(&mut out, &|a| [r * a.cos(), y, r * a.sin()]);
    }
    for lon in 0..6 {
        let l = lon as f32 / 6.0 * std::f32::consts::PI;
        ring(&mut out, &|a| [0.8 * a.cos() * l.cos(), 0.8 * a.sin(), 0.8 * a.cos() * l.sin()]);
    }
    out
}

// ------------------------------------------------------------------------------ phrases ----

/// What a sentence asks of the studio, as the tool's arguments. `open`: the studio is up (edits
/// and camera moves only then); `earth_open`: the Earth panel is up ("zoom in" is its own then).
#[cfg(test)]
pub fn command(text: &str, open: bool, earth_open: bool) -> Option<Value> {
    command_with(text, open, earth_open, &[])
}

/// `command`, with the names that are not paint even though they hold a paint word (the
/// model's title and its part names: "the Iron Man suit" is not "iron").
fn command_with(text: &str, open: bool, earth_open: bool, names: &[String]) -> Option<Value> {
    if let Some(path) = path_in(text) {
        let t = clean(text);
        if OPEN_VERB.is_match(&t) || t.contains("3d") {
            return Some(json!({ "path": path }));
        }
    }
    let t = clean(text);
    if t.is_empty() {
        return None;
    }
    if let Some(args) = open_command(&t) {
        return Some(args);
    }
    if !open {
        return None;
    }
    if let Some(args) = correction(&t) {
        return Some(args);
    }
    let mut args = edit_command(&t, earth_open, names)?;
    if args["action"] == "export" {
        // (The folder as he said it: the cleaned sentence has lost its case and colons.)
        if let Some(folder) = folder_in(text) {
            args["folder"] = json!(folder);
        }
    }
    Some(args)
}

/// The thing a paint order names when that is not his interface: "make the helmet yellow" →
/// "helmet". None for "make it yellow", "make the theme yellow", "yellow, please". (The brain
/// holds an interface recolour back for such a sentence: 2026-11-09, the model turned the whole
/// HUD yellow and said the helmet was.)
pub fn painted_thing(text: &str) -> Option<String> {
    let args = command_with(text, true, false, &[])?;
    let mut parts: Vec<&str> = args["part"].as_str().into_iter().collect();
    if let Some(list) = args["edits"].as_array() {
        parts.extend(list.iter().filter_map(|e| e["part"].as_str()));
    }
    const HIS: &[&str] = &["you", "yourself", "your", "jai", "jai", "jay", "everything", "colour", "color", "colours", "colors", "dich", "dein", "deine", "deinen", "alles", "farbe"];
    parts.into_iter().find(|p| !p.split_whitespace().any(|w| HUD_WORDS.contains(&w) || HIS.contains(&w))).map(str::to_string)
}

/// "No, not an iPad, I mean a real apple", "I meant the fruit": the model on show was the wrong
/// thing; the one meant, and what it is not.
fn correction(t: &str) -> Option<Value> {
    static NOT_BUT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:no\s+|nope\s+|nein\s+)?(?:that\s+is\s+|that's\s+|thats\s+|it\s+is\s+|it's\s+)?(?:not|nicht|kein|keine|keinen)\s+(.+?)\s+(?:i\s+(?:mean|meant|want|wanted|said|asked\s+for)|but|rather|give\s+me|show\s+me|sondern|ich\s+(?:meine|meinte|will|wollte))\s+(.+)$")
    });
    static I_MEAN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:no\s+|nope\s+|nein\s+)?(?:i\s+(?:mean|meant)|ich\s+(?:meine|meinte))\s+(.+)$"));
    if let Some(c) = NOT_BUT.captures(t) {
        let (not, meant) = (subject(&c[1])?, subject(&c[2])?);
        return Some(json!({ "query": meant, "avoid": [not] }));
    }
    let meant = subject(&I_MEAN.captures(t)?[1].to_string())?;
    Some(json!({ "query": meant }))
}

/// The fast path: a ready intent for the brain. (Unit tests of other modules never see a studio
/// that a test of this one opened.)
pub fn intent(text: &str) -> Option<Intent> {
    // A formula in 3D ("show me z = x² + y² in 3D") is the Simulation Lab's surface.
    if text.contains('=') {
        return None;
    }
    let open = is_open() && !cfg!(test);
    let (names, model) = if open {
        let s = studio();
        let mut names = vec![s.title.to_lowercase()];
        if let Some(m) = &s.base {
            names.extend(m.parts.iter().map(|p| p.name.to_lowercase()).filter(|n| n.contains(' ')).take(200));
        }
        (names, s.base.clone())
    } else {
        (Vec::new(), None)
    };
    let args = command_with(text, open, crate::earth::is_open(), &names)?;
    // "Make the note gold" is not an edit unless the model has such a part, or the word names
    // a place on it ("the helmet" of a suit in one piece; 2026-11-09: that sentence went to the
    // model, which turned the whole interface yellow).
    let mut asked: Vec<&str> = args["part"].as_str().into_iter().collect();
    if let Some(list) = args["edits"].as_array() {
        asked.extend(list.iter().filter_map(|e| e["part"].as_str()));
    }
    if !asked.is_empty() {
        let model = model?;
        if asked.iter().any(|p| named_parts(&model, p).is_empty() && zone_of(p).is_none()) {
            return None;
        }
    }

    Some(Intent::Tool { name: TOOL, args })
}

/// Phrases that name things, not paint ("Iron Man", "Silver Surfer"), and the given names.
fn mask(normed: &str, names: &[String]) -> String {
    const PROTECTED: &[&str] = &["iron man", "ironman", "man of steel", "silver surfer", "golden gate", "golden gate bridge", "iron giant", "red planet", "black hole", "white house", "green lantern", "black panther", "red bull", "gold rush", "black widow", "iron maiden", "steel tower", "silver arrow"];
    let mut out = normed.to_string();
    for phrase in PROTECTED.iter().map(|p| p.to_string()).chain(names.iter().map(|n| scene3d::norm(n).trim().to_string())) {
        if phrase.is_empty() || !phrase.contains(' ') {
            continue;
        }
        let needle = format!(" {phrase} ");
        let joined = format!(" {} ", phrase.replace(' ', "_"));
        while let Some(i) = out.find(&needle) {
            out.replace_range(i..i + needle.len(), &joined);
        }
    }
    out
}

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

/// Lower case, the wake word and politeness gone, "three d" / "3-D" / "3D-Modell" as "3d".
fn clean(text: &str) -> String {
    let mut t = text.to_lowercase().replace(['’', '`'], "'");
    t = t.replace(|c: char| matches!(c, ',' | '!' | '?' | '"' | ';' | '(' | ')'), " ");
    // (A drive's colon stays: "d:\prints".)
    t = t.split(':').enumerate().map(|(i, part)| if i > 0 && part.starts_with(['\\', '/']) { format!(":{part}") } else if i > 0 { format!(" {part}") } else { part.to_string() }).collect();
    t = t.trim().trim_end_matches('.').to_string();
    static THREE_D: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:three[\s-]?d|drei[\s-]?d|3[\s-]d)\b|\b3d-"));
    t = THREE_D.replace_all(&t, "3d ").to_string();
    t = t.replace('-', " ");
    // A word said twice is said once ("a 3D model, model of the Eiffel Tower").
    let mut words: Vec<&str> = t.split_whitespace().collect();
    words.dedup();
    let mut t: String = words.join(" ");
    static LEAD: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:hey|ok|okay|yo|hi|hallo)\s+)?(?:jai|jay)\s*(?:please\s+|bitte\s+)?|^(?:please|bitte|now|jetzt|and|und|also|so)\s+|^(?:can|could|would|will)\s+you\s+(?:please\s+)?|^(?:i\s+want\s+you\s+to|i'd\s+like\s+you\s+to|i\s+would\s+like\s+you\s+to|go\s+ahead\s+and)\s+|^(?:kannst|könntest|koenntest|würdest|wuerdest)\s+du\s+(?:bitte\s+)?(?:mal\s+)?")
    });
    static TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"\s+(?:please|bitte|for\s+me|für\s+mich|jai|mal|now|jetzt|sir)$"));
    for _ in 0..4 {
        let before = t.clone();
        t = LEAD.replace(&t, "").trim().to_string();
        t = TAIL.replace(&t, "").trim().to_string();
        if let Some(rest) = crate::intents::without_lost_can(&t) {
            t = rest;
        }
        if t == before {
            break;
        }
    }
    t
}

/// A model file path in the sentence ("C:\Users\…\suit.glb", "D:/models/x.stl").
fn path_in(text: &str) -> Option<String> {
    static PATH: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)((?:[a-z]:[\\/]|\\\\|~[\\/])[^"<>|*?\r\n]*?\.(?:glb|gltf|obj|stl|zip))\b"#));
    PATH.captures(text).map(|c| c[1].trim().to_string())
}

static OPEN_VERB: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:open|load|show|render|display|import|view|bring up|pull up|zeig|zeige|öffne|oeffne|lade|render|rendere)\b"));

/// The subject of an opening request, cleaned ("an Iron Man suit" → "iron man suit").
fn subject(raw: &str) -> Option<String> {
    static ART: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:a|an|the|some|one|my|ein|eine|einen|einem|einer|eines|der|die|das|den|dem|des|vom|von|von\s+der|von\s+dem|von\s+einem|von\s+einer)\s+"));
    static END: LazyLock<Regex> = LazyLock::new(|| re(r"\s+(?:for\s+me|please|bitte|now|jetzt|in\s+3d|als\s+3d\s+modell|as\s+a\s+3d\s+model|model|modell|real\s+quick|quickly|schnell|in\s+(?:a|the)\s+(?:new\s+|separate\s+|own\s+)?window|in\s+einem\s+(?:neuen\s+|eigenen\s+)?fenster)$"));
    let mut s = raw.trim().to_string();
    for _ in 0..3 {
        s = ART.replace(&s, "").trim().to_string();
        s = END.replace(&s, "").trim().to_string();
    }
    let s = s.trim_matches(|c: char| c == '.' || c == '\'' || c.is_whitespace()).to_string();
    (!s.is_empty() && s.len() <= 120 && s != "it" && s != "es" && s != "that" && s != "this").then_some(s)
}

/// "Render me X in 3D", "show me a 3D model of X", "zeig mir X in 3D", "3D-Modell von X",
/// "open the 3D studio", "the model in my Downloads".
fn open_command(t: &str) -> Option<Value> {
    const VERBS: &str = r"(?:render|draw|build|make|create|model|generate|show|display|bring\s+up|pull\s+up|give|put\s+up|load|open|visuali[sz]e|project|zeig|zeige|zeigt|rendere|bau|baue|erstelle|erstell|mach|mache|generiere|modelliere|öffne|oeffne|lade|stell\s+dar)";
    static OF_3D: LazyLock<Regex> = LazyLock::new(|| {
        re(&format!(r"^{VERBS}(?:\s+(?:me|us|mir|uns))?(?:\s+up)?\s+(?:a|an|the|some|this|that|another|your\s+own|your|a\s+new|ein|eine|einen|das|den|die|dein\s+eigenes|dein)?\s*3d\s+(?:model|modell|render|rendering|version|view|ansicht|hologram|hologramm|replica|scan)\s+(?:of|von|vom|from)\s+(.+)$"))
    });
    // "Show me the model of an eatable apple" (2026-11-09: without "3D" it went to the model).
    static MODEL_OF: LazyLock<Regex> = LazyLock::new(|| {
        re(&format!(r"^{VERBS}(?:\s+(?:me|us|mir|uns))?(?:\s+up)?\s+(?:a|an|the|this|that|another|ein|eine|einen|das|den|die)\s+(?:model|modell|render|rendering|replica)\s+(?:of|von|vom)\s+(.+)$"))
    });
    static IN_3D: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{VERBS}(?:\s+(?:me|us|mir|uns))?(?:\s+up)?\s+(.+?)\s+(?:in|als|as)\s+(?:a\s+)?3d(?:\s+(?:model|modell|hologram|hologramm))?$")));
    static A_3D: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{VERBS}(?:\s+(?:me|us|mir|uns))?\s+(?:a|an|the|ein|eine|einen|das|den|die)\s+3d\s+(.+)$")));
    static BARE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:a|an|the|ein|das)?\s*3d\s+(?:model|modell)\s+(?:of|von|vom|der|des)\s+(.+)$"));
    static RENDER_ME: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:render|rendere)\s+(?:me|mir)\s+(.+)$"));
    static STUDIO: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:open|show|start|launch|bring\s+up|pull\s+up|öffne|oeffne|starte|zeig|zeige)(?:\s+(?:me|mir))?(?:\s+(?:the|your|my|das|dein|die|den|deine))?\s+(?:3d\s+studio|3d\s+viewer|model\s+viewer|3d\s+model\s+viewer|3d\s+workshop|3d\s+lab|3d\s+werkstatt|3d\s+labor|3d\s+ansicht|modellansicht)$")
    });
    static DOWNLOADS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:open|load|show|render|display|import|view|zeig|zeige|öffne|oeffne|lade|rendere)(?:\s+(?:me|mir))?\s+(?:the|my|that|this|das|die|den|mein|meine|meinen)?\s*(?:3d\s+)?(?:model|file|stl|obj|glb|gltf|modell|datei|stl\s+file|obj\s+file|glb\s+file|model\s+file|3d\s+file|3d\s+datei)\s+(?:i\s+(?:just\s+)?downloaded|(?:in|from)\s+(?:my\s+)?downloads?(?:\s+folder)?|(?:aus|in)\s+(?:meinen|meinem|den)?\s*downloads?(?:\s+ordner)?|(?:das|die|den)\s+ich\s+(?:gerade\s+)?heruntergeladen\s+habe)$")
    });
    if STUDIO.is_match(t) {
        return Some(json!({ "action": "show" }));
    }
    if DOWNLOADS.is_match(t) {
        return Some(json!({ "path": "downloads" }));
    }
    // ("Make the 3D model bigger" is about the model on show, not one called "model bigger".)
    let a_3d = || A_3D.captures(t).filter(|c| !matches!(c[1].split_whitespace().next(), Some("model" | "modell" | "studio" | "viewer")));
    let raw = OF_3D.captures(t).or_else(|| IN_3D.captures(t)).or_else(a_3d).or_else(|| BARE.captures(t)).or_else(|| MODEL_OF.captures(t)).map(|c| c[1].to_string());
    let raw = match raw {
        Some(r) => r,
        None => {
            // "Render me an Iron Man suit": render alone is the studio's word, unless it is
            // about a picture, a video or a song.
            let c = RENDER_ME.captures(t)?;
            let r = c[1].to_string();
            if ["video", "image", "picture", "photo", "song", "audio", "mp4", "frame", "bild", "foto"].iter().any(|w| r.split_whitespace().any(|x| x == *w)) {
                return None;
            }
            r
        }
    };
    let q = subject(&raw)?;
    // "Show me the weather in 3D" is still a request for the studio; "show it in 3D" has no subject.
    let (q, material) = split_material(&q);
    let mut args = json!({ "query": q });
    if let Some(m) = material {
        args["material"] = json!(m);
    }
    Some(args)
}

/// "an Iron Man suit in gold", "a car made of carbon fibre": the subject and its finish.
fn split_material(q: &str) -> (String, Option<String>) {
    static TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+?)\s+(?:in|made\s+of|out\s+of|aus|with\s+a|with)\s+(.+?)(?:\s+(?:finish|texture|paint|colou?r|look))?$"));
    if let Some(c) = TAIL.captures(q) {
        let tail = c[2].to_string();
        let paints = scene3d::paints_at(&scene3d::norm(&tail));
        // The whole tail must be paints and joining words ("red and gold").
        let mut rest = scene3d::norm(&tail);
        for (at, len, _) in paints.iter().rev() {
            rest.replace_range(*at..*at + *len, " ");
        }
        if !paints.is_empty() && rest.split_whitespace().all(|w| ["and", "und", "with", "mit", "a", "an"].contains(&w)) {
            return (c[1].to_string(), Some(tail));
        }
    }
    (q.to_string(), None)
}

/// Words that mean the HUD, not the model ("make the theme red").
const HUD_WORDS: &[&str] = &[
    "theme", "interface", "hud", "orb", "core", "background", "window", "windows", "text", "board", "console", "accent", "wallpaper", "desktop", "screen", "font", "cursor", "icon", "icons", "emblem", "ui", "farbschema", "hintergrund", "oberfläche", "schrift", "bildschirm", "fenster", "design", "lights", "lamps", "licht", "lampen", "volume", "music",
];

fn edit_command(t: &str, earth_open: bool, names: &[String]) -> Option<Value> {
    // "The 3D model" is the model.
    let said = t.replace("the 3d model", "the model").replace("das 3d modell", "das modell");
    let t = said.as_str();
    let words: Vec<&str> = t.split_whitespace().collect();
    let has = |w: &str| words.contains(&w);
    let mentions_model = ["model", "it", "modell", "es", "the suit", "the planet"].iter().any(|w| has(w)) || t.contains("the model") || t.contains("das modell");
    // Closing and resetting.
    static CLOSE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:close|hide|dismiss|exit|quit|schließ|schließe|schliess|schliesse|mach|mache)(?:\s+(?:the|your|das|den|die|dein))?\s+(?:3d\s+)?(?:model|studio|viewer|model\s+viewer|3d|modell|ansicht)(?:\s+(?:window|fenster))?(?:\s+zu)?$"));
    if CLOSE.is_match(t) {
        return Some(json!({ "action": "close" }));
    }
    static RESET: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:reset|restore|revert|undo\s+(?:all|everything)(?:\s+changes)?)(?:\s+(?:it|the\s+model|the\s+view|the\s+colou?rs|the\s+look|the\s+materials?|everything|all))?(?:\s+to\s+(?:the\s+)?(?:original|default)s?)?$|^(?:go\s+)?back\s+to\s+(?:the\s+)?original(?:\s+(?:look|colou?rs|materials?))?$|^(?:setz|setze)\s+(?:es|das\s+modell|alles|ihn|sie)\s+zurück$|^(?:zurücksetzen|original\s+wiederherstellen|alles\s+zurücksetzen)$")
    });
    if RESET.is_match(t) {
        return Some(json!({ "action": "reset" }));
    }
    // Export.
    static EXPORT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:save|export|write|store|download|speicher|speichere|exportier|exportiere|sichere)(?:\s+(?:it|the\s+model|this|that|the\s+suit|es|das\s+modell|ihn|sie|das))?(?:\s+(?:as|to|into|in|als|im|for|für))?(?:\s+(?:an?|eine?|einen|the|das))?\s*(stl|obj|glb|gltf|3mf)?(?:\s+(?:file|datei|format))?(.*)$")
    });
    if let Some(c) = EXPORT.captures(t) {
        let rest = c.get(2).map_or("", |m| m.as_str());
        let printer = t.contains("printer") || t.contains("printing") || t.contains("print it") || t.contains("drucker") || t.contains("drucken");
        // "Save it to my desktop", "save it somewhere else": the printer's format.
        let format = c.get(1).map(|m| m.as_str()).or((printer || folder_in(rest).is_some()).then_some("stl"));
        if let Some(fmt) = format {
            let fmt = if fmt == "gltf" { "glb" } else if fmt == "3mf" { "stl" } else { fmt };
            let mut args = json!({ "action": "export", "format": fmt });
            if let Some(cm) = height_cm(rest) {
                args["height_cm"] = json!(cm);
            }
            if let Some(k) = scale_ratio(rest) {
                args["scale"] = json!(k);
            }
            return Some(args);
        }
    }
    static MEASURE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:measure(?:\s+(?:it|the\s+model|that|this))?|how\s+(?:big|tall|large|wide|long|high)\s+(?:is|would)\s+(?:it|the\s+model|that|this)(?:\s+be)?(?:\s+in\s+real\s+life)?|what\s+(?:are|is)\s+(?:its|the\s+model'?s|the)\s+(?:size|dimensions?|height|measurements?)|(?:its|the)\s+dimensions|wie\s+(?:groß|gross|hoch|breit|lang)\s+ist\s+(?:es|das|das\s+modell|er|sie)(?:\s+in\s+echt)?|(?:miss|vermiss|vermesse)\s+(?:es|das|das\s+modell|ihn|sie))$")
    });
    if MEASURE.is_match(t) {
        return Some(json!({ "action": "measure" }));
    }
    static PARTS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what|which)\s+parts\s+(?:does\s+it\s+have|has\s+it\s+got|are\s+there)|^list\s+(?:the|its|all)\s+parts$|^what\s+is\s+it\s+made\s+of$|^(?:welche|aus\s+welchen)\s+teile"));
    if PARTS.is_match(t) {
        return Some(json!({ "action": "parts" }));
    }
    // "Make it (way) more detailed", "a more accurate version", "enhance it", "mach es detaillierter".
    static ENHANCE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:make|render|build|do|redo|give|give\s+me|show\s+me|mach|mache)\s+(?:it|this|that|the\s+model|a|one|es|das(?:\s+modell)?)?\s*(?:a\s+lot\s+|way\s+|much\s+|even\s+|far\s+|a\s+bit\s+|a\s+little\s+|viel\s+|noch\s+|etwas\s+)*(?:more\s+(?:detailed|accurate|realistic|precise|lifelike|refined)|(?:more|higher|finer|extra)\s+detail|better|sharper|detaillierter|genauer|realistischer|besser|feiner)(?:\s+(?:version|model|one|modell))?$|^(?:enhance|refine|improve|upgrade|detail|verbessere|verfeinere)(?:\s+(?:it|this|that|the\s+model|es|das\s+modell))?(?:\s+(?:more|further|even\s+more|weiter))?$|^(?:more|mehr|even\s+more|noch\s+mehr)\s+details?$|^(?:add|füge|fuege)\s+(?:more|mehr)\s+details?(?:\s+(?:to\s+it|hinzu))?$")
    });
    if ENHANCE.is_match(t) {
        return Some(json!({ "action": "enhance" }));
    }
    if HUD_WORDS.iter().any(|w| has(w)) {
        return None;
    }
    // The look.
    static HOLO: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:switch|go|change)\s+(?:to|into)\s+|(?:make|turn)\s+(?:it|the\s+model)\s+(?:into\s+)?(?:a\s+)?|(?:show|render|display)\s+(?:it|the\s+model)\s+as\s+(?:a\s+)?|(?:mach|zeig|zeige)\s+(?:es|das\s+modell)\s+(?:als\s+)?(?:ein\s+)?|(?:wechsel|wechsle)\s+(?:zu|in\s+den)\s+|as\s+a\s+|als\s+)?(?:holo(?:gram|gramm|graphic|grafisch)?)(?:\s+(?:mode|view|look|modus|ansicht))?(?:\s+(?:mode|modus))?$"));
    if HOLO.is_match(t) {
        return Some(json!({ "action": "edit", "look": "hologram" }));
    }
    static REAL: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:switch|go|change)\s+(?:back\s+)?(?:to|into)\s+|(?:make|turn)\s+(?:it|the\s+model)\s+|(?:show|render)\s+(?:it|the\s+model)\s+(?:as\s+)?|back\s+to\s+|(?:mach|zeig|zeige)\s+(?:es|das\s+modell)\s+|zurück\s+zu[mr]?\s+)?(?:realistic|photo\s?real(?:istic)?|realistisch|realistische[nr]?|(?:normal|solid|normale[nr]?)\s+(?:look|view|render(?:ing)?|ansicht|darstellung))(?:\s+(?:mode|view|look|modus|ansicht|again|wieder))?$"));
    if REAL.is_match(t) {
        return Some(json!({ "action": "edit", "look": "realistic" }));
    }
    // Wireframe.
    if t.contains("wireframe") || t.contains("wire frame") || t.contains("drahtgitter") || t.contains("gittermodell") || t == "mesh view" {
        let off = ["off", "no", "aus", "without", "hide", "remove", "ohne", "weg", "stop", "disable"].iter().any(|w| has(w)) || t.starts_with("no ") || t.contains("turn off");
        return Some(json!({ "action": "edit", "wireframe": !off }));
    }
    // Spin. (Not "turn": "turn it all the way up" is the volume's; not "dreh auf" either.)
    static SPIN: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:spin|spins|spinning|rotate|rotates|rotating|rotation|turntable|drehung|rotier|rotiere|rotieren|rotiert|dreh\s+dich|drehe\s+dich|dreht\s+sich|drehen|drehen\s+lassen)\b"));
    let spinning = SPIN.is_match(t);
    if spinning || ["hold still", "freeze it", "freeze", "halt still", "halt es an", "halte es an"].contains(&t) {
        let faster = ["faster", "quicker", "speed up", "schneller"].iter().any(|w| t.contains(w));
        let slower = ["slower", "slow down", "langsamer"].iter().any(|w| t.contains(w));
        let stop = ["stop", "halt", "freeze", "pause", "hold still", "no more", "nicht mehr", "hör auf", "hoer auf", "anhalten", "stopp", "aufhören"].iter().any(|w| t.contains(w));
        // "Turn it to the side" is a view, "turn it gold" an edit.
        if spinning && !faster && !slower && !stop && (t.contains(" to the ") || !scene3d::paints_in(t).is_empty() || t.contains("turn off") || t.contains("turn on")) {
            // fall through
        } else {
            let spin = if faster {
                "faster"
            } else if slower {
                "slower"
            } else if stop {
                "stop"
            } else {
                "start"
            };
            if spinning || stop {
                return Some(json!({ "action": "edit", "spin": spin }));
            }
        }
    }
    // Zoom.
    static ZOOM_IN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:zoom(?:\s+in)?|zoom\s+in\s+(?:on|to)\s+(?:it|the\s+model)|(?:get|move|come)\s+closer|closer|rein\s?zoomen|hineinzoomen|zoom\s+rein|näher(?:\s+ran)?|naeher|ran\s?zoomen)$"));
    static ZOOM_OUT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:zoom\s+out|zoom\s+out\s+(?:of|from)\s+(?:it|the\s+model)|(?:move|go)\s+(?:back|further\s+away)|further(?:\s+away)?|raus\s?zoomen|herauszoomen|zoom\s+raus|weiter\s+weg)$"));
    if ZOOM_IN.is_match(t) || ZOOM_OUT.is_match(t) {
        if earth_open && !mentions_model {
            return None;
        }
        return Some(json!({ "action": "edit", "view": if ZOOM_OUT.is_match(t) { "zoom_out" } else { "zoom_in" } }));
    }
    // Views.
    static VIEW: LazyLock<Regex> = LazyLock::new(|| {
        // (A bare "right" or "back" is a reply, not a view: a view is asked "from" somewhere.)
        re(r"^(?:(?:show|view|look\s+at|turn|rotate|see)\s+(?:me\s+)?(?:it|the\s+model|that)?\s*(?:from|to)\s+(?:the\s+)?|(?:zeig|zeige)\s+(?:es\s+|mir\s+es\s+|es\s+mir\s+|mir\s+das\s+modell\s+|das\s+modell\s+)?von\s+|from\s+(?:the\s+)?|von\s+)(front|back|rear|side|left|right|top|above|bottom|below|underneath|vorne|vorn|hinten|der\s+seite|seite|links|rechts|oben|unten)(?:\s+(?:view|side|ansicht))?$|^(front|side|top|back|rear|bottom|vorder|seiten|drauf|rück|unter)\s?(?:view|ansicht|sicht)$")
    });
    if let Some(c) = VIEW.captures(t) {
        let w = c.get(1).or_else(|| c.get(2)).map_or("", |m| m.as_str());
        let view = match w {
            "front" | "vorne" | "vorn" | "vorder" => "front",
            "back" | "rear" | "hinten" | "rück" => "back",
            "side" | "right" | "rechts" | "der seite" | "seite" | "seiten" => "right",
            "left" | "links" => "left",
            "top" | "above" | "oben" | "drauf" => "top",
            _ => "bottom",
        };
        return Some(json!({ "action": "edit", "view": view }));
    }
    // Explode.
    static EXPLODE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:explode(?:\s+(?:it|the\s+model|the\s+view))?|(?:show\s+(?:me\s+)?)?(?:an?\s+|the\s+)?explo(?:ded|sion)\s+view|(?:take|pull|break|blow)\s+(?:it|the\s+model)\s+apart|(?:show|spread)\s+(?:me\s+)?(?:all\s+)?(?:the\s+|its\s+)?(?:parts|pieces)(?:\s+apart)?|explosionsansicht|zerleg(?:e)?\s+(?:es|das\s+modell|ihn|sie)|(?:nimm|zieh)\s+(?:es|das\s+modell)\s+auseinander)$"));
    static ASSEMBLE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:put\s+(?:it|the\s+model)\s+(?:back\s+)?together(?:\s+again)?|reassemble(?:\s+it)?|assemble\s+it|collapse\s+(?:it|the\s+view)|unexplode(?:\s+it)?|(?:setz|setze|bau|baue)\s+(?:es|das\s+modell)\s+(?:wieder\s+)?zusammen)$"));
    if EXPLODE.is_match(t) {
        return Some(json!({ "action": "edit", "explode": true }));
    }
    if ASSEMBLE.is_match(t) {
        return Some(json!({ "action": "edit", "explode": false }));
    }
    // Size.
    static SIZE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:make|mach|mache)\s+(?:it|the\s+model|es|das\s+modell|ihn|sie)\s+(?:a\s+(?:bit|little)\s+)?(bigger|larger|huge|smaller|tiny|größer|grösser|groesser|kleiner)$|^(?:scale\s+it\s+(up|down)|(enlarge|shrink)\s+it)$"));
    if let Some(c) = SIZE.captures(t) {
        let w = c.get(1).or_else(|| c.get(2)).or_else(|| c.get(3)).map_or("", |m| m.as_str());
        let bigger = matches!(w, "bigger" | "larger" | "huge" | "größer" | "grösser" | "groesser" | "up" | "enlarge");
        return Some(json!({ "action": "edit", "scale": if bigger { "bigger" } else { "smaller" } }));
    }
    // Glow.
    static GLOW_OFF: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:stop\s+(?:the\s+)?glow(?:ing)?|no\s+(?:more\s+)?glow|(?:turn|switch)\s+off\s+the\s+glow|(?:remove|kill)\s+the\s+glow|(?:make\s+it\s+)?stop\s+glowing|kein\s+leuchten(?:\s+mehr)?|nicht\s+mehr\s+leuchten|hör\s+auf\s+zu\s+leuchten)$"));
    if GLOW_OFF.is_match(t) {
        return Some(json!({ "action": "edit", "glow": false }));
    }
    static GLOW: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:make|let|have|light)\s+(.+?)\s+(?:glow|shine|light\s+up)(?:\s+(.+))?$|^light\s+(.+?)\s+up(?:\s+(.+))?$|^(?:lass|lasse|mach|mache)\s+(.+?)\s+(?:leuchten|glühen|gluehen|leuchtend)(?:\s+(.+))?$"));
    if let Some(c) = GLOW.captures(t) {
        let target = c.get(1).or_else(|| c.get(3)).or_else(|| c.get(5)).map_or("", |m| m.as_str());
        let colour = c.get(2).or_else(|| c.get(4)).or_else(|| c.get(6)).map(|m| m.as_str().to_string());
        let mut args = json!({ "action": "edit", "glow": colour.as_deref().filter(|c| !scene3d::paints_in(c).is_empty()).unwrap_or("on") });
        if let Some(part) = part_word(target) {
            args["part"] = json!(part);
        }
        return Some(args);
    }
    // Colours and finishes: "make it gold", "paint the helmet blue and the arms silver",
    // "mach den Helm rot", "carbon fibre texture".
    paint_command(t, names)
}

/// The part a target phrase names (None: all of it).
fn part_word(target: &str) -> Option<String> {
    static ART: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:the|its|his|her|their|all\s+the|a|an|den|die|das|dem|der|seinen|seine|sein|ihre|ihren|alle)(?:\s+|$)"));
    let t = ART.replace(target.trim(), "").trim().to_string();
    const ALL: &[&str] = &["it", "this", "that", "him", "her", "them", "everything", "all", "all of it", "the whole thing", "whole thing", "model", "the model", "es", "ihn", "sie", "das", "alles", "modell", "das modell", "das ganze", "ganze", "the whole model", "whole model", "the entire thing", "entire thing", "it all"];
    (!t.is_empty() && !ALL.contains(&t.as_str())).then_some(t)
}

fn paint_command(t: &str, names: &[String]) -> Option<Value> {
    static VERB: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:make|turn|paint|colou?r|change|set|give|apply|switch|render|do|dye|spray|coat|wrap|skin|redo|mach|mache|färb|färbe|faerbe|lackier|lackiere|gib|setz|stell|änder|ändere|wechsel|wechsle|überzieh|beschicht)\b\s*")
    });
    let verb = VERB.find(t);
    let body = verb.map_or(t, |m| &t[m.end()..]);
    let n = mask(&scene3d::norm(body), names);
    let paints = scene3d::paints_at(&n);
    if paints.is_empty() {
        return None;
    }
    // Without a verb only a bare finish counts ("carbon fibre", "gold please", "in marble").
    if verb.is_none() {
        static BARE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:in|as|aus|als)\s+)?(?:an?\s+|eine?\s+)?(.+?)(?:\s+(?:texture|finish|look|material|paint|textur|oberfläche|lack|version))?$"));
        let inner = BARE.captures(n.trim()).map(|c| c[1].to_string()).unwrap_or_default();
        let rest = scene3d::norm(&inner);
        let covered: usize = scene3d::paints_at(&rest).iter().map(|(_, l, _)| l).sum();
        let words: usize = rest.trim().len();
        if covered + 6 < words || covered == 0 {
            return None;
        }
        return Some(json!({ "action": "edit", "material": inner.trim() }));
    }
    // Clauses: "the helmet blue and the arms silver".
    static SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"\s(?:and|und|,)\s+(?:the|den|die|das|its|seine?n?)\s+"));
    let pieces: Vec<String> = SPLIT.split(n.trim()).map(str::to_string).collect();
    let mut edits: Vec<Value> = Vec::new();
    for (k, piece) in pieces.iter().enumerate() {
        let pn = scene3d::norm(piece);
        let found = scene3d::paints_at(&pn);
        let Some((first_at, _, _)) = found.first() else {
            if k == 0 {
                return None;
            }
            continue;
        };
        // Nothing but finish words after the last paint ("a marble texture"): "set a red
        // alert" or "change the song to Red by …" are not paint jobs.
        const TAIL_OK: &[&str] = &[
            "texture", "textures", "finish", "look", "colour", "color", "paint", "paintjob", "job", "material", "version", "again", "instead", "too", "now", "all", "over", "everywhere", "one", "ones", "and", "with", "a", "accents", "highlights", "textur", "farbe", "lack", "oberfläche", "bitte", "jetzt", "auch", "wieder", "stattdessen", "und", "mit", "an", "trim",
        ];
        let (last_at, last_len, _) = found.last().cloned().unwrap_or((0, 0, Paint::Colour("", [0.0; 3])));
        if !pn[last_at + last_len..].split_whitespace().all(|w| TAIL_OK.contains(&w)) {
            return None;
        }
        // The words before the first paint name the target ("the helmet", "it", "den Helm").
        let target_words = pn[..*first_at].trim().replace('_', " ");
        // ("The Iron Man suit colour to green": the colour is not a part.)
        static PREP: LazyLock<Regex> = LazyLock::new(|| re(r"\s+(?:to|into|in|a|an|out\s+of|with|aus|in|zu|eine?|einen|mit|look|like|colou?rs?|paint|finish|texture|material|farbe|lack)$|^(?:colou?rs?|farbe)$"));
        let mut target = target_words;
        for _ in 0..3 {
            target = PREP.replace(&target, "").trim().to_string();
        }
        // The model by its own name is all of it ("the Iron Man suit").
        let part = part_word(&target).filter(|p| names.first().is_none_or(|title| scene3d::norm(title).trim() != scene3d::norm(p).trim()));
        // Paint words inside the target are part names ("the gold parts" stays a colour).
        let material: String = found.iter().map(|(_, _, p)| p.name()).collect::<Vec<_>>().join(" and ");
        let mut e = json!({ "material": material });
        if let Some(p) = part {
            if p.split_whitespace().count() > 4 {
                return None;
            }
            e["part"] = json!(p);
        }
        edits.push(e);
    }
    match edits.len() {
        0 => None,
        1 => {
            let mut a = edits.remove(0);
            a["action"] = json!("edit");
            Some(a)
        }
        _ => Some(json!({ "action": "edit", "edits": edits })),
    }
}

/// "20 cm tall", "18 centimetres high", "15 cm hoch".
fn height_cm(text: &str) -> Option<f32> {
    static H: LazyLock<Regex> = LazyLock::new(|| re(r"(\d+(?:[.,]\d+)?)\s*(mm|cm|m|millimet(?:er|re)s?|centimet(?:er|re)s?|zentimeter|meters?|metres?|meter)\b"));
    let c = H.captures(text)?;
    let v: f32 = c[1].replace(',', ".").parse().ok()?;
    let cm = match &c[2] {
        u if u.starts_with("mm") || u.starts_with("milli") => v / 10.0,
        u if u == "m" || u.starts_with("met") => v * 100.0,
        _ => v,
    };
    (cm > 0.05 && cm < 100_000.0).then_some(cm)
}

/// "at 1:10", "im Maßstab 1:20", "1 to 50".
fn scale_ratio(text: &str) -> Option<f32> {
    static S: LazyLock<Regex> = LazyLock::new(|| re(r"\b1\s*(?::|to|zu)\s*(\d+(?:[.,]\d+)?)\b"));
    let c = S.captures(text)?;
    let d: f32 = c[1].replace(',', ".").parse().ok()?;
    (d >= 1.0).then(|| 1.0 / d)
}

// ---------------------------------------------------------------------------- the tool ----

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

/// Kept terse: every character here is in every prompt (the live tests and the phrase paths
/// cover the details the model does not need spelled out).
pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "3D studio: any object as a 3D model. edit: material (colour/finish), part, glow, look realistic|hologram, wireframe, spin faster|slower|stop|start, view front|back|left|right|top|bottom|zoom_in|zoom_out, explode, scale bigger|smaller. enhance: more detail. export: format stl|obj|glb, height_cm, folder (path, desktop, ask).",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "description": "what to show"},
            "path": {"type": "string", "description": "model file or 'downloads'"},
            "action": {"type": "string", "enum": ["show", "edit", "enhance", "export", "measure", "parts", "reset", "close"]},
            "material": {"type": "string"},
            "part": {"type": "string"},
            "glow": {"type": "string"},
            "look": {"type": "string"},
            "wireframe": {"type": "boolean"},
            "spin": {"type": "string"},
            "view": {"type": "string"},
            "explode": {"type": "boolean"},
            "scale": {"type": "string"},
            "format": {"type": "string"},
            "height_cm": {"type": "number"},
            "folder": {"type": "string"}
        }}
    }})]
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// A number, also when the model sent it as text ("0.1", "18 cm").
fn num_arg(args: &Value, key: &str) -> Option<f64> {
    let v = args.get(key)?;
    v.as_f64().or_else(|| {
        let s = v.as_str()?.trim();
        let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(s.len());
        s[..end].replace(',', ".").parse().ok()
    })
}

fn reply(display: impl Into<String>, spoken: impl Into<String>) -> String {
    format!("{}\nSpoken: {}", display.into(), spoken.into())
}

/// `show_3d_model`.
pub async fn run_tool(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let action = str_arg(args, "action").unwrap_or("").to_ascii_lowercase();
    match action.as_str() {
        "close" | "hide" => {
            let was = is_open();
            close();
            return Ok(reply("3D studio closed.", if was { "Closed, Sir." } else { "The 3D studio isn't open, Sir." }));
        }
        "export" | "save" => return export(args).await,
        "measure" | "size" => return Ok(measure()),
        "parts" | "list_parts" => return Ok(parts()),
        "reset" => return Ok(reset()),
        "enhance" | "detail" | "more_detail" | "refine" | "improve" => return enhance(ctx).await,
        _ => {}
    }
    if let Some(path) = str_arg(args, "path") {
        studio().query.clear();
        return open_local(path, args).await;
    }
    if let Some(query) = str_arg(args, "query") {
        // The model on show named again with a change ("iron man suit green colour", "… hyper
        // detailed" from the model, 2026-11-09): that change; a new model replayed the block's
        // entrance every time.
        if let Some(change) = is_open().then(|| studio().query.to_lowercase()).and_then(|shown| change_to_shown(query, &shown)) {
            return if DETAIL_WORDS.is_match(&change) {
                enhance(ctx).await
            } else {
                edit_command(&clean(&format!("make it {change}")), false, &[]).map_or_else(|| edit(args), |a| edit(&a))
            };
        }
        // "I meant the fruit": the sense of what was asked for before.
        let asked = {
            let mut s = studio();
            let asked = if is_open() && !s.query.is_empty() && sense_only(query) { format!("{} {query}", wanted(&s.query, &[]).core) } else { query.to_string() };
            s.query = asked.clone();
            s.level = num_arg(args, "level").map_or(1, |l| (l as u32).clamp(1, scene3d::MAX_LEVEL));
            asked
        };
        return open_query(ctx, &asked, args).await;
    }
    // "show" with nothing named: the studio itself ("studio" from older callers).
    if !has_edits(args) && (matches!(action.as_str(), "show" | "open" | "studio") || !is_open()) {
        return Ok(open_studio());
    }
    edit(args)
}

static DETAIL_WORDS: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:detail|detailed|details|accurate|realistic|resolution|hd|high\s+poly|enhanced|refined|better)\b"));

/// What `query` adds to the model on show (`shown`, lower case): "iron man suit green colour"
/// → "green colour"; None for another model.
fn change_to_shown(query: &str, shown: &str) -> Option<String> {
    let q = query.to_lowercase();
    let rest = q.strip_prefix(shown).filter(|_| !shown.is_empty())?.trim();
    let rest = rest.trim_start_matches(|c: char| !c.is_alphanumeric()).trim_start_matches("in ").trim_start_matches("with ").trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

fn has_edits(args: &Value) -> bool {
    ["material", "color", "colour", "glow", "look", "wireframe", "spin", "view", "explode", "scale", "edits", "part"].iter().any(|k| args.get(k).is_some_and(|v| !v.is_null()))
}

/// "Open the 3D studio": the model it has, else the arc reactor as a showpiece.
fn open_studio() -> String {
    if is_open() && studio().base.is_some() {
        crate::gui::show();
        return reply("3D studio: on screen.", "It's up, Sir.");
    }
    let job = begin("Arc reactor", "Built-in", "", None);
    let model = scene3d::build(&arc_reactor_doc());
    deliver(job, model, Delivery { source: "Built-in".into(), title: Some("Arc reactor".into()), ..Delivery::default() });
    reply("3D studio open with the arc reactor. Ask for anything: \"render me an Iron Man suit in 3D\".", "The studio is up, Sir. What shall I build?")
}

fn arc_reactor_doc() -> scene3d::SceneDoc {
    scene3d::lookup("arc reactor").and_then(|e| e.kind.scene(1)).unwrap_or_default()
}

/// Applies the edit arguments; the reply says what changed.
fn edit(args: &Value) -> Result<String, String> {
    let (has_model, working) = {
        let s = studio();
        (s.base.is_some(), s.phase == Phase::Working)
    };
    if !is_open() || (!has_model && !working) {
        return Ok(reply("Nothing is in the 3D studio.", "There's nothing in the studio yet, Sir. Tell me what to render."));
    }
    let mut said: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    // Materials: one edit, or a list ("the helmet blue and the arms silver").
    let mut items: Vec<Value> = args.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
    if args.get("material").or_else(|| args.get("color")).or_else(|| args.get("colour")).is_some() {
        items.push(json!({ "material": str_arg(args, "material").or_else(|| str_arg(args, "color")).or_else(|| str_arg(args, "colour")).unwrap_or(""), "part": args.get("part").cloned().unwrap_or(Value::Null) }));
    }
    let mut new_edits: Vec<Edit> = Vec::new();
    for item in &items {
        let material = str_arg(item, "material").unwrap_or("");
        let paints = scene3d::paints_in(material);
        if paints.is_empty() {
            problems.push(format!("I don't know the finish \"{material}\""));
            continue;
        }
        let part = str_arg(item, "part").map(str::to_string);
        let target = match &part {
            Some(p) => {
                let known = studio().base.as_ref().is_none_or(|m| !named_parts(m, p).is_empty());
                if known {
                    Target::Named(p.clone())
                } else {
                    // No part of that name: where such a part is, on a model that came in one piece.
                    match carve_zone(p) {
                        Ok(name) => Target::Named(name),
                        Err(why) => {
                            problems.push(why);
                            continue;
                        }
                    }
                }
            }
            None => Target::All,
        };
        if paints.len() >= 2 && target == Target::All {
            new_edits.push(Edit { target: Target::Primary, change: Change::Paint(paints[0].clone()) });
            new_edits.push(Edit { target: Target::Secondary, change: Change::Paint(paints[1].clone()) });
            said.push(format!("{} and {}", paints[0].name(), paints[1].name()));
        } else {
            for p in &paints {
                new_edits.push(Edit { target: target.clone(), change: Change::Paint(p.clone()) });
            }
            let names = paints.iter().map(Paint::name).collect::<Vec<_>>().join(" and ");
            said.push(match &part {
                Some(p) => format!("the {p} in {names}"),
                None => names,
            });
        }
    }
    if let Some(g) = args.get("glow").filter(|v| !v.is_null()) {
        let target = str_arg(args, "part").map_or(Target::All, |p| Target::Named(p.to_string()));
        let off = g.as_bool() == Some(false) || g.as_str().is_some_and(|s| ["off", "false", "no", "none", "aus"].contains(&s.to_ascii_lowercase().as_str()));
        if off {
            new_edits.push(Edit { target, change: Change::NoGlow });
            said.push("no glow".into());
        } else {
            let colour = g.as_str().and_then(|s| scene3d::paints_in(s).into_iter().find_map(|p| match p {
                Paint::Colour(_, c) => Some(c),
                Paint::Finish(f) => f.color,
            }));
            new_edits.push(Edit { target, change: Change::Glow(colour) });
            said.push("glowing".into());
        }
    }
    let mut s = studio();
    if !new_edits.is_empty() {
        s.edits.extend(new_edits);
        let excess = s.edits.len().saturating_sub(32);
        s.edits.drain(..excess);
        rebuild(&mut s);
    }
    if let Some(look) = str_arg(args, "look") {
        s.view.look = if look.to_ascii_lowercase().starts_with("holo") { Look::Hologram } else { Look::Realistic };
        said.push(if s.view.look == Look::Hologram { "hologram".into() } else { "realistic".into() });
    }
    if let Some(w) = args.get("wireframe").and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| !["off", "false", "no"].contains(&s)))) {
        s.view.wireframe = w;
        said.push(if w { "wireframe".into() } else { "solid".into() });
    }
    if let Some(spin) = str_arg(args, "spin").map(str::to_ascii_lowercase) {
        s.view.spin = match spin.as_str() {
            "faster" => (s.view.spin.max(0.2) * 2.0).min(4.0),
            "slower" => s.view.spin * 0.5,
            "stop" => 0.0,
            other => other.parse::<f32>().unwrap_or(DEFAULT_SPIN).clamp(0.0, 6.0),
        };
        said.push(if s.view.spin == 0.0 { "holding still".into() } else { "spinning".into() });
    }
    if let Some(v) = str_arg(args, "view").map(|v| v.to_ascii_lowercase().replace(' ', "_")) {
        let cmd = match v.as_str() {
            "front" => CameraCmd::Front,
            "back" => CameraCmd::Back,
            "left" => CameraCmd::Left,
            "right" | "side" => CameraCmd::Right,
            "top" => CameraCmd::Top,
            "bottom" => CameraCmd::Bottom,
            "zoom_in" | "in" | "closer" => CameraCmd::Zoom(0.7),
            "zoom_out" | "out" => CameraCmd::Zoom(1.4),
            _ => CameraCmd::Reset,
        };
        s.camera.push_back(cmd);
        if matches!(cmd, CameraCmd::Front | CameraCmd::Back | CameraCmd::Left | CameraCmd::Right | CameraCmd::Top | CameraCmd::Bottom) {
            s.view.spin = 0.0;
        }
        said.push(format!("{} view", v.replace('_', " ")));
    }
    if let Some(e) = args.get("explode").and_then(|v| v.as_bool().or_else(|| v.as_f64().map(|f| f > 0.0))) {
        s.view.explode = if e { 1.0 } else { 0.0 };
        said.push(if e { "pulled apart".into() } else { "back together".into() });
    }
    if let Some(k) = args.get("scale").filter(|v| !v.is_null()) {
        let factor = match k.as_str() {
            Some("bigger" | "larger" | "up") => 1.3,
            Some("smaller" | "down") => 1.0 / 1.3,
            _ => num_arg(args, "scale").filter(|f| *f > 0.0).map_or(1.0, |f| f as f32),
        };
        s.view.scale = (s.view.scale * factor).clamp(0.3, 3.0);
        said.push(if factor >= 1.0 { "bigger".into() } else { "smaller".into() });
    }
    touched(&mut s);
    let title = s.title.clone();
    drop(s);
    crate::gui::show();
    if said.is_empty() {
        let why = problems.first().cloned().unwrap_or_else(|| "nothing to change".into());
        return Ok(reply(format!("3D studio: {why}."), format!("{}, Sir.", capitalise(&why))));
    }
    let line = said.join(", ");
    let mut spoken = match said.len() {
        1 if line.starts_with("the ") => format!("Done, Sir: {line}."),
        _ => format!("{}, Sir.", capitalise(&line)),
    };
    if let Some(p) = problems.first() {
        spoken.push_str(&format!(" {p}."));
    }
    Ok(reply(format!("3D studio ({title}): {line}."), spoken))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn reset() -> String {
    let mut s = studio();
    if s.base.is_none() {
        return reply("Nothing is in the 3D studio.", "There's nothing to reset, Sir.");
    }
    s.edits.clear();
    let look = s.view.look;
    s.view = ViewState { look, spin: if s.planet.is_some() { 0.15 } else { DEFAULT_SPIN }, ..ViewState::default() };
    s.camera.push_back(CameraCmd::Reset);
    rebuild(&mut s);
    touched(&mut s);
    reply(format!("3D studio: {} back to its original look.", s.title), "Back to the original, Sir.")
}

/// "about 18 cm", "1.9 m", "12,742 km".
pub fn length(m: f32) -> String {
    let m = m.abs();
    if m >= 10_000.0 {
        let km = (m / 1000.0).round() as u64;
        let s = km.to_string();
        let mut out = String::new();
        for (i, ch) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }
        format!("{out} km")
    } else if m >= 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else if m >= 20.0 {
        format!("{:.0} m", m)
    } else if m >= 1.0 {
        format!("{:.1} m", m)
    } else if m >= 0.1 {
        format!("{:.0} cm", m * 100.0)
    } else if m >= 0.01 {
        format!("{:.1} cm", m * 100.0)
    } else {
        format!("{:.1} mm", m * 1000.0)
    }
}

fn measure() -> String {
    let s = studio();
    let Some(m) = s.base.clone() else {
        return reply("Nothing is in the 3D studio.", "There's nothing to measure yet, Sir.");
    };
    let title = s.title.clone();
    let planet = s.planet.is_some();
    drop(s);
    let size = m.size_m();
    if planet {
        // Pole to pole (Saturn's rings are not the planet).
        let d = length(size[1]);
        return reply(format!("{title}: {d} across ({} triangles)", m.triangles()), format!("{title} is about {d} across, Sir."));
    }
    let (w, h, d) = (length(size[0]), length(size[1]), length(size[2]));
    reply(
        format!("{title}: {h} tall, {w} wide, {d} deep at 1:1 ({} triangles, {} parts).", m.triangles(), m.parts.len()),
        format!("At full size it's about {h} tall, {w} wide and {d} deep, Sir."),
    )
}

fn parts() -> String {
    let s = studio();
    let Some(m) = s.base.clone() else {
        return reply("Nothing is in the 3D studio.", "There's nothing in the studio yet, Sir.");
    };
    drop(s);
    let mut names: Vec<String> = Vec::new();
    for p in &m.parts {
        let n = p.name.trim_start_matches("left ").trim_start_matches("right ").to_string();
        if !names.contains(&n) {
            names.push(n);
        }
    }
    // The block shows every name he can use; the spoken line stops after a dozen.
    let list = |cap: usize| {
        let shown: Vec<&str> = names.iter().map(String::as_str).take(cap).collect();
        let more = names.len().saturating_sub(shown.len());
        if more > 0 { format!("{} and {more} more", shown.join(", ")) } else { shown.join(", ") }
    };
    let count = if names.len() == 1 { "one part".to_string() } else { format!("{} parts", names.len()) };
    reply(format!("Parts ({} meshes, {} vertices): {}", m.parts.len(), m.vertices(), list(48)), format!("It has {count}: {}, Sir.", list(12)))
}

// -------------------------------------------------------------------------------- export ----

fn export_dir() -> PathBuf {
    if demo_mode() {
        std::env::temp_dir().join("jai_3d_export_demo")
    } else {
        crate::features::user_folder("Documents").join("JAI Models")
    }
}

/// The model as Sir sees it (his materials on it).
fn edited_model() -> Option<(Model, String, bool)> {
    let s = studio();
    let scene = s.scene.clone()?;
    let title = s.title.clone();
    let planet = s.planet.is_some();
    drop(s);
    let mut m = (*scene.model).clone();
    m.materials = scene.materials.clone();
    m.textures = scene.textures.clone();
    for (p, mat) in m.parts.iter_mut().zip(&scene.part_material) {
        p.material = *mat;
    }
    m.name = title.clone();
    Some((m, title, planet))
}

async fn export(args: &Value) -> Result<String, String> {
    let Some((model, title, planet)) = edited_model() else {
        return Ok(reply("Nothing is in the 3D studio.", "There's nothing to save yet, Sir. Tell me what to render first."));
    };
    let format = str_arg(args, "format").unwrap_or("stl").to_ascii_lowercase();
    let size = model.size_m();
    let height = size[1].max(1e-6);
    let printer = format == "stl";
    // The scale: asked for, a height asked for, or for printing a sensible desk size.
    let asked = num_arg(args, "scale").map(|k| k as f32).filter(|k| *k > 0.0 && *k <= 10.0);
    let scale = if let Some(cm) = num_arg(args, "height_cm").filter(|cm| *cm > 0.0) {
        (cm as f32 / 100.0) / height
    } else if let Some(k) = asked {
        k
    } else if printer && (planet || height > 0.3 || height < 0.02) {
        0.18 / height
    } else {
        1.0
    };
    let stamp = chrono::Local::now().format("%Y-%m-%d %H%M").to_string();
    let name = format!("{} {stamp}.{format}", crate::features::safe_name(&title));
    let file = match str_arg(args, "folder").filter(|_| !demo_mode()) {
        Some(f) if f.eq_ignore_ascii_case("ask") => {
            let (name, ext) = (name.clone(), format.clone());
            match tokio::task::spawn_blocking(move || save_dialog(&name, &ext)).await.map_err(|e| e.to_string())?? {
                Some(path) => path,
                None => return Ok(reply("Not saved (the dialog was cancelled).", "Very well, Sir, not saved.")),
            }
        }
        Some(f) => folder_path(f).join(&name),
        None => export_dir().join(&name),
    };
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_else(export_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("I couldn't create {}: {e}", dir.display()))?;
    let path = file.clone();
    let fmt = format.clone();
    let out = tokio::task::spawn_blocking(move || match fmt.as_str() {
        "obj" => mesh_io::export_obj(&model, &path, scale),
        "glb" | "gltf" => mesh_io::export_glb(&model, &path, scale),
        _ => mesh_io::export_stl(&model, &path, scale),
    })
    .await
    .map_err(|e| e.to_string())??;
    let tall = length(out.size_m[1]);
    let ratio = if (scale - 1.0).abs() < 1e-3 {
        "at 1:1".to_string()
    } else if scale < 1.0 {
        format!("at 1:{}", fmt_ratio(1.0 / scale))
    } else {
        format!("at {}:1", fmt_ratio(scale))
    };
    let real = if (scale - 1.0).abs() > 1e-3 { format!(" (the real thing is about {})", length(height)) } else { String::new() };
    let name = out.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    info!("[3D] Exported {} triangles to {}", out.triangles, out.path.display());
    let folder = out.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let (where_, said) = if demo_mode() {
        ("the temp folder (test run)".to_string(), "the temp folder".to_string())
    } else if dir == export_dir() {
        ("Documents\\JAI Models".to_string(), "JAI Models".to_string())
    } else {
        let last = out.path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| folder.clone());
        (folder.clone(), last)
    };
    Ok(reply(
        format!("Saved {name} in {where_}: about {tall} tall {ratio}{real}, {} triangles.", out.triangles),
        format!("Saved as {} in {said}, Sir: about {tall} tall {ratio}.", format.to_uppercase()),
    ))
}

/// A folder Sir named: a path ("D:\Prints", "~\3D"), one of his own ("desktop", "downloads"),
/// else a folder of that name in Documents.
fn folder_path(name: &str) -> PathBuf {
    let n = name.trim().trim_matches('"');
    let known = [("desktop", "Desktop"), ("schreibtisch", "Desktop"), ("documents", "Documents"), ("dokumente", "Documents"), ("downloads", "Downloads"), ("pictures", "Pictures"), ("bilder", "Pictures"), ("3d objects", "3D Objects"), ("music", "Music"), ("videos", "Videos")];
    if let Some((_, folder)) = known.iter().find(|(k, _)| n.eq_ignore_ascii_case(k)) {
        return crate::features::user_folder(folder);
    }
    if let Some(rest) = n.strip_prefix("~\\").or_else(|| n.strip_prefix("~/")) {
        return crate::apps::home_dir().unwrap_or_else(std::env::temp_dir).join(rest);
    }
    let p = PathBuf::from(n);
    if p.is_absolute() {
        p
    } else {
        crate::features::user_folder("Documents").join(crate::features::safe_name(n))
    }
}

/// Where Sir wants a model saved, from what he said: a path, one of his folders, or "ask" (the
/// save dialog).
fn folder_in(text: &str) -> Option<String> {
    static PATH: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)((?:[a-z]:[\\/]|\\\\|~[\\/])[^"<>|*?\r\n]*)"#));
    if let Some(c) = PATH.captures(text) {
        static TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:[\s.,!]+(?:please|bitte|for\s+me|jai|sir|as\s+(?:an?\s+)?(?:stl|obj|glb)(?:\s+file)?))*[\s.,!]*$"));
        let p = TAIL.replace(c[1].trim(), "").to_string();
        return (p.len() > 3).then_some(p);
    }
    let t = text.to_lowercase();
    const ASK: &[&str] = &["choose where", "choose the folder", "choose a folder", "pick where", "pick the folder", "pick a folder", "somewhere else", "where i want", "let me choose", "let me pick", "save dialog", "save as dialog", "wo ich will", "ordner auswählen", "speichern unter", "selbst aussuchen"];
    if ASK.iter().any(|w| t.contains(w)) {
        return Some("ask".into());
    }
    const KNOWN: &[(&str, &str)] = &[("desktop", "desktop"), ("schreibtisch", "desktop"), ("documents", "documents"), ("dokumente", "documents"), ("downloads", "downloads"), ("download folder", "downloads"), ("pictures", "pictures"), ("bilder", "pictures"), ("3d objects", "3d objects")];
    KNOWN.iter().find(|(w, _)| t.split(|c: char| !c.is_alphanumeric() && c != ' ').any(|part| part.contains(w))).map(|(_, f)| f.to_string())
}

/// The desktop's "Save as" dialog (zenity on GNOME, kdialog on KDE): the file Sir chose, or None
/// when he cancelled. (A test session opens no dialog.)
#[cfg(not(windows))]
fn save_dialog(name: &str, ext: &str) -> Result<Option<PathBuf>, String> {
    if crate::platform::in_test_session() {
        return Err("a test session opens no dialogs".into());
    }
    let start = export_dir().join(name).display().to_string();
    let zenity = ["--file-selection".to_string(), "--save".into(), format!("--filename={start}"), "--title=Save the 3D model — J.A.I.".into()];
    let kdialog = ["--getsavefilename".to_string(), start.clone(), format!("*.{ext}")];
    for (program, args) in [("zenity", &zenity[..]), ("kdialog", &kdialog[..])] {
        match std::process::Command::new(program).args(args).output() {
            Ok(out) if out.status.success() => {
                let chosen = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return Ok((!chosen.is_empty()).then(|| PathBuf::from(chosen)));
            }
            Ok(_) => return Ok(None),
            Err(_) => continue,
        }
    }
    Err("there's no save dialog on this PC (zenity or kdialog); name a folder instead".into())
}

/// The Windows "Save as" dialog: the file Sir chose, or None when he cancelled. (A test session
/// opens no dialog.)
#[cfg(windows)]
fn save_dialog(name: &str, ext: &str) -> Result<Option<PathBuf>, String> {
    if crate::platform::in_test_session() {
        return Err("a test session opens no dialogs".into());
    }
    use windows::core::HSTRING;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, SIGDN_FILESYSPATH};
    // SAFETY: COM on this (blocking) thread only, released before it returns; the dialog's
    // strings outlive the calls that use them and the path it returns is freed after copying.
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let chosen = (|| -> windows::core::Result<Option<PathBuf>> {
            let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetTitle(&HSTRING::from("Save the 3D model — J.A.I."))?;
            dialog.SetFileName(&HSTRING::from(name))?;
            dialog.SetDefaultExtension(&HSTRING::from(ext))?;
            if dialog.Show(HWND(0)).is_err() {
                return Ok(None);
            }
            let raw = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = raw.to_string();
            CoTaskMemFree(Some(raw.0 as *const std::ffi::c_void));
            Ok(path.ok().map(PathBuf::from))
        })();
        if com {
            CoUninitialize();
        }
        chosen.map_err(|e| format!("the save dialog failed: {e}"))
    }
}

/// "Make it more detailed": a built-in made in levels comes at its next level (Sir's paint and
/// look stay on it); any other scene goes back to the language model to be refined; a file or a
/// Sketchfab model is already all there is.
async fn enhance(ctx: &ToolContext) -> Result<String, String> {
    let (query, level, doc, edits, look, ready) = {
        let s = studio();
        (s.query.clone(), s.level, s.doc.clone(), s.edits.clone(), s.view.look, s.base.is_some() && is_open())
    };
    if !ready {
        return Ok(reply("Nothing is in the 3D studio.", "There's nothing to refine yet, Sir. Tell me what to render first."));
    }
    if let Some(entry) = scene3d::lookup(&query).filter(|e| matches!(e.kind, scene3d::Kind::Detailed(_))) {
        if level >= scene3d::MAX_LEVEL {
            return Ok(reply(format!("The {} is at its finest level ({level} of {}).", entry.key, scene3d::MAX_LEVEL), "That's the finest I have of it, Sir."));
        }
        let next = level + 1;
        open_builtin(entry, &json!({ "level": next })).await?;
        let triangles = {
            let mut s = studio();
            s.level = next;
            s.query = query;
            s.edits = edits;
            s.view.look = look;
            rebuild(&mut s);
            touched(&mut s);
            s.base.as_ref().map_or(0, |m| m.triangles())
        };
        return Ok(reply(format!("3D studio: the {} at detail level {next} of {} ({} triangles).", entry.key, scene3d::MAX_LEVEL, triangles), "More detail, Sir."));
    }
    let Some(doc) = doc else {
        return Ok(reply("This model came from a file or Sketchfab and is shown in full.", "That one is already shown in full detail, Sir. I can compose my own, more detailed version if you name it again."));
    };
    let base = scene3d::doc_json(&doc);
    let (llm, cfg, job) = (ctx.pim.llm().cloned(), ctx.config.snapshot(), studio().job);
    let q = if query.is_empty() { doc.name.clone() } else { query };
    tokio::spawn(async move {
        match compose(job, &q, llm, &cfg, Some(base)).await {
            Ok(line) => crate::announcements::say(line),
            Err(why) => {
                fail(job, &format!("Not refined: {why}"));
                crate::announcements::say(format!("I couldn't refine it, Sir: {why}."));
            }
        }
    });
    Ok(reply("Refining the model: more parts, truer proportions. It stays on screen meanwhile.", "Refining it now, Sir."))
}

/// Saves the model from the panel's chips; the studio's note says where it went.
pub fn export_in_background(rt: &tokio::runtime::Handle, format: &str, folder: Option<&str>) {
    let args = json!({ "format": format, "folder": folder });
    rt.spawn(async move {
        let line = match export(&args).await {
            Ok(r) => r.lines().next().unwrap_or("").to_string(),
            Err(e) => format!("Not saved: {e}"),
        };
        let mut s = studio();
        s.note = Some(line);
        touched(&mut s);
    });
}

/// The Integrations hub's "Test": whom Sketchfab says the token belongs to (the token itself is
/// never shown or logged).
pub async fn check_sketchfab_token(token: &str) -> (bool, String) {
    let resp = http().get("https://api.sketchfab.com/v3/me").header("Authorization", format!("Token {}", token.trim())).timeout(Duration::from_secs(10)).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or_default();
            let who = v["displayName"].as_str().or_else(|| v["username"].as_str()).unwrap_or("your account").to_string();
            (true, format!("OK: Sketchfab knows the token as {who}."))
        }
        Ok(r) if r.status().as_u16() == 401 => (false, "Sketchfab didn't accept that token.".into()),
        Ok(r) => (false, format!("Sketchfab answered {}.", r.status())),
        Err(e) => (false, format!("Sketchfab didn't answer: {}", e.without_url())),
    }
}

fn fmt_ratio(k: f32) -> String {
    if k >= 10.0 {
        format!("{:.0}", k)
    } else {
        format!("{:.1}", k).trim_end_matches(".0").to_string()
    }
}

// ------------------------------------------------------------------------------- sources ----

fn http() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder().user_agent("JAI/3 (3D studio)").connect_timeout(Duration::from_secs(8)).timeout(Duration::from_secs(240)).build().unwrap_or_default()
    });
    &CLIENT
}

fn cache_dir() -> PathBuf {
    if cfg!(test) {
        std::env::temp_dir().join("jai_model_cache_test")
    } else {
        PathBuf::from(CACHE_DIR)
    }
}

/// A cached file (its time refreshed: recently used files stay longest).
fn cached(name: &str) -> Option<PathBuf> {
    let p = cache_dir().join(name);
    if p.exists() {
        if let Ok(f) = std::fs::File::options().append(true).open(&p) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(p)
    } else {
        None
    }
}

fn store(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("I couldn't create the model cache: {e}"))?;
    let p = dir.join(name);
    std::fs::write(&p, bytes).map_err(|e| format!("I couldn't cache the model: {e}"))?;
    evict(&dir, CACHE_LIMIT);
    Ok(p)
}

/// Deletes the oldest entries of `dir` until it holds at most `limit` bytes.
fn evict(dir: &Path, limit: u64) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = read
        .flatten()
        .map(|e| {
            let p = e.path();
            let size = if p.is_dir() { walkdir::WalkDir::new(&p).into_iter().flatten().filter_map(|f| f.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum() } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
            let when = e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            (when, size, p)
        })
        .collect();
    let mut total: u64 = entries.iter().map(|e| e.1).sum();
    if total <= limit {
        return;
    }
    entries.sort_by_key(|e| e.0);
    for (_, size, p) in entries {
        if total <= limit {
            break;
        }
        let gone = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        if gone.is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Downloads `url` (at most `max` bytes) with the studio's progress bar from `from` to `to`.
async fn download(url: &str, auth: Option<String>, job: u64, max: u64, (from, to): (f32, f32)) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    let mut req = http().get(url);
    if let Some(a) = auth {
        req = req.header("Authorization", a);
    }
    let resp = req.send().await.map_err(|e| format!("the download failed: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!("the server answered {}", resp.status()));
    }
    let total = resp.content_length();
    if total.is_some_and(|t| t > max) {
        return Err(format!("the file is {} MB, more than I fetch", total.unwrap_or(0) >> 20));
    }
    let mut buf: Vec<u8> = Vec::with_capacity(total.unwrap_or(1 << 20).min(max) as usize);
    let mut stream = resp.bytes_stream();
    let mut last = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("the download broke off: {}", e.without_url()))?;
        buf.extend_from_slice(&chunk);
        if buf.len() as u64 > max {
            return Err("the file is larger than I fetch".into());
        }
        if last.elapsed() > Duration::from_millis(120) {
            last = Instant::now();
            if !current(job) {
                return Err("cancelled".into());
            }
            if let Some(t) = total {
                progress(job, Some(from + (to - from) * buf.len() as f32 / t.max(1) as f32), None);
            }
        }
    }
    Ok(buf)
}

/// Loads a model file on a blocking thread with the progress bar from `from` to 1.
async fn load_path(path: PathBuf, job: u64, from: f32) -> Result<Model, String> {
    tokio::task::spawn_blocking(move || {
        let report = move |p: f32| progress(job, Some(from + (1.0 - from) * p), None);
        mesh_io::load_file(&path, &report)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One entry of the curated keyless index.
struct Curated {
    words: &'static [&'static str],
    title: &'static str,
    /// "khronos:<Name>" or "nasa:<path in the repository>".
    at: &'static str,
    author: &'static str,
    license: &'static str,
}

const SEE_SOURCE: &str = "see its README in glTF-Sample-Assets";
const NASA_LICENSE: &str = "public domain (NASA)";

/// Keyless models checked to serve real binaries (NASA 3D Resources and the Khronos samples).
static CURATED: &[Curated] = &[
    Curated { words: &["international space station", "space station", "iss", "raumstation", "internationale raumstation"], title: "International Space Station", at: "nasa:3D Models/International Space Station (ISS) (B)/International Space Station (ISS) (B).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["hubble space telescope", "hubble telescope", "hubble", "hubble teleskop", "weltraumteleskop hubble"], title: "Hubble Space Telescope", at: "nasa:3D Models/Hubble Space Telescope (B)/Hubble Space Telescope (B).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["perseverance rover", "perseverance", "mars rover", "mars 2020", "marsrover"], title: "Mars 2020 Perseverance Rover", at: "nasa:3D Models/Mars 2020 Perseverance Rover/Mars 2020 Perseverance Rover.glb", author: "NASA/JPL-Caltech", license: NASA_LICENSE },
    Curated { words: &["saturn v", "saturn 5", "saturn five", "saturn v rocket", "apollo rocket", "moon rocket", "mondrakete"], title: "Saturn V", at: "nasa:3D Models/Saturn V/Saturn V.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["space shuttle", "shuttle", "spaceshuttle", "raumfähre", "raumfaehre"], title: "Space Shuttle", at: "nasa:3D Models/Space Shuttle (A)/Space Shuttle (A).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["apollo lunar module", "lunar module", "lunar lander", "apollo lander", "mondlandefähre", "mondfähre", "landefähre"], title: "Apollo Lunar Module", at: "nasa:3D Models/Apollo Lunar Module/Apollo Lunar Module.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["voyager probe", "voyager", "voyager sonde"], title: "Voyager", at: "nasa:3D Models/Voyager Probe (B)/Voyager Probe (B).glb", author: "NASA/JPL", license: NASA_LICENSE },
    Curated { words: &["james webb space telescope", "james webb telescope", "james webb", "webb telescope", "jwst", "james webb teleskop"], title: "James Webb Space Telescope", at: "nasa:3D Models/James Webb Space Telescope (A)/James Webb Space Telescope (A).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["cassini huygens", "cassini", "cassini probe"], title: "Cassini", at: "nasa:3D Models/Cassini Assembly/Cassini Assembly.glb", author: "NASA/JPL", license: NASA_LICENSE },
    Curated { words: &["ingenuity", "mars helicopter", "ingenuity helicopter", "marshubschrauber"], title: "Ingenuity Mars Helicopter", at: "nasa:3D Models/Ingenuity Mars Helicopter/Ingenuity Mars Helicopter.glb", author: "NASA/JPL-Caltech", license: NASA_LICENSE },
    Curated { words: &["astronaut", "spaceman", "raumfahrer"], title: "Astronaut", at: "nasa:3D Models/Astronaut/Astronaut.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["spacesuit", "space suit", "eva suit", "raumanzug", "extravehicular mobility unit"], title: "Extravehicular Mobility Unit", at: "nasa:3D Models/Extravehicular Mobility Unit/Extravehicular Mobility Unit.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["viking lander", "viking"], title: "Viking Lander", at: "nasa:3D Models/Viking Lander/Viking Lander.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["juno probe", "juno spacecraft", "juno"], title: "Juno", at: "nasa:3D Models/Juno (A)/Juno (A).glb", author: "NASA/JPL", license: NASA_LICENSE },
    Curated { words: &["parker solar probe", "parker probe", "solar probe"], title: "Parker Solar Probe", at: "nasa:3D Models/Parker Solar Probe/Parker Solar Probe.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["robonaut", "robonaut 2"], title: "Robonaut 2", at: "nasa:3D Models/Robonaut 2/Robonaut 2.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["chandra", "chandra x ray observatory", "x ray observatory"], title: "Chandra X-ray Observatory", at: "nasa:3D Models/Chandra X-ray Observatory/Chandra X-ray Observatory.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["rosetta", "rosetta probe"], title: "Rosetta", at: "nasa:3D Models/Rosetta/Rosetta.glb", author: "NASA/ESA", license: NASA_LICENSE },
    Curated { words: &["gemini capsule", "gemini spacecraft", "gemini kapsel"], title: "Gemini", at: "nasa:3D Models/Gemini/Gemini.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["x 57", "x57", "x 57 maxwell"], title: "X-57 Maxwell", at: "nasa:3D Models/X-57/X-57.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["insight lander", "insight"], title: "InSight Lander", at: "nasa:3D Models/InSight Cruise Lander/InSight Cruise Lander (panels deployed).glb", author: "NASA/JPL", license: NASA_LICENSE },
    Curated { words: &["global hawk"], title: "Global Hawk", at: "nasa:3D Models/Global Hawk/Global Hawk.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["skylab"], title: "Skylab", at: "nasa:3D Models/Skylab/Skylab.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["mir space station", "mir station", "raumstation mir", "station mir"], title: "Mir", at: "nasa:3D Models/Mir/Mir.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["pioneer 10", "pioneer probe"], title: "Pioneer 10", at: "nasa:3D Models/Pioneer 10/Pioneer 10.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["kepler telescope", "kepler space telescope", "kepler"], title: "Kepler", at: "nasa:3D Models/Kepler (A)/Kepler (A).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["crawler transporter", "crawler"], title: "Crawler-Transporter", at: "nasa:3D Models/Crawler/Crawler.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["vehicle assembly building", "vab"], title: "Vehicle Assembly Building", at: "nasa:3D Models/Vehicle Assembly Building (VAB)/Vehicle Assembly Building (VAB).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["apollo soyuz"], title: "Apollo-Soyuz", at: "nasa:3D Models/Apollo Soyuz/Apollo Soyuz.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["dawn probe", "dawn spacecraft"], title: "Dawn", at: "nasa:3D Models/Dawn/Dawn.glb", author: "NASA/JPL", license: NASA_LICENSE },
    Curated { words: &["maven"], title: "MAVEN", at: "nasa:3D Models/Mars Atmosphere and Volatile EvolutioN (MAVEN) (A)/Mars Atmosphere and Volatile EvolutioN (MAVEN) (A).glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["bennu", "asteroid bennu", "asteroid"], title: "Asteroid Bennu", at: "nasa:3D Models/1999 RQ36 asteroid/1999 RQ36 asteroid.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["crab nebula", "krebsnebel"], title: "Crab Nebula", at: "nasa:3D Printing/Crab Nebula/Crab Nebula.glb", author: "NASA/CXC", license: NASA_LICENSE },
    Curated { words: &["orion capsule", "orion spacecraft", "orion kapsel", "orion"], title: "Orion Capsule", at: "nasa:3D Printing/Orion Capsule/Orion Capsule (no fbc).stl", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["space launch system", "sls", "artemis rocket", "sls rocket"], title: "Space Launch System", at: "nasa:3D Printing/Space Launch System (SLS) Block 1/Space Launch System (SLS) Block 1.stl", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["space exploration vehicle", "lunar rover", "mondauto"], title: "Space Exploration Vehicle", at: "nasa:3D Models/Space Exploration Vehicle/Space Exploration Vehicle.glb", author: "NASA", license: NASA_LICENSE },
    Curated { words: &["damaged helmet", "sci fi helmet", "scifi helmet", "battle helmet", "combat helmet", "flight helmet", "helmet"], title: "Damaged Helmet", at: "khronos:DamagedHelmet", author: "theblueturtle_", license: "CC BY 4.0" },
    Curated { words: &["rubber duck", "rubber duckie", "duck", "ente", "badeente", "quietscheente"], title: "Duck", at: "khronos:Duck", author: "Sony", license: "SCEA Shared Source License" },
    Curated { words: &["avocado"], title: "Avocado", at: "khronos:Avocado", author: "Microsoft", license: "CC0" },
    Curated { words: &["boombox", "boom box", "ghetto blaster"], title: "BoomBox", at: "khronos:BoomBox", author: "Microsoft", license: "CC0" },
    Curated { words: &["lantern", "laterne"], title: "Lantern", at: "khronos:Lantern", author: "Microsoft", license: "CC0" },
    Curated { words: &["water bottle", "bottle", "wasserflasche", "flasche"], title: "Water Bottle", at: "khronos:WaterBottle", author: "Microsoft", license: "CC0" },
    Curated { words: &["fox", "fuchs"], title: "Fox", at: "khronos:Fox", author: "PixelMannen, tomkranis", license: "CC0 / CC BY 4.0" },
    Curated { words: &["milk truck", "truck", "lkw", "lastwagen", "milchlaster"], title: "Milk Truck", at: "khronos:CesiumMilkTruck", author: "Cesium", license: "CC BY 4.0" },
    Curated { words: &["toy car", "spielzeugauto"], title: "Toy Car", at: "khronos:ToyCar", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["chess set", "chessboard", "chess", "schach", "schachbrett", "schachspiel"], title: "A Beautiful Game", at: "khronos:ABeautifulGame", author: "ASWF / MaterialX", license: "CC BY 4.0" },
    Curated { words: &["corset", "korsett"], title: "Corset", at: "khronos:Corset", author: "Microsoft", license: "CC0" },
    Curated { words: &["antique camera", "old camera", "vintage camera", "alte kamera"], title: "Antique Camera", at: "khronos:AntiqueCamera", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["chronograph", "wristwatch", "watch", "armbanduhr"], title: "Chronograph Watch", at: "khronos:ChronographWatch", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["chair", "armchair", "stuhl", "sessel"], title: "Sheen Chair", at: "khronos:SheenChair", author: "Wayfair", license: SEE_SOURCE },
    Curated { words: &["concept car", "car concept", "konzeptauto"], title: "Car Concept", at: "khronos:CarConcept", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["glass dragon", "dragon", "drache", "drachen"], title: "Dragon", at: "khronos:DragonAttenuation", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["mosquito in amber", "mosquito", "amber", "mücke", "bernstein"], title: "Mosquito in Amber", at: "khronos:MosquitoInAmber", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["iridescent lamp", "lamp", "lampe"], title: "Iridescence Lamp", at: "khronos:IridescenceLamp", author: "Wayfair", license: SEE_SOURCE },
    Curated { words: &["velvet sofa", "sofa", "couch"], title: "Glam Velvet Sofa", at: "khronos:GlamVelvetSofa", author: "Wayfair", license: SEE_SOURCE },
    Curated { words: &["sunglasses", "sonnenbrille"], title: "Sunglasses", at: "khronos:SunglassesKhronos", author: "Khronos", license: SEE_SOURCE },
    Curated { words: &["barramundi", "fish", "fisch"], title: "Barramundi Fish", at: "khronos:BarramundiFish", author: "Microsoft", license: "CC0" },
    Curated { words: &["refrigerator", "fridge", "kühlschrank"], title: "Commercial Refrigerator", at: "khronos:CommercialRefrigerator", author: "Khronos", license: SEE_SOURCE },
];

fn curated_url(at: &str) -> Option<(String, String)> {
    if let Some(name) = at.strip_prefix("khronos:") {
        return Some((format!("https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/{name}/glTF-Binary/{name}.glb"), "Khronos glTF Sample Assets".into()));
    }
    let path = at.strip_prefix("nasa:")?;
    let encoded = path.split('/').map(|seg| urlencoding::encode(seg).into_owned()).collect::<Vec<_>>().join("/");
    Some((format!("https://raw.githubusercontent.com/nasa/NASA-3D-Resources/master/{encoded}"), "NASA 3D Resources".into()))
}

/// The curated model a request names, with the length of the phrase that matched. Only when
/// nothing else was asked for: "an Iron Man helmet" is not the damaged helmet.
fn curated(query: &str) -> Option<(&'static Curated, usize)> {
    let q = scene3d::norm(&query.replace('\'', " "));
    let mut best: Option<(&'static Curated, &'static str)> = None;
    for c in CURATED {
        for w in c.words {
            if q.contains(&format!(" {w} ")) && best.is_none_or(|(_, b)| w.len() > b.len()) {
                best = Some((c, w));
            }
        }
    }
    let (entry, word) = best?;
    let mut rest = q.replacen(&format!(" {word} "), " ", 1);
    for (at, len, _) in scene3d::paints_at(&rest).into_iter().rev() {
        rest.replace_range(at..at + len, " ");
    }
    const ALSO: &[&str] = &["nasa", "nasas", "s", "space", "spacecraft", "probe", "telescope", "rocket", "rover", "original", "famous", "old", "vintage", "sample", "khronos", "sonde", "rakete", "teleskop", "raumsonde", "from", "von"];
    rest.split_whitespace().all(|w| scene3d::FILLER.contains(&w) || ALSO.contains(&w)).then_some((entry, word.len()))
}

/// The library entry a request names and the length of its word (for weighing it against the
/// curated index: "Saturn V" is the rocket, "Saturn" the planet).
fn library(query: &str) -> Option<(&'static scene3d::Entry, usize)> {
    let entry = scene3d::lookup(query)?;
    let q = scene3d::norm(query);
    let len = entry.words.iter().filter(|w| q.contains(&format!(" {w} "))).map(|w| w.len()).max().unwrap_or(0);
    Some((entry, len))
}

/// A Sketchfab search result.
#[derive(Clone, Debug, PartialEq)]
struct Hit {
    uid: String,
    name: String,
    author: String,
    license: String,
    likes: u64,
    faces: u64,
    glb_size: Option<u64>,
    thumb: Option<String>,
    url: String,
}

fn parse_hits(v: &Value) -> Vec<Hit> {
    v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let uid = r["uid"].as_str()?.to_string();
                    let thumbs = r["thumbnails"]["images"].as_array().cloned().unwrap_or_default();
                    // The thumbnail closest to 640 px wide.
                    let thumb = thumbs.iter().filter_map(|t| Some((t["width"].as_u64()?, t["url"].as_str()?.to_string()))).min_by_key(|(w, _)| w.abs_diff(640)).map(|(_, u)| u);
                    Some(Hit {
                        name: r["name"].as_str().unwrap_or("model").to_string(),
                        author: r["user"]["displayName"].as_str().or_else(|| r["user"]["username"].as_str()).unwrap_or("").to_string(),
                        license: r["license"]["label"].as_str().or_else(|| r["license"].as_str()).unwrap_or("").to_string(),
                        likes: r["likeCount"].as_u64().unwrap_or(0),
                        faces: r["faceCount"].as_u64().unwrap_or(0),
                        glb_size: r["archives"]["glb"]["size"].as_u64(),
                        thumb,
                        url: r["viewerUrl"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://sketchfab.com/3d-models/{uid}")),
                        uid,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The best result for a request: a name that is the thing asked for, its popularity, a size
/// that loads fast.
#[cfg(test)]
fn pick(hits: &[Hit], query: &str) -> Option<Hit> {
    ranked(hits, query).into_iter().next().map(|(_, h)| h)
}

/// What a request means to the catalogue.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    /// The thing itself ("apple"): the studio's title, the words a hit's name is held against.
    core: String,
    /// The words searched for ("apple fruit").
    search: String,
    /// Things it is not ("ipad" after "not an iPad, a real apple").
    avoid: Vec<String>,
}

/// Everyday words that are also a brand: asked for plainly ("a model of an apple") they mean
/// the thing, and the catalogue is told so (2026-11-09: "an apple" came back as an iPad Pro).
const EVERYDAY: &[(&str, &str)] = &[
    ("apple", "fruit"), ("apples", "fruit"), ("apfel", "fruit"), ("orange", "fruit"), ("blackberry", "fruit"), ("raspberry", "fruit"), ("kiwi", "fruit"), ("date", "fruit"),
    ("jaguar", "animal"), ("puma", "animal"), ("python", "snake"), ("crane", "bird"), ("turkey", "bird"), ("dove", "bird"), ("falcon", "bird"), ("mustang", "horse"), ("beetle", "insect"),
    ("shell", "seashell"), ("amazon", "river"),
];

/// "A real apple", "an eatable apple", "the apple you can eat", "apple, the fruit": the words
/// that say which sense is meant leave the name and steer the search.
fn wanted(query: &str, avoid: &[String]) -> Wanted {
    static REAL: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(?:real|actual|genuine|proper|normal|regular|ordinary|natural|living|live|echte[rnms]?|echt|richtige[rnms]?|normale[rnms]?)\b|\b(?:(?:that|which|what)\s+)?(?:you|i|one|we)\s+can\s+eat\b|\b(?:to|for|zum)\s+(?:eat|eating|essen)\b|\b(?:like|as\s+in|i\s+mean|meaning|not\s+the\s+(?:company|brand|device|phone|computer|logo))\b")
    });
    // The sense as an adjective anywhere ("eatable apple"), or as the last word ("apple fruit",
    // "apple, the fruit"): "fruit bowl" and "animal cell" are things of their own.
    static KIND: LazyLock<Regex> = LazyLock::new(|| re(r"\b(eatable|edible|essbare[rnms]?|essbar)\b|(?:^|\s)(?:the\s+|die\s+|das\s+)?(fruit|frucht|obst|food|animal|tier|bird|vogel|plant|pflanze)\s*$"));
    let q = scene3d::norm(query);
    let said: Option<&'static str> = KIND.captures(&q).map(|c| match c.get(1).or(c.get(2)).map_or("", |m| m.as_str()) {
        "animal" | "tier" => "animal",
        "bird" | "vogel" => "bird",
        "plant" | "pflanze" => "plant",
        "food" => "food",
        _ => "fruit",
    });
    let stripped = KIND.replace_all(&REAL.replace_all(&q, " "), " ").split_whitespace().filter(|w| !matches!(*w, "the" | "a" | "an")).collect::<Vec<_>>().join(" ");
    // Nothing but the sense word was said ("fruit"): that is the thing.
    let core = if stripped.is_empty() { q.trim().to_string() } else { stripped };
    let everyday = EVERYDAY.iter().find(|(w, _)| *w == core).map(|(_, kind)| *kind);
    // "Eatable" names a fruit only where the word is one; elsewhere it is plain food.
    let kind = match (said, everyday) {
        (Some("fruit"), None) if !q.contains(" fruit ") && !q.contains(" frucht ") && !q.contains(" obst ") => Some("food"),
        (Some(k), _) => Some(k),
        (None, k) => k,
    };
    let search = match kind {
        Some(k) if !core.split_whitespace().any(|w| w == k) => format!("{core} {k}"),
        _ => core.clone(),
    };
    let avoid = avoid.iter().flat_map(|a| scene3d::norm(a).split_whitespace().map(singular).collect::<Vec<_>>()).filter(|w| w.len() > 1 && !matches!(w.as_str(), "an" | "the" | "ein" | "eine" | "einen" | "der" | "die" | "das") && !core.split_whitespace().any(|c| singular(c) == *w)).collect();
    Wanted { core, search, avoid }
}

/// Nothing but a sense was named ("the fruit", "a real one", "the animal").
fn sense_only(query: &str) -> bool {
    let q = scene3d::norm(query);
    let core = wanted(query, &[]).core;
    let words = |text: &str| text.split_whitespace().filter(|w| !matches!(*w, "the" | "a" | "an" | "die" | "das")).map(str::to_string).collect::<Vec<_>>();
    let named = words(&core);
    // Only the sense word was left in ("the fruit"), or a stand-in for the thing ("a real one").
    (words(&q) == named && named.iter().all(|w| matches!(w.as_str(), "fruit" | "frucht" | "obst" | "food" | "animal" | "tier" | "bird" | "vogel" | "plant" | "pflanze")))
        || named.iter().all(|w| matches!(w.as_str(), "one" | "thing" | "kind" | "version"))
}

/// "apples" → "apple" (names are compared word by word).
fn singular(word: &str) -> String {
    let w = word.to_lowercase();
    match w.strip_suffix('s') {
        Some(stem) if w.len() > 3 && !w.ends_with("ss") && !w.ends_with("us") => stem.to_string(),
        _ => w,
    }
}

/// Words in a model's name that describe it instead of renaming it ("Low Poly Red Apple (Free)").
const DESCRIPTIVE: &[&str] = &[
    "low", "poly", "lowpoly", "high", "hd", "hq", "3d", "model", "scan", "scanned", "photogrammetry", "realistic", "real", "pbr", "game", "ready", "free", "freebie", "demo", "stylized", "stylised",
    "cartoon", "cute", "simple", "detailed", "textured", "raw", "asset", "tasty", "delicious", "fresh", "ripe", "red", "green", "yellow", "blue", "black", "white", "golden", "small", "big", "little",
    "old", "new", "rigged", "animated", "printable", "fruit", "food", "animal", "fbx", "obj", "glb", "gltf", "the", "a", "an", "my", "test", "wip", "version", "final", "download", "downloadable",
];

/// Words after which a name describes where its thing is or what goes with it ("Basket of
/// apples", "Laptop and mouse"): the thing itself stands before them.
const JOINERS: &[&str] = &["of", "with", "in", "on", "by", "for", "from", "and", "at", "inside", "under", "mit", "von", "und", "im", "auf"];

/// How well a catalogue name is the thing asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    /// The request's words in the name.
    matched: usize,
    /// The name's own head word is the request's ("Red Apple" is an apple, "Apple Watch" a watch).
    headed: bool,
    /// Words that name something else ("ipad", "pro").
    extras: usize,
}

fn fit(name: &str, words: &[String]) -> Fit {
    let normed = scene3d::norm(name);
    let tokens: Vec<String> = normed.split_whitespace().map(singular).collect();
    let is_word = |t: &String| words.iter().any(|w| w == t || (w.len() >= 5 && t.contains(w.as_str())));
    let plain = |t: &String| DESCRIPTIVE.contains(&t.as_str()) || t.chars().all(|c| c.is_ascii_digit()) || (t.len() <= 2 && t.chars().any(|c| c.is_ascii_digit()));
    let matched = words.iter().filter(|w| tokens.iter().any(|t| t == *w || (w.len() >= 5 && t.contains(w.as_str())))).count();
    let extras = tokens.iter().filter(|t| !is_word(t) && !plain(t) && !JOINERS.contains(&t.as_str())).count();
    let head = tokens.iter().take_while(|t| !JOINERS.contains(&t.as_str())).filter(|t| !plain(t)).last();
    let headed = match (head, words.last()) {
        (Some(h), Some(w)) => h == w,
        _ => false,
    };
    Fit { matched, headed, extras }
}

/// The request's words as names are compared: singular, without filler.
fn name_words(query: &str) -> Vec<String> {
    scene3d::norm(query).split_whitespace().filter(|w| w.len() > 2 && !["the", "and", "with", "for"].contains(w)).map(singular).collect()
}

/// The results best first, each with how many of the request's words its name has.
#[cfg(test)]
fn ranked(hits: &[Hit], query: &str) -> Vec<(usize, Hit)> {
    fitted(hits, query, &[]).into_iter().map(|(f, h)| (f.matched, h)).collect()
}

/// The results best first with their fit; names holding a word of `avoid` are left out.
fn fitted(hits: &[Hit], query: &str, avoid: &[String]) -> Vec<(Fit, Hit)> {
    let words = name_words(query);
    let mut out: Vec<(f32, Fit, Hit)> = hits
        .iter()
        .filter(|h| avoid.is_empty() || !scene3d::norm(&h.name).split_whitespace().any(|t| avoid.contains(&singular(t))))
        .map(|h| {
            let f = fit(&h.name, &words);
            // Popularity breaks ties between names that fit; it never outweighs the name.
            let mut score = f.matched as f32 * 3.0 + if f.headed { 5.0 } else { 0.0 } - f.extras.min(4) as f32 + (((h.likes + 1) as f32).ln() * 0.6).min(4.0);
            if h.glb_size.is_some_and(|s| s > SKETCHFAB_PREFERRED) {
                score -= 4.0;
            }
            if h.glb_size.is_none() {
                score -= 1.0;
            }
            if h.faces > 1_500_000 {
                score -= 3.0;
            }
            (score, f, h.clone())
        })
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().map(|(_, f, h)| (f, h)).collect()
}

/// The hits worth fetching for a request, best first: for one word, the names that are that
/// thing where any are ("Apple", "Red Apple", never "Apple iPad Pro"); else every name with
/// one of its words.
fn candidates(hits: &[Hit], want: &Wanted) -> Vec<Hit> {
    let all = fitted(hits, &want.core, &want.avoid);
    let single = name_words(&want.core).len() == 1;
    let headed: Vec<Hit> = all.iter().filter(|(f, _)| f.headed && f.matched > 0).map(|(_, h)| h.clone()).collect();
    if single && !headed.is_empty() {
        return headed;
    }
    all.into_iter().filter(|(f, _)| f.matched > 0).map(|(_, h)| h).collect()
}

async fn sketchfab_search(query: &str) -> Result<Vec<Hit>, String> {
    let url = format!("https://api.sketchfab.com/v3/search?type=models&downloadable=true&count=24&q={}", urlencoding::encode(query));
    let resp = http().get(&url).timeout(Duration::from_secs(12)).send().await.map_err(|e| format!("Sketchfab didn't answer: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!("Sketchfab answered {}", resp.status()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("Sketchfab's answer was unreadable: {}", e.without_url()))?;
    Ok(parse_hits(&v))
}

/// The model's archive from Sketchfab with Sir's token: its path in the cache.
async fn sketchfab_fetch(hit: &Hit, token: &str, job: u64) -> Result<PathBuf, String> {
    let safe: String = hit.uid.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    for name in [format!("sketchfab_{safe}.glb"), format!("sketchfab_{safe}.zip")] {
        if let Some(p) = cached(&name) {
            return Ok(p);
        }
    }
    let api = format!("https://api.sketchfab.com/v3/models/{safe}/download");
    let resp = http().get(&api).header("Authorization", format!("Token {token}")).timeout(Duration::from_secs(15)).send().await.map_err(|e| format!("Sketchfab didn't answer: {}", e.without_url()))?;
    match resp.status().as_u16() {
        200 => {}
        401 => return Err("Sketchfab didn't accept the token in the Integrations hub".into()),
        403 => return Err("that model can't be downloaded".into()),
        429 => return Err("Sketchfab says too many downloads for now".into()),
        s => return Err(format!("Sketchfab answered {s}")),
    }
    let v: Value = resp.json().await.map_err(|e| format!("Sketchfab's answer was unreadable: {}", e.without_url()))?;
    let (url, ext) = if let Some(u) = v["glb"]["url"].as_str() {
        (u.to_string(), "glb")
    } else if let Some(u) = v["gltf"]["url"].as_str() {
        (u.to_string(), "zip")
    } else {
        return Err("Sketchfab offered no glTF download".into());
    };
    progress(job, Some(0.08), Some("Downloading from Sketchfab…"));
    // (The archive link is pre-signed: the token never goes along.)
    let bytes = download(&url, None, job, MAX_DOWNLOAD, (0.08, 0.75)).await?;
    store(&format!("sketchfab_{safe}.{ext}"), &bytes)
}

/// Built-in models made of primitives that a downloaded real one beats (the planets, the
/// primitives and the simple machines are exact as they are).
const SHOWPIECES: &[&str] = &["iron man suit", "arc reactor", "robot", "car", "rocket", "airplane", "house", "tree", "satellite", "eiffel tower", "tower"];

/// Objaverse (Allen AI) mirrors 800,000 of Sketchfab's free models on Hugging Face, where they
/// download without a key; its index says where each model's file lies.
const OBJAVERSE: &str = "https://huggingface.co/datasets/allenai/objaverse/resolve/main/";
const OBJAVERSE_INDEX: &str = "objaverse_paths.json";

/// Model `uid`'s file in the Objaverse index ("glbs/000-023/<uid>.glb").
fn objaverse_path<'a>(index: &'a str, uid: &str) -> Option<&'a str> {
    let end = index.find(&format!("/{uid}.glb"))? + uid.len() + 5;
    let start = index[..end].rfind("glbs/")?;
    Some(&index[start..end])
}

/// A gzip file's contents (one member).
fn gunzip(gz: &[u8]) -> Result<Vec<u8>, String> {
    if gz.len() < 18 || gz[..3] != [0x1f, 0x8b, 8] {
        return Err("the index isn't gzip".into());
    }
    let flags = gz[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + u16::from_le_bytes([gz[10], gz[11]]) as usize;
    }
    for name in [8u8, 16] {
        if flags & name != 0 {
            at += gz.get(at..).and_then(|r| r.iter().position(|&b| b == 0)).ok_or("the index's header is broken")? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let size = u32::from_le_bytes(<[u8; 4]>::try_from(&gz[gz.len() - 4..]).unwrap_or_default()) as usize;
    mesh_io::inflate(gz.get(at..gz.len() - 8).ok_or("the index's header is broken")?, size)
}

/// The first of `hits` (best first) that the Objaverse mirror has, downloaded: its path in the
/// cache and the hit.
async fn objaverse_fetch(hits: &[Hit], job: u64) -> Result<(PathBuf, Hit), String> {
    let index = match cached(OBJAVERSE_INDEX) {
        Some(p) => p,
        None => {
            progress(job, Some(0.04), Some("Fetching the index of free models…"));
            let gz = download(&format!("{OBJAVERSE}object-paths.json.gz"), None, job, 64 << 20, (0.04, 0.2)).await?;
            let json = tokio::task::spawn_blocking(move || gunzip(&gz)).await.map_err(|e| e.to_string())??;
            store(OBJAVERSE_INDEX, &json)?
        }
    };
    let uids: Vec<String> = hits.iter().map(|h| h.uid.clone()).collect();
    let (i, file) = tokio::task::spawn_blocking(move || {
        let index = std::fs::read_to_string(index).ok()?;
        uids.iter().enumerate().find_map(|(i, uid)| objaverse_path(&index, uid).map(|p| (i, p.to_string())))
    })
    .await
    .ok()
    .flatten()
    .ok_or("none of the matches is in the free mirror")?;
    let hit = hits[i].clone();
    let name = format!("objaverse_{}.glb", hit.uid.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>());
    if let Some(p) = cached(&name) {
        return Ok((p, hit));
    }
    progress(job, Some(0.2), Some(&format!("Downloading \"{}\"…", hit.name)));
    let bytes = download(&format!("{OBJAVERSE}{file}"), None, job, MAX_DOWNLOAD, (0.2, 0.75)).await?;
    Ok((store(&name, &bytes)?, hit))
}

/// A downloaded Sketchfab model on the studio, credited.
fn deliver_hit(job: u64, model: Model, hit: &Hit) -> String {
    let credit = Credit { title: hit.name.clone(), author: hit.author.clone(), license: hit.license.clone(), source: "Sketchfab".into(), url: hit.url.clone() };
    deliver(job, model, Delivery { source: "Sketchfab".into(), credit: Some(credit), title: Some(hit.name.clone()), ..Delivery::default() });
    format!("\"{}\" from Sketchfab is ready, Sir.", hit.name)
}

/// The best Sketchfab match's thumbnail as a preview card.
async fn preview_of(hit: &Hit, job: u64) -> Preview {
    let mut p = Preview { title: hit.name.clone(), author: hit.author.clone(), license: hit.license.clone(), url: hit.url.clone(), thumbnail: None, id: next_id() };
    if let Some(u) = &hit.thumb {
        if let Ok(bytes) = download(u, None, job, 8 << 20, (0.0, 0.0)).await {
            p.thumbnail = mesh_io::decode_texture(&bytes);
            p.id = next_id();
        }
    }
    p
}

// -------------------------------------------------------------------------------- opening ----

fn planet_maps(p: Planet, demo: bool) -> pt::PlanetMaps {
    if p == Planet::Earth && !demo {
        for name in ["bluemarble_4096.jpg", "bluemarble_2048.jpg", "bluemarble_8192.jpg"] {
            let path = Path::new("config/earth_cache/base").join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                if let Some(maps) = pt::earth_from_photo(&bytes) {
                    return maps;
                }
            }
        }
    }
    pt::planet(p, if demo { 256 } else { pt::PLANET_WIDTH })
}

/// A built-in model (planet or scene) on screen.
async fn open_builtin(entry: &'static scene3d::Entry, args: &Value) -> Result<String, String> {
    let demo = demo_mode();
    // The studio opens at once (the scanning lattice) while the model is made.
    let (title, planet, doc) = match entry.kind {
        scene3d::Kind::Planet(p) => (capitalise(entry.key), Some(p), None),
        kind => {
            let doc = kind.scene(num_arg(args, "level").map_or(1, |l| l as u32)).unwrap_or_default();
            (doc.name.clone(), None, Some(doc))
        }
    };
    let job = begin(&title, "Built-in", "", None);
    let kept = doc.clone();
    let model = tokio::task::spawn_blocking(move || match (planet, doc) {
        (Some(p), _) => scene3d::planet_model(p, planet_maps(p, demo)),
        (None, Some(doc)) => scene3d::build(&doc),
        (None, None) => Model::default(),
    })
    .await
    .map_err(|e| e.to_string())?;
    let photo = planet == Some(Planet::Earth) && model.textures.first().is_some_and(|t| t.width >= 2048);
    deliver(job, model, Delivery { source: if photo { "Built-in · NASA Blue Marble".into() } else { "Built-in".into() }, planet, title: Some(title.clone()), doc: kept, ..Delivery::default() });
    let extra = apply_open_extras(args);
    let spoken = format!("Here's {}, Sir.", with_article(&if planet.is_some() { title.clone() } else { title.to_lowercase() }));
    Ok(reply(format!("3D studio: {title} (built-in){extra}. Drag to turn it, scroll to zoom; say \"make it gold\" or \"hologram mode\"."), spoken))
}

/// "a" or "an" ("the" for the one-of-a-kind, nothing for a planet's name).
fn article(title: &str) -> String {
    let t = title.trim().to_lowercase();
    const THE: &[&str] = &["moon", "sun", "earth", "iron man suit", "arc reactor", "international space station", "hubble space telescope", "space shuttle", "crab nebula", "saturn v", "james webb space telescope", "space launch system", "vehicle assembly building"];
    if THE.contains(&t.as_str()) {
        return "the ".into();
    }
    if scene3d::lookup(&t).is_some_and(|e| matches!(e.kind, scene3d::Kind::Planet(_)) && e.key == t) {
        return String::new();
    }
    if t.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an ".into()
    } else {
        "a ".into()
    }
}

/// A finish asked for with the request ("an Iron Man suit in gold"), applied once it is there.
fn apply_open_extras(args: &Value) -> String {
    let mut extra = String::new();
    if let Some(m) = str_arg(args, "material") {
        let paints = scene3d::paints_in(m);
        if !paints.is_empty() {
            let mut s = studio();
            if paints.len() >= 2 {
                s.edits.push(Edit { target: Target::Primary, change: Change::Paint(paints[0].clone()) });
                s.edits.push(Edit { target: Target::Secondary, change: Change::Paint(paints[1].clone()) });
            } else {
                s.edits.push(Edit { target: Target::All, change: Change::Paint(paints[0].clone()) });
            }
            rebuild(&mut s);
            touched(&mut s);
            extra = format!(" in {}", paints.iter().map(Paint::name).collect::<Vec<_>>().join(" and "));
        }
    }
    if str_arg(args, "look").is_some_and(|l| l.starts_with("holo")) {
        let mut s = studio();
        s.view.look = Look::Hologram;
        touched(&mut s);
    }
    extra
}

/// "Render me X in 3D": the library at once, else a keyless repository, else Sketchfab
/// (with Sir's token) and the language model.
async fn open_query(ctx: &ToolContext, query: &str, args: &Value) -> Result<String, String> {
    // The thing itself, without the words that only say which sense is meant ("a real apple").
    let avoid: Vec<String> = match args.get("avoid") {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    };
    let want = wanted(query.trim().trim_end_matches('.'), &avoid);
    let query = want.core.clone();
    let lib = library(&query);
    let cur = curated(&query);
    let demo = demo_mode();
    // A real model beats a built-in showpiece made of primitives (2026-11-09: "the models look
    // trash"); the built-in one stays the fallback.
    let showpiece = lib.map(|(e, _)| e).filter(|e| SHOWPIECES.contains(&e.key) && !demo);
    match (lib, cur) {
        _ if showpiece.is_some() => {}
        (Some((entry, l)), Some((_, c))) if l >= c => return open_builtin(entry, args).await,
        (Some((entry, _)), None) => return open_builtin(entry, args).await,
        _ => {}
    }
    if let Some((entry, _)) = cur {
        if demo {
            return Ok(stand_in(&query, entry.title, args));
        }
        return Ok(open_curated(entry, args));
    }
    if demo {
        return Ok(stand_in(&query, &title_case(&query), args));
    }
    let token = ctx.pim.config().vault().secret(SKETCHFAB_SERVICE, crate::vault::TOKEN).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let llm = ctx.pim.llm().cloned();
    let cfg = ctx.config.snapshot();
    let title = title_case(&query);
    let job = begin(&title, "Sketchfab", "Searching Sketchfab…", Some(Arc::new(placeholder_for(&query))));
    let material = str_arg(args, "material").map(str::to_string);
    let look = str_arg(args, "look").map(str::to_string);
    let q = query.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        let outcome = fetch_or_compose(job, &want, token, llm, cfg, showpiece).await;
        if !current(job) {
            return;
        }
        let extras = json!({ "material": material, "look": look });
        match outcome {
            Ok(line) => {
                apply_open_extras(&extras);
                if started.elapsed() > Duration::from_secs(4) {
                    crate::announcements::say(line);
                }
            }
            Err(why) => {
                warn!("[3D] '{q}' failed: {why}");
                fail(job, &why);
                crate::announcements::say(format!("I couldn't build {q}, Sir: {why}."));
            }
        }
    });
    Ok(reply(format!("3D studio: working on {query}."), format!("Finding {} for you, Sir.", with_article(&query))))
}

fn with_article(q: &str) -> String {
    let q = q.trim();
    let lower = q.to_lowercase();
    if ["a ", "an ", "the ", "my ", "some "].iter().any(|a| lower.starts_with(a)) {
        q.to_string()
    } else {
        format!("{}{q}", article(q))
    }
}

fn title_case(q: &str) -> String {
    q.split_whitespace()
        .map(|w| if w.len() <= 2 && !w.chars().all(|c| c.is_ascii_digit()) && w != "x" { w.to_string() } else { capitalise(w) })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The wireframe shown while a model is on its way: the closest built-in shape, else a lattice.
fn placeholder_for(query: &str) -> Vec<[[f32; 3]; 2]> {
    let q = scene3d::norm(query);
    for e in scene3d::LIBRARY {
        if let Some(doc) = e.kind.scene(1).filter(|_| e.words.iter().any(|w| q.contains(&format!(" {w} ")))) {
            return wire_of(&scene3d::build(&doc), 1800);
        }
    }
    let humanoid = ["suit", "armor", "armour", "robot", "man", "woman", "person", "hero", "knight", "soldier", "figure", "anzug", "rüstung", "ritter"];
    if humanoid.iter().any(|w| q.contains(&format!(" {w} "))) {
        if let Some(doc) = scene3d::lookup("iron man suit").and_then(|e| e.kind.scene(1)) {
            return wire_of(&scene3d::build(&doc), 1800);
        }
    }
    scan_sphere()
}

/// The verifier's and the tests' answer for anything outside the library: a built-in stand-in,
/// clearly marked, with no download and no language model.
fn stand_in(query: &str, title: &str, args: &Value) -> String {
    let q = scene3d::norm(query);
    let doc = scene3d::LIBRARY
        .iter()
        .filter(|e| e.words.iter().any(|w| q.contains(&format!(" {w} "))))
        .find_map(|e| e.kind.scene(1))
        .unwrap_or_else(|| {
            let key = if ["suit", "armor", "armour", "robot"].iter().any(|w| q.contains(w)) { "iron man suit" } else { "torus knot" };
            scene3d::lookup(key).and_then(|e| e.kind.scene(1)).unwrap_or_default()
        });
    let job = begin(title, "Demo stand-in", "", None);
    deliver(job, scene3d::build(&doc), Delivery { source: "Demo stand-in (no download in a test run)".into(), title: Some(title.to_string()), ..Delivery::default() });
    let extra = apply_open_extras(args);
    reply(format!("3D studio: {title}{extra} (a built-in stand-in: test runs fetch nothing)."), format!("Here's {}, Sir.", with_article(&title.to_lowercase())))
}

fn open_curated(entry: &'static Curated, args: &Value) -> String {
    let Some((url, source)) = curated_url(entry.at) else {
        return reply("That model's address is broken.", "I couldn't find that model, Sir.");
    };
    let title = entry.title.to_string();
    let job = begin(&title, &source, &format!("Fetching from {source}…"), Some(Arc::new(placeholder_for(&title))));
    let ext = if entry.at.to_ascii_lowercase().ends_with(".stl") { "stl" } else { "glb" };
    let file: String = entry.at.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let file = format!("{}.{ext}", file.chars().rev().take(80).collect::<String>().chars().rev().collect::<String>());
    let credit = Credit { title: title.clone(), author: entry.author.into(), license: entry.license.into(), source: source.clone(), url: url.clone() };
    let extras = json!({ "material": str_arg(args, "material"), "look": str_arg(args, "look") });
    tokio::spawn(async move {
        let started = Instant::now();
        let path = match cached(&file) {
            Some(p) => Ok(p),
            None => match download(&url, None, job, MAX_DOWNLOAD, (0.0, 0.7)).await {
                Ok(bytes) => store(&file, &bytes),
                Err(e) => Err(e),
            },
        };
        let model = match path {
            Ok(p) => load_path(p, job, 0.7).await,
            Err(e) => Err(e),
        };
        match model {
            Ok(m) => {
                info!("[3D] {} from {}: {} triangles in {} ms", credit.title, credit.source, m.triangles(), started.elapsed().as_millis());
                let t = credit.title.clone();
                if deliver(job, m, Delivery { source: credit.source.clone(), credit: Some(credit), ..Delivery::default() }) {
                    apply_open_extras(&extras);
                    if started.elapsed() > Duration::from_secs(4) {
                        crate::announcements::say(format!("The {t} is ready, Sir."));
                    }
                }
            }
            Err(e) => {
                warn!("[3D] {} failed: {e}", credit.title);
                fail(job, &format!("I couldn't fetch it: {e}"));
            }
        }
    });
    reply(format!("3D studio: fetching {title} from {source}."), format!("Bringing up {}, Sir.", with_article(&title)))
}

/// Sketchfab with Sir's token, else the preview card and a composed model.
async fn fetch_or_compose(job: u64, want: &Wanted, token: Option<String>, llm: Option<Arc<crate::runtime::LlmRuntime>>, cfg: crate::config::JaiConfig, builtin: Option<&'static scene3d::Entry>) -> Result<String, String> {
    let query = want.core.as_str();
    let search = sketchfab_search(&want.search);
    // No real model: rebuilt from a photo (found by the sense meant), else composed from primitives.
    let photo_query = want.search.clone();
    let compose_task = move |llm: Option<Arc<crate::runtime::LlmRuntime>>, cfg: crate::config::JaiConfig, q: String| async move {
        match from_photo(job, &q, &photo_query, &cfg).await {
            Some(done) => Ok(done),
            None if !current(job) => Err("cancelled".into()),
            None => compose(job, &q, llm, &cfg, None).await,
        }
    };
    match token {
        Some(token) => {
            let hits = search.await.unwrap_or_else(|e| {
                warn!("[3D] Sketchfab search failed: {e}");
                Vec::new()
            });
            if let Some(hit) = candidates(&hits, want).into_iter().next() {
                let card = preview_of(&hit, job).await;
                set_preview(job, Some(card));
                progress(job, Some(0.05), Some(&format!("Downloading \"{}\"…", hit.name)));
                match sketchfab_fetch(&hit, &token, job).await {
                    Ok(path) => match load_path(path, job, 0.75).await {
                        Ok(model) => return Ok(deliver_hit(job, model, &hit)),
                        Err(e) => set_note(job, Some(format!("Sketchfab's file didn't load ({e}); composing one instead."))),
                    },
                    Err(e) => set_note(job, Some(format!("Sketchfab: {e}. Composing one instead."))),
                }
            }
            match builtin {
                Some(entry) => open_builtin(entry, &json!({})).await,
                None => compose_task(llm, cfg, query.to_string()).await,
            }
        }
        None => {
            // A real model from the free mirror first.
            let hits = search.await.unwrap_or_else(|e| {
                warn!("[3D] Sketchfab search failed: {e}");
                Vec::new()
            });
            let named = candidates(&hits, want);
            if !named.is_empty() {
                match objaverse_fetch(&named, job).await {
                    Ok((path, hit)) => match load_path(path, job, 0.75).await {
                        Ok(model) => return Ok(deliver_hit(job, model, &hit)),
                        Err(e) => warn!("[3D] The free copy of '{}' didn't load: {e}", hit.name),
                    },
                    Err(e) => warn!("[3D] No free model for '{query}': {e}"),
                }
            }
            if let Some(entry) = builtin {
                return open_builtin(entry, &json!({})).await;
            }
            // Else the card and a composition at once.
            let card = async {
                if let Some(hit) = named.first().cloned() {
                    let p = preview_of(&hit, job).await;
                    set_preview(job, Some(p));
                    set_note(job, Some("Full Sketchfab models: paste your Sketchfab API token in Integrations → Quick Connections → Sketchfab.".into()));
                }
            };
            let (_, composed) = tokio::join!(card, compose_task(llm, cfg, query.to_string()));
            // The card stays up beside the composed stand-in.
            {
                let mut s = studio();
                if s.job == job && s.preview.is_none() {
                    s.note = None;
                    touched(&mut s);
                }
            }
            composed
        }
    }
}

/// Photos tried for photo-to-3D (each is cut out; the cleanest whole subject wins).
const PHOTO_TRIES: usize = 4;

/// The thing rebuilt from a photograph (`photo3d`), credited to the photo. None when the networks
/// aren't installed, no photo shows one whole subject or the rebuild fails: the composer takes
/// over.
async fn from_photo(job: u64, query: &str, photo_query: &str, cfg: &crate::config::JaiConfig) -> Option<String> {
    use crate::image_viewer::{self as iv, Fetched};
    let dir = crate::photo3d::dir(cfg);
    if !crate::photo3d::installed(&dir) {
        info!("[3D] Photo to 3D isn't installed ({}); composing '{query}'", dir.display());
        return None;
    }
    progress(job, Some(0.02), Some("Looking for a photo of it…"));
    let mut photos = Vec::new();
    for p in iv::photos(photo_query).await {

        for url in std::iter::once(&p.full).chain(&p.alt) {
            if let Ok(Fetched::Bytes { bytes, .. }) = iv::fetch(url, true, None).await {
                photos.push((p, bytes));
                break;
            }
        }
        if photos.len() == PHOTO_TRIES || !current(job) {
            break;
        }
    }
    if photos.is_empty() || !current(job) {
        warn!("[3D] No photo of '{query}' to rebuild it from");
        return None;
    }
    let title = title_case(query);
    let name = title.clone();
    let bytes: Vec<Vec<u8>> = photos.iter().map(|(_, b)| b.clone()).collect();
    let built = tokio::task::spawn_blocking(move || {
        let go = |p: f32, status: &str| {
            progress(job, Some(p), Some(status));
            current(job)
        };
        // Half the threads: the HUD (and Sir's game) keep the rest.
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get() / 2);
        crate::photo3d::build(&dir, &bytes, &name, threads, &go)
    })
    .await;
    let (model, chosen) = match built {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            warn!("[3D] Photo to 3D of '{query}' failed: {e}");
            return None;
        }
        Err(e) => {
            warn!("[3D] Photo to 3D of '{query}' stopped: {e}");
            return None;
        }
    };
    let photo = &photos[chosen].0;
    let credit = Credit { title: photo.title.clone(), author: photo.creator.clone(), license: photo.license.clone(), source: format!("{}, rebuilt in 3D by TripoSR", photo.source), url: photo.page.clone() };
    if !deliver(job, model, Delivery { source: "Photo → 3D (TripoSR)".into(), credit: Some(credit), title: Some(title.clone()), ..Delivery::default() }) {
        return None;
    }
    Some(format!("I found no real model of {}, Sir, so I rebuilt it from a photograph.", with_article(&title.to_lowercase())))
}

const COMPOSE_PROMPT: &str ="You build 3D models out of simple shapes. Reply with JSON only:\n{\"name\":\"…\",\"materials\":{\"<name>\":{\"color\":\"#rrggbb\",\"metallic\":0-1,\"roughness\":0-1,\"emissive\":0-5}},\"parts\":[{\"name\":\"…\",\"shape\":\"box|rounded_box|sphere|cylinder|cone|torus|capsule|dome|truss|lathe|extrude\",\"size\":[w,h,d],\"position\":[x,y,z],\"rotation\":[x,y,z],\"material\":\"<name>\"}]}\nMetres at real size, Y up, the object stands on y=0 and faces +Z. position is the part's centre, size its full width/height/depth, rotation in degrees. cylinder \"top\": top radius share (0.5 tapers). truss: an open steel lattice filling its box (towers, cranes, bridges, pylons), \"top\": top width share, \"bays\": sections. lathe \"points\": [[radius,height]…] in -0.5..0.5 turned around Y (vases, bottles, domes). extrude \"points\": [[x,y]…] in -0.5..0.5, depth along Z (wings, fins, signs). 12 to 30 parts named after the real parts (helmet, left arm, wheel …), real proportions and colours, glowing parts emissive. Write materials first, then parts.";

/// What the composer is asked to do with a model it (or the library) made.
const REFINE_ASK: &str = "Make this 3D model of {q} more detailed and more accurate. Keep its overall size, position and colours. Split big parts into their real sub-parts, fix wrong proportions and add the missing details: 30 to 60 parts. Reply with the whole new model as JSON in the same format.\nThe model now: ";

/// The language model composes the object as a scene of primitives; parts appear as they are
/// written.
async fn compose(job: u64, query: &str, llm: Option<Arc<crate::runtime::LlmRuntime>>, cfg: &crate::config::JaiConfig, base: Option<Value>) -> Result<String, String> {
    use futures_util::StreamExt;
    let fallback = |why: &str| -> Result<String, String> {
        if base.is_some() {
            // (A refinement that fails leaves the model as it was.)
            return Err(why.to_string());
        }
        // Without the language model: the closest built-in shape, said plainly.
        let q = scene3d::norm(query);
        let doc = scene3d::LIBRARY.iter().filter(|e| e.words.iter().any(|w| q.contains(&format!(" {w} ")))).find_map(|e| e.kind.scene(1));
        match doc {
            Some(d) => {
                let name = d.name.clone();
                deliver(job, scene3d::build(&d), Delivery { source: "Built-in stand-in".into(), keep_preview: true, ..Delivery::default() });
                set_note(job, Some(format!("{why}: showing the built-in {} instead.", name.to_lowercase())));
                Ok(format!("I couldn't compose it, Sir, so here's my own {} instead.", name.to_lowercase()))
            }
            None => Err(why.to_string()),
        }
    };
    let Some(llm) = llm else { return fallback("the language engine isn't running") };
    if !crate::vram_guard::llm_allowed() {
        return fallback("the language engine is resting while a game runs");
    }
    if llm.wait_ready(Duration::from_secs(20)).await.is_err() {
        return fallback("the language engine isn't ready");
    }
    let refine = base.is_some();
    progress(job, None, Some(if refine { "Refining it…" } else { "Composing it from parts…" }));
    let started = Instant::now();
    let ask = match &base {
        Some(doc) => format!("{}{doc}", REFINE_ASK.replace("{q}", query)),
        None => query.to_string(),
    };
    let resp = http()
        .post(format!("{}/api/chat", llm.url()))
        .json(&json!({
            "model": cfg.primary_model,
            "messages": [{ "role": "system", "content": COMPOSE_PROMPT }, { "role": "user", "content": ask }],
            "format": "json",
            "stream": true,
            "think": false,
            "keep_alive": crate::vram_guard::keep_alive_value(cfg),
            "options": crate::brain::Brain::json_options(cfg, if refine { 4000 } else { 2200 }, 0.35)
        }))
        .timeout(if refine { COMPOSE_TIMEOUT * 3 } else { COMPOSE_TIMEOUT })
        .send()
        .await;
    let resp = match resp {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => return fallback(&format!("the language engine answered {}", r.status())),
        Err(e) => return fallback(&format!("the language engine didn't answer ({})", e.without_url())),
    };
    let mut stream = resp.bytes_stream();
    let (mut line, mut text) = (Vec::<u8>::new(), String::new());
    let (mut shown, mut last_try) = (0usize, Instant::now());
    let title = title_case(query);
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        for b in chunk {
            if b != b'\n' {
                line.push(b);
                continue;
            }
            if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                if let Some(c) = v["message"]["content"].as_str() {
                    text.push_str(c);
                }
            }
            line.clear();
        }
        if !current(job) {
            return Err("cancelled".into());
        }
        // A new part finished: show what there is.
        if last_try.elapsed() > Duration::from_millis(200) && text.matches('}').count() > shown + 1 {
            last_try = Instant::now();
            if let Ok(doc) = scene3d::parse_scene(&text) {
                if doc.parts.len() > shown {
                    shown = doc.parts.len();
                    // (A refined model replaces the one on screen only once it is whole.)
                    if !refine {
                        deliver(job, scene3d::build(&doc), Delivery { source: "Composed".into(), partial: true, keep_preview: true, title: Some(title.clone()), ..Delivery::default() });
                    }
                    progress(job, None, Some(&format!("{} {shown} parts", if refine { "Refining…" } else { "Composing…" })));
                }
            }
        }
    }
    let doc = match scene3d::parse_scene(&text) {
        Ok(d) => d,
        Err(e) => return fallback(&format!("the composition didn't work ({e})")),
    };
    info!("[3D] {} '{query}': {} parts in {} ms", if refine { "Refined" } else { "Composed" }, doc.parts.len(), started.elapsed().as_millis());
    let parts = doc.parts.len();
    if refine {
        // A refinement that came back poorer than what it started from is not shown.
        let before = base.as_ref().and_then(|b| b["parts"].as_array()).map_or(0, Vec::len);
        if parts <= before {
            return Err(format!("the refinement came back with {parts} parts, no more than the {before} it had"));
        }
    }
    let title = if refine { studio().title.clone() } else { title };
    deliver(job, scene3d::build(&doc), Delivery { source: "Composed".into(), keep_preview: true, title: Some(title), doc: Some(doc), ..Delivery::default() });
    if refine {
        return Ok(format!("Refined, Sir: {parts} parts now."));
    }
    Ok(format!("Your {} is ready, Sir: {parts} parts.", query.trim_start_matches("a ").trim_start_matches("an ")))
}

/// A model file: a path, or the newest one in Downloads.
async fn open_local(spec: &str, args: &Value) -> Result<String, String> {
    let path = if ["downloads", "download", "downloaded", "newest", "latest"].contains(&spec.to_ascii_lowercase().as_str()) {
        if demo_mode() {
            return Ok(reply("3D studio: no files are read in a test run.", "In a test run I don't read your Downloads, Sir."));
        }
        match newest_model(&crate::features::user_folder("Downloads")) {
            Some(p) => p,
            None => return Ok(reply("There's no 3D model file (glb, gltf, obj, stl or zip) in Downloads.", "I don't see a 3D model in your Downloads, Sir.")),
        }
    } else {
        let spec = spec.trim_matches('"');
        let p = if let Some(rest) = spec.strip_prefix("~/").or_else(|| spec.strip_prefix("~\\")) { crate::apps::home_dir().unwrap_or_default().join(rest) } else { PathBuf::from(spec) };
        if !p.is_file() {
            return Ok(reply(format!("{} doesn't exist.", p.display()), "I can't find that file, Sir."));
        }
        if !mesh_io::Format::is_model_file(&p) && mesh_io::Format::of_path(&p) != Some(mesh_io::Format::Zip) {
            return Ok(reply(format!("{} is not a model file (glb, gltf, obj, stl or zip).", p.display()), "That isn't a 3D model file, Sir."));
        }
        p
    };
    Ok(open_file(path, args))
}

/// The newest model file in a folder (and one level below it).
fn newest_model(dir: &Path) -> Option<PathBuf> {
    walkdir::WalkDir::new(dir)
        .max_depth(2)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| mesh_io::Format::is_model_file(e.path()) || mesh_io::Format::of_path(e.path()) == Some(mesh_io::Format::Zip) && zip_has_model(e.path()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.into_path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// A ZIP whose directory names a model file (read without unpacking).
fn zip_has_model(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.len() > MAX_DOWNLOAD * 2 {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else { return false };
    let tail = &bytes[bytes.len().saturating_sub(1 << 20)..];
    let text = String::from_utf8_lossy(tail).to_lowercase();
    [".gltf", ".glb", ".obj", ".stl"].iter().any(|e| text.contains(e))
}

/// Opens a model file (from a path, Downloads or a drop on the studio).
pub fn open_file(path: PathBuf, args: &Value) -> String {
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "model".into());
    let title = name.replace(['_', '-'], " ");
    let job = begin(&title, "Your file", "Loading…", None);
    let extras = json!({ "material": str_arg(args, "material"), "look": str_arg(args, "look") });
    let shown = path.display().to_string();
    let handle = tokio::runtime::Handle::try_current();
    let work = async move {
        match load_path(path, job, 0.0).await {
            Ok(m) => {
                info!("[3D] Loaded {shown}: {} triangles", m.triangles());
                if deliver(job, m, Delivery { source: "Your file".into(), ..Delivery::default() }) {
                    apply_open_extras(&extras);
                }
            }
            Err(e) => fail(job, &e),
        }
    };
    match handle {
        Ok(h) => {
            h.spawn(work);
        }
        Err(_) => {
            std::thread::spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    rt.block_on(work);
                }
            });
        }
    }
    reply(format!("3D studio: loading {name}."), format!("Loading {title}, Sir."))
}

/// A file dropped on the studio (from the panel, which may run outside the tokio runtime).
pub fn open_dropped(path: PathBuf, rt: &tokio::runtime::Handle) {
    let _guard = rt.enter();
    open_file(path, &Value::Null);
}

#[cfg(test)]
mod tests {
    use super::*;

    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn the_model_on_show_named_again_is_changed_not_rebuilt() {
        // The model's calls of 2026-11-09 while the Iron Man suit was up.
        let change = |q: &str| change_to_shown(q, "iron man suit");
        assert_eq!(change("iron man suit green colour").as_deref(), Some("green colour"));
        assert!(change("iron man suit maximum detail resolution").is_some_and(|c| DETAIL_WORDS.is_match(&c)));
        assert!(change("iron man suit hyper detailed").is_some_and(|c| DETAIL_WORDS.is_match(&c)));
        assert_eq!(change("Iron Man suit"), None);
        assert_eq!(change("the eiffel tower"), None);
        let green = edit_command(&clean("make it green colour"), false, &[]).unwrap();
        assert_eq!(green["material"], "green");
    }

    #[test]
    fn the_one_tool_stays_terse_and_takes_numbers_as_text() {
        let defs = definitions();
        assert_eq!(defs.len(), 1);
        let chars = serde_json::to_string(&defs).unwrap().len();
        println!("show_3d_model schema: {chars} chars ≈ {} tokens", chars / 4 + 1);
        assert!(chars <= 1_000, "{chars} chars: the schema is in every prompt");
        assert_eq!(num_arg(&json!({ "h": "18 cm" }), "h"), Some(18.0));
        assert_eq!(num_arg(&json!({ "h": "0,5" }), "h"), Some(0.5));
        assert_eq!(num_arg(&json!({ "h": 12 }), "h"), Some(12.0));
        assert_eq!(num_arg(&json!({ "h": "big" }), "h"), None);
    }

    fn lock() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn args_of(text: &str, open: bool) -> Option<Value> {
        command(text, open, false)
    }

    fn query_of(text: &str) -> Option<String> {
        args_of(text, false).and_then(|a| a["query"].as_str().map(str::to_string))
    }

    #[test]
    fn opening_phrases_in_english_and_german() {
        assert_eq!(query_of("JAI, render me an Iron Man suit in 3D").as_deref(), Some("iron man suit"));
        assert_eq!(query_of("render me an iron man suit in three d please").as_deref(), Some("iron man suit"));
        assert_eq!(query_of("Show me a 3D model of Earth.").as_deref(), Some("earth"));
        assert_eq!(query_of("show me a 3-D model of the earth").as_deref(), Some("earth"));
        assert_eq!(query_of("Can you show me the Hubble telescope in 3D?").as_deref(), Some("hubble telescope"));
        assert_eq!(query_of("3D model of a rocket").as_deref(), Some("rocket"));
        assert_eq!(query_of("build me a 3d model of a castle").as_deref(), Some("castle"));
        assert_eq!(query_of("Zeig mir den Mars in 3D").as_deref(), Some("mars"));
        assert_eq!(query_of("zeig mir ein 3D-Modell von der Erde").as_deref(), Some("erde"));
        assert_eq!(query_of("3D-Modell vom Saturn").as_deref(), Some("saturn"));
        assert_eq!(query_of("Rendere mir einen Roboter in 3D").as_deref(), Some("roboter"));
        assert_eq!(query_of("render me an iron man suit").as_deref(), Some("iron man suit"));
        let a = args_of("render me an iron man suit in gold in 3d", false).unwrap();
        assert_eq!((a["query"].as_str(), a["material"].as_str()), (Some("iron man suit"), Some("gold")));
        assert_eq!(args_of("open the 3D studio", false).unwrap()["action"], "show");
        assert_eq!(args_of("öffne das 3D-Studio", false).unwrap()["action"], "show");
        assert_eq!(args_of("show me the model in my downloads", false).unwrap()["path"], "downloads");
        assert_eq!(args_of("open the stl file I just downloaded", false).unwrap()["path"], "downloads");
        assert_eq!(args_of(r"open C:\Users\me\Desktop\suit.glb in 3d", false).unwrap()["path"], r"C:\Users\me\Desktop\suit.glb");
        // Not the studio's.
        for t in ["render me a video of the party", "show me the earth", "what's the weather", "zoom in", "make it gold", "show me my schedule", "open the downloads folder", "show it in 3d"] {
            assert_eq!(args_of(t, false), None, "{t}");
        }
    }

    /// Sir's words from the log: the tower said twice, "more detailed", saved where he says.
    #[test]
    fn more_detail_the_eiffel_tower_and_saving_anywhere() {
        assert_eq!(query_of("JAI show me a 3D model, model of the Eiffel Tower.").as_deref(), Some("eiffel tower"));
        let edit = |t: &str| args_of(t, true);
        for t in ["Make it more detailed.", "Make it way more detailed.", "enhance it", "more detail", "give me a more accurate version", "mach es detaillierter", "make the 3D model more detailed"] {
            assert_eq!(edit(t).map(|a| a["action"].clone()), Some(json!("enhance")), "{t}");
        }
        assert_eq!(args_of("enhance it", false), None, "only while the studio is open");
        let saved = |t: &str| edit(t).map(|a| (a["action"].as_str().map(str::to_string), a["format"].as_str().map(str::to_string), a["folder"].as_str().map(str::to_string)));
        let stl = |folder: &str| Some((Some("export".to_string()), Some("stl".to_string()), Some(folder.to_string())));
        assert_eq!(saved("save it to my desktop"), stl("desktop"));
        assert_eq!(saved(r"save it to D:\Prints"), stl(r"D:\Prints"));
        assert_eq!(saved("save it somewhere else"), stl("ask"));
        assert_eq!(saved("export it as OBJ to my downloads").map(|s| (s.1, s.2)), Some((Some("obj".to_string()), Some("downloads".to_string()))));
        // (A whole path is kept as it is: a drive on Windows, a path from "/" on Linux.)
        let anywhere = if cfg!(windows) { r"D:\Prints" } else { "/mnt/prints" };
        assert_eq!(folder_path(anywhere), PathBuf::from(anywhere));
        assert!(folder_path("desktop").ends_with("Desktop"));
    }

    /// A standing figure in one piece, the way a downloaded suit arrives ("Object_2"): boxes for
    /// the legs, the torso, the arms, the neck and the head, 1.8 tall.
    fn one_piece_figure() -> Model {
        let mut body = Part { name: "Object_2".into(), ..Part::default() };
        let mut cube = |min: [f32; 3], max: [f32; 3]| {
            let base = body.positions.len() as u32;
            for k in 0..8u32 {
                body.positions.push([if k & 1 == 0 { min[0] } else { max[0] }, if k & 2 == 0 { min[1] } else { max[1] }, if k & 4 == 0 { min[2] } else { max[2] }]);
                body.normals.push([0.0, 1.0, 0.0]);
            }
            for t in [[0, 1, 3], [0, 3, 2], [4, 6, 7], [4, 7, 5], [0, 4, 5], [0, 5, 1], [2, 3, 7], [2, 7, 6], [0, 2, 6], [0, 6, 4], [1, 5, 7], [1, 7, 3]] {
                body.indices.extend(t.iter().map(|i| base + i));
            }
        };
        cube([-0.2, 0.0, -0.1], [-0.04, 0.85, 0.1]); // legs
        cube([0.04, 0.0, -0.1], [0.2, 0.85, 0.1]);
        cube([-0.2, 0.85, -0.12], [0.2, 1.5, 0.12]); // torso
        cube([-0.34, 0.8, -0.07], [-0.23, 1.45, 0.07]); // arms
        cube([0.23, 0.8, -0.07], [0.34, 1.45, 0.07]);
        cube([-0.05, 1.5, -0.05], [0.05, 1.55, 0.05]); // neck
        cube([-0.1, 1.58, -0.1], [0.1, 1.8, 0.1]); // head
        Model { name: "Iron Man suit".into(), parts: vec![body], materials: vec![Material::default()], unit_m: 1.0, ..Model::default() }
    }

    /// 2026-11-09, "Make the helmet yellow." with a downloaded suit on show: no part was called
    /// helmet, so the sentence went to the model, which turned the interface yellow and said the
    /// helmet was.
    #[test]
    fn the_helmet_of_a_one_piece_suit_is_painted_by_its_place() {
        let mut model = one_piece_figure();
        assert!(named_parts(&model, "helmet").is_empty(), "the suit came in one piece");
        assert!(is_figure(&model));
        let all = model.triangles();
        let (name, zone) = zone_of("helmet").unwrap();
        assert_eq!((name, zone_of("Kopf").map(|z| z.0), zone_of("mask").map(|z| z.0)), ("helmet", Some("helmet"), Some("helmet")));
        let taken = carve(&mut model, name, zone);
        assert_eq!(taken, 12, "the head's twelve triangles, nothing of the neck or the shoulders");
        assert_eq!(model.triangles(), all, "nothing lost");
        let helmet = named_parts(&model, "helmet");
        assert_eq!(helmet.len(), 1);
        let b = model.parts[helmet[0]].bounds();
        assert!(b.min[1] >= 1.57 && b.max[1] <= 1.81 && b.min[0] >= -0.11 && b.max[0] <= 0.11, "{b:?}");
        assert_eq!(model.parts[helmet[0]].positions.len(), 8, "only the vertices it uses");
        // Painted: the helmet alone changes.
        let yellow = scene3d::paints_in("yellow").remove(0);
        let before = model.materials[0].base_color;
        let (materials, _, part_material) = effective(&model, &[Edit { target: Target::Named("helmet".into()), change: Change::Paint(yellow) }]);
        for (i, part) in model.parts.iter().enumerate() {
            let c = materials[part_material[i]].base_color;
            if part.name == "helmet" {
                assert!(c[0] > 0.7 && c[1] > 0.5 && c[2] < 0.35, "yellow: {c:?}");
            } else {
                assert_eq!(c, before, "{} keeps its look", part.name);
            }
        }
        // The other places of a figure, and of anything.
        // (The chest: the torso's sides and top, and the neck's underside on it.)
        for (word, tris) in [("arms", 24), ("boots", 4), ("chest", 12)] {
            let mut m = one_piece_figure();
            let (name, zone) = zone_of(word).unwrap();
            assert_eq!(carve(&mut m, name, zone), tris, "{word}");
        }
        let mut m = one_piece_figure();
        let (name, zone) = zone_of("top half").unwrap();
        let top = carve(&mut m, name, zone);
        assert!(top > 12 && top < all, "{top} of {all}");
        // A car has no helmet; a word that is no place is no zone ("make the note gold").
        let mut car = Part { name: "body".into(), ..Part::default() };
        car.positions = vec![[-2.0, 0.0, -0.9], [2.0, 0.0, -0.9], [2.0, 1.4, 0.9], [-2.0, 1.4, 0.9]];
        car.indices = vec![0, 1, 2, 0, 2, 3];
        assert!(!is_figure(&Model { parts: vec![car], materials: vec![Material::default()], unit_m: 1.0, ..Model::default() }));
        assert!(zone_of("note").is_none() && zone_of("theme").is_none() && zone_of("interface").is_none());
    }

    #[test]
    fn a_paint_order_names_its_thing_or_the_interface() {
        for (said, thing) in [("Make the helmet yellow.", "helmet"), ("paint the arms silver", "arms"), ("mach den Helm rot", "helm"), ("JAI, make the cape red", "cape")] {
            assert_eq!(painted_thing(said).as_deref(), Some(thing), "{said}");
        }
        for said in ["make it yellow", "make the theme yellow", "make the interface yellow", "yellow please", "change the accent to gold", "make yourself red", "what time is it", "make the background darker"] {
            assert_eq!(painted_thing(said), None, "{said}");
        }
    }

    #[test]
    fn edits_only_while_the_studio_is_open() {


        let edit = |t: &str| args_of(t, true);
        assert_eq!(edit("make it gold").unwrap()["material"], "gold");
        // Sir's words of 2026-11-09 as Whisper heard them ("Can you…" → "And you…").
        assert_eq!(edit("And you made it green.").unwrap()["material"], "green");
        let iron = edit("And you change it to Iron.");
        assert!(iron.as_ref().and_then(|v| v["material"].as_str()).is_some_and(|m| m == "steel"), "iron is the steel finish: {iron:?}");
        assert_eq!(edit("And you give it way more detail.").unwrap()["action"], "enhance");
        assert_eq!(edit("Can you make it more detailed?").unwrap()["action"], "enhance");
        assert_eq!(edit("JAI, make it carbon fibre").unwrap()["material"], "carbon fibre");
        assert_eq!(edit("give it a marble texture").unwrap()["material"], "marble");
        assert_eq!(edit("paint it red and gold").unwrap()["material"], "red and gold");
        let h = edit("make the helmet blue").unwrap();
        assert_eq!((h["part"].as_str(), h["material"].as_str()), (Some("helmet"), Some("blue")));
        let de = edit("mach den Helm rot").unwrap();
        assert_eq!((de["part"].as_str(), de["material"].as_str()), (Some("helm"), Some("red")));
        let two = edit("paint the helmet blue and the arms silver").unwrap();
        assert_eq!(two["edits"][0]["part"], "helmet");
        assert_eq!(two["edits"][1]["part"], "arms");
        assert_eq!(two["edits"][1]["material"], "silver");
        assert_eq!(edit("mach es aus Holz").unwrap()["material"], "wood");
        assert_eq!(edit("carbon fibre").unwrap()["material"], "carbon fibre");
        assert_eq!(edit("make it glow").unwrap()["glow"], "on");
        let g = edit("make the eyes glow red").unwrap();
        assert_eq!((g["part"].as_str(), g["glow"].as_str()), (Some("eyes"), Some("red")));
        assert_eq!(edit("lass es leuchten").unwrap()["glow"], "on");
        assert_eq!(edit("stop glowing").unwrap()["glow"], false);
        assert_eq!(edit("wireframe").unwrap()["wireframe"], true);
        assert_eq!(edit("turn off the wireframe").unwrap()["wireframe"], false);
        assert_eq!(edit("hologram mode").unwrap()["look"], "hologram");
        assert_eq!(edit("make it a hologram").unwrap()["look"], "hologram");
        assert_eq!(edit("realistic mode").unwrap()["look"], "realistic");
        assert_eq!(edit("spin faster").unwrap()["spin"], "faster");
        assert_eq!(edit("stop spinning").unwrap()["spin"], "stop");
        assert_eq!(edit("dreh dich schneller").unwrap()["spin"], "faster");
        assert_eq!(edit("zoom in").unwrap()["view"], "zoom_in");
        assert_eq!(edit("show it from the front").unwrap()["view"], "front");
        assert_eq!(edit("side view").unwrap()["view"], "right");
        assert_eq!(edit("von oben").unwrap()["view"], "top");
        assert_eq!(edit("explode it").unwrap()["explode"], true);
        assert_eq!(edit("put it back together").unwrap()["explode"], false);
        assert_eq!(edit("make it bigger").unwrap()["scale"], "bigger");
        assert_eq!(edit("mach es kleiner").unwrap()["scale"], "smaller");
        assert_eq!(edit("reset").unwrap()["action"], "reset");
        assert_eq!(edit("measure it").unwrap()["action"], "measure");
        assert_eq!(edit("how tall is it").unwrap()["action"], "measure");
        let ex = edit("save it as STL for my 3D printer").unwrap();
        assert_eq!((ex["action"].as_str(), ex["format"].as_str()), (Some("export"), Some("stl")));
        let ex = edit("export it as OBJ").unwrap();
        assert_eq!(ex["format"], "obj");
        let ex = edit("save it as an stl 20 cm tall").unwrap();
        assert_eq!(ex["height_cm"], 20.0);
        assert_eq!(edit("speichere es als STL für meinen 3D-Drucker").unwrap()["format"], "stl");
        assert_eq!(edit("close the 3d model").unwrap()["action"], "close");
        // A name that holds a paint word is not paint: "Iron Man" is not iron.
        let named = edit("make the iron man suit gold").unwrap();
        assert_eq!((named["material"].as_str(), named["part"].as_str()), (Some("gold"), Some("iron man suit")));
        // (The model on show called by its own name is the whole model, not a part of it.)
        let titled = command_with("paint the silver falcon red", true, false, &["Silver Falcon".to_string()]).unwrap();
        assert_eq!((titled["material"].as_str(), titled["part"].as_str()), (Some("red"), None));
        // From Sir's log: the model on show by its own words, its colour, its own name.
        assert_eq!(edit("make the 3D model bigger").unwrap()["scale"], "bigger");
        assert_eq!(args_of("make the 3D model bigger", false), None, "not a new model called 'model bigger'");
        let c = edit("change the color to green").unwrap();
        assert_eq!((c["material"].as_str(), c.get("part")), (Some("green"), None));
        let whole = command_with("Change the Iron Man suit color to green.", true, false, &["iron man suit".to_string()]).unwrap();
        assert_eq!((whole["material"].as_str(), whole.get("part")), (Some("green"), None));
        // The HUD's own colours stay the HUD's; zoom is the Earth's while it is up.
        for t in ["make the theme red", "make the background blue", "set the accent to gold", "turn the volume up", "what time is it"] {
            assert_eq!(edit(t), None, "{t}");
        }
        // Other commands said while the studio is up stay theirs.
        for t in ["Turn it all the way up.", "Dreh auf.", "Right.", "Back", "normal mode", "set a red alert", "change the song to Red by Taylor Swift", "make the core blue", "Turn the music off.", "what is the red planet"] {
            assert_eq!(edit(t), None, "{t}");
        }
        assert_eq!(edit("von vorne").unwrap()["view"], "front");
        assert_eq!(edit("make it rotate").unwrap()["spin"], "start");
        assert_eq!(edit("give it a gold paint job").unwrap()["material"], "gold");
        assert_eq!(command("zoom in", true, true), None);
        assert!(command("zoom in on the model", true, true).is_some());
        // Closed: none of it.
        assert_eq!(args_of("make it gold", false), None);
        assert_eq!(args_of("spin faster", false), None);
    }

    #[test]
    fn heights_scales_and_lengths() {
        assert_eq!(height_cm(" 18 cm tall"), Some(18.0));
        assert_eq!(height_cm("150 mm"), Some(15.0));
        assert_eq!(height_cm("0,5 m hoch"), Some(50.0));
        assert_eq!(scale_ratio("at 1:10"), Some(0.1));
        assert_eq!(scale_ratio("im maßstab 1 zu 20"), Some(0.05));
        assert_eq!(length(0.18), "18 cm");
        assert_eq!(length(1.94), "1.9 m");
        assert_eq!(length(12_742_000.0), "12,742 km");
        assert_eq!(length(0.004), "4.0 mm");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_studio_opens_edits_measures_and_exports_without_the_network() {
        let _g = lock();
        let ctx_free = run_builtin_flow().await;
        assert!(ctx_free.contains("Spoken:"), "{ctx_free}");
    }

    /// The tool's flow without a `ToolContext` (only the library, edits, measure and export).
    async fn run_builtin_flow() -> String {
        let entry = scene3d::lookup("iron man suit").unwrap();
        let out = open_builtin(entry, &json!({})).await.unwrap();
        assert!(out.contains("Iron Man suit"), "{out}");
        let snap = snapshot();
        assert!(snap.open && snap.scene.is_some() && snap.phase == Phase::Ready);
        let scene = snap.scene.clone().unwrap();
        assert!(scene.part_names.iter().any(|n| n == "helmet"));
        // "Make the helmet blue": only the helmet changes.
        let reply_text = edit(&json!({ "material": "blue", "part": "helmet" })).unwrap();
        assert!(reply_text.contains("helmet"), "{reply_text}");
        let scene = snapshot().scene.clone().unwrap();
        let helmet = scene.model.parts.iter().position(|p| p.name == "helmet").unwrap();
        let chest = scene.model.parts.iter().position(|p| p.name == "chest").unwrap();
        let hm = &scene.materials[scene.part_material[helmet]];
        let cm = &scene.materials[scene.part_material[chest]];
        assert!(hm.base_color[2] > hm.base_color[0], "the helmet is blue: {:?}", hm.base_color);
        assert!(cm.base_color[0] > cm.base_color[2], "the chest stays red: {:?}", cm.base_color);
        // An unknown part is said, nothing breaks.
        let r = edit(&json!({ "material": "gold", "part": "tail" })).unwrap();
        assert!(r.contains("can't find a part called \"tail\""), "{r}");
        // "Paint it red and gold": the larger surface red, the rest gold.
        edit(&json!({ "material": "red and gold" })).unwrap();
        let scene = snapshot().scene.clone().unwrap();
        let golds = scene.part_material.iter().filter(|m| scene.materials[**m].metallic == 1.0 && scene.materials[**m].base_color[1] > 0.5).count();
        assert!(golds > 0 && golds < scene.model.parts.len(), "two tones: {golds} of {}", scene.model.parts.len());
        // A patterned finish brings its textures.
        edit(&json!({ "material": "carbon fibre" })).unwrap();
        let scene = snapshot().scene.clone().unwrap();
        let m = &scene.materials[scene.part_material[0]];
        assert!(m.triplanar && m.base_color_tex.is_some() && m.normal_tex.is_some());
        assert!(scene.textures.len() >= 3);
        edit(&json!({ "look": "hologram", "spin": "faster", "explode": true, "view": "front" })).unwrap();
        let snap = snapshot();
        assert_eq!(snap.view.look, Look::Hologram);
        assert_eq!(snap.view.explode, 1.0);
        assert!(take_camera().contains(&CameraCmd::Front));
        // Measure: a person-sized suit.
        let m = measure();
        assert!(m.contains(" m tall") && m.contains("At full size"), "{m}");
        // Export round trip: STL for the printer is scaled to 18 cm and says so.
        let out = export(&json!({ "format": "stl" })).await.unwrap();
        assert!(out.contains("about 18 cm tall at 1:"), "{out}");
        let dir = export_dir();
        let newest = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "stl")).max_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap()).unwrap();
        let back = mesh_io::load_file(&newest, &|_| {}).unwrap();
        assert!((back.size_m()[1] - 0.18).abs() < 0.002, "{:?}", back.size_m());
        let glb = export(&json!({ "format": "glb", "height_cm": 50 })).await.unwrap();
        assert!(glb.contains("50 cm tall"), "{glb}");
        assert_eq!(reset().contains("original"), true);
        let snap = snapshot();
        assert_eq!(snap.view.explode, 0.0);
        let parts_line = parts();
        assert!(parts_line.contains("helmet"), "{parts_line}");
        // Planets: no ground, a tilt, an atmosphere, the real size.
        let earth = scene3d::lookup("earth").unwrap();
        open_builtin(earth, &json!({ "material": "gold" })).await.unwrap();
        let snap = snapshot();
        let scene = snap.scene.clone().unwrap();
        assert!(scene.planet && scene.atmosphere.is_some() && scene.tilt_deg > 20.0);
        assert!(measure().contains("12,742 km"), "{}", measure());
        // The stand-in for anything else in a test run.
        let s = stand_in("millennium falcon", "Millennium Falcon", &json!({}));
        assert!(s.contains("stand-in"), "{s}");
        close();
        assert!(!is_open() && snapshot().scene.is_none());
        out
    }

    #[test]
    fn sketchfab_results_are_read_and_the_best_one_picked() {
        let v = json!({"results": [
            {"uid": "aaa", "name": "Iron Man Mark 85", "user": {"displayName": "Stark"}, "license": {"label": "CC Attribution"}, "likeCount": 900, "faceCount": 120000,
             "archives": {"glb": {"size": 220000000u64}}, "thumbnails": {"images": [{"width": 1920, "url": "big"}, {"width": 640, "url": "mid"}]}, "viewerUrl": "https://sketchfab.com/3d-models/x-aaa"},
            {"uid": "bbb", "name": "Iron man suit (game ready)", "user": {"username": "maker"}, "license": {"label": "CC Attribution"}, "likeCount": 300, "faceCount": 60000,
             "archives": {"glb": {"size": 18000000u64}}, "thumbnails": {"images": [{"width": 256, "url": "small"}]}},
            {"uid": "ccc", "name": "Banana", "likeCount": 99999, "archives": {"glb": {"size": 1000}}}
        ]});
        let hits = parse_hits(&v);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].thumb.as_deref(), Some("mid"));
        assert_eq!(hits[1].author, "maker");
        assert_eq!(hits[1].url, "https://sketchfab.com/3d-models/bbb");
        assert_eq!(pick(&hits, "iron man suit").unwrap().uid, "bbb", "the matching, lighter model wins");
        assert_eq!(ranked(&hits, "iron man suit")[0].0, 3, "three words of the request matched");
        // Both suits are worth fetching for the three words; the banana is not.
        let suits: Vec<String> = candidates(&hits, &wanted("iron man suit", &[])).into_iter().map(|h| h.uid).collect();
        assert_eq!(suits, ["bbb", "aaa"]);

        // The free mirror's index and its gzip (one stored deflate block).
        let index = r#"{"aaa": "glbs/000-001/aaa.glb", "bbb":"glbs/000-159/bbb.glb"}"#;
        assert_eq!((objaverse_path(index, "bbb"), objaverse_path(index, "ccc")), (Some("glbs/000-159/bbb.glb"), None));
        let mut gz = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 0xff];
        gz.extend(b"paths.json\0");
        gz.extend([1, index.len() as u8, 0, !(index.len() as u8), 0xff]);
        gz.extend(index.as_bytes());
        gz.extend([0, 0, 0, 0, index.len() as u8, 0, 0, 0]);
        assert_eq!(gunzip(&gz).unwrap(), index.as_bytes());
        assert_eq!(curated_url("nasa:3D Models/Saturn V/Saturn V.glb").unwrap().0, "https://raw.githubusercontent.com/nasa/NASA-3D-Resources/master/3D%20Models/Saturn%20V/Saturn%20V.glb");
        assert!(curated_url("khronos:DamagedHelmet").unwrap().0.ends_with("/DamagedHelmet/glTF-Binary/DamagedHelmet.glb"));
        // "Saturn V" is the rocket, "Saturn" the planet; "the Hubble telescope" is NASA's.
        assert_eq!(curated("saturn v").map(|(c, _)| c.title), Some("Saturn V"));
        let (lib, l) = library("saturn v").unwrap();
        assert_eq!(lib.key, "saturn");
        assert!(curated("saturn v").unwrap().1 > l);
        assert_eq!(curated("the hubble telescope").map(|(c, _)| c.title), Some("Hubble Space Telescope"));
        assert!(curated("a millennium falcon").is_none());
    }

    /// 2026-11-09: "Can you show me a 3D model of an Apple?" brought up an iPad Pro, and three
    /// more tries ("a real apple", "an eatable apple") did no better. The names are the
    /// catalogue's own answer to "apple" that week.
    #[test]
    fn an_apple_is_the_fruit_not_the_tablet() {
        let named = |list: &[(&str, u64)]| -> Vec<Hit> {
            list.iter()
                .enumerate()
                .map(|(i, (name, likes))| Hit { uid: format!("u{i}"), name: name.to_string(), author: String::new(), license: String::new(), likes: *likes, faces: 20_000, glb_size: Some(2_000_000), thumb: None, url: String::new() })
                .collect()
        };
        let catalogue = named(&[
            ("Apple Macintosh", 204), ("Apple Watch Ultra 2", 344), ("Apple", 317), ("Apple II Computer", 750), ("Apple Watch", 144), ("Red Apple", 172), ("Royal gala apples", 347),
            ("Cute Apple (Free)", 290), ("Apples", 114), ("Bronze Moses at Augustana University", 627), ("Apple iPad Pro", 323), ("Apple Desktop", 88), ("Apple iPhone 17 Pro Max", 81),
            ("Apple Box", 281), ("Apple Vision Pro", 162), ("Tasty Green Apple", 283), ("Basket of apples", 900), ("Pineapple", 5000), ("Apple Tree", 800),
        ]);
        for asked in ["apple", "an apple", "real apple", "eatable apple", "apple fruit", "the apple you can eat"] {
            let want = wanted(asked, &[]);
            assert_eq!((want.core.as_str(), want.search.as_str()), ("apple", "apple fruit"), "{asked}");
            let names: Vec<String> = candidates(&catalogue, &want).into_iter().map(|h| h.name).collect();
            assert_eq!(names[0], "Apple", "{asked}: {names:?}");
            for wrong in ["Apple iPad Pro", "Apple Watch", "Apple II Computer", "Apple Box", "Apple Tree", "Basket of apples", "Pineapple", "Apple Vision Pro", "Bronze Moses at Augustana University"] {
                assert!(!names.iter().any(|n| n == wrong), "{asked}: {wrong} is no apple ({names:?})");
            }
            for right in ["Red Apple", "Cute Apple (Free)", "Apples", "Tasty Green Apple", "Royal gala apples"] {
                assert!(names.iter().any(|n| n == right), "{asked}: {right} is one ({names:?})");
            }
        }
        // The product by its name is still the product.
        let tablet = wanted("apple ipad pro", &[]);
        assert_eq!(tablet.search, "apple ipad pro");
        assert_eq!(candidates(&catalogue, &tablet)[0].name, "Apple iPad Pro");
        assert_eq!(candidates(&catalogue, &wanted("apple watch", &[]))[0].name, "Apple Watch");
        // Only products on offer: the nearest name still comes, never nothing.
        let products = named(&[("Apple iPad Pro", 323), ("Apple Watch", 144)]);
        assert_eq!(candidates(&products, &wanted("apple", &[])).len(), 2);
        // "Not an iPad": whatever else, not that.
        let no_tablet = wanted("real apple", &["an iPad".to_string()]);
        assert_eq!(no_tablet.avoid, ["ipad"]);
        assert_eq!(candidates(&products, &no_tablet).iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Apple Watch"]);
        // Other senses and plain things.
        assert_eq!(wanted("a real horse", &[]).search, "horse");
        assert_eq!(wanted("jaguar", &[]).search, "jaguar animal");
        assert_eq!(wanted("jaguar e type", &[]).search, "jaguar e type");
        assert_eq!(wanted("iron man suit", &[]), Wanted { core: "iron man suit".into(), search: "iron man suit".into(), avoid: vec![] });
        assert_eq!(wanted("edible mushroom", &[]).search, "mushroom food");
        assert!(sense_only("the fruit") && sense_only("a real one") && !sense_only("an apple") && !sense_only("fruit bowl"));
        // His own sentences, with a model on show.
        let said = |text: &str| command_with(text, true, false, &["apple ipad pro".to_string()]);
        assert_eq!(said("No, not an iPad, I mean a real Apple."), Some(json!({"query": "real apple", "avoid": ["ipad"]})));
        assert_eq!(said("Can you show me this 3D render of an apple?"), Some(json!({"query": "apple"})));
        assert_eq!(said("Can you show me the model of an eatable apple?"), Some(json!({"query": "eatable apple"})));
        assert_eq!(said("I meant the fruit"), Some(json!({"query": "fruit"})));
        assert_eq!(command_with("No, not an iPad, I mean a real Apple.", false, false, &[]), None, "nothing on show: not the studio's");
    }

    #[test]
    fn the_cache_keeps_its_bound_by_dropping_the_oldest() {

        let dir = std::env::temp_dir().join(format!("jai_cache_bound_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, name) in ["old.glb", "mid.glb", "new.glb"].iter().enumerate() {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; 1000]).unwrap();
            let f = std::fs::File::options().append(true).open(&p).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + i as u64 * 1000)).unwrap();
        }
        evict(&dir, 2100);
        let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(!left.contains(&"old.glb".to_string()) && left.len() == 2, "{left:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_streamed_composition_shows_parts_as_they_arrive() {
        // What the language model streams, cut after the second part.
        let text = r##"{"name":"Lamp","materials":{"brass":{"color":"#c9a74a","metallic":1,"roughness":0.3}},"parts":[{"name":"base","shape":"cylinder","size":[0.3,0.05,0.3],"position":[0,0.025,0],"material":"brass"},{"name":"stem","shape":"cylinder","size":[0.03,0.5,0.03],"position":[0,0.3,0],"material":"brass"},{"name":"sha"##;
        let doc = scene3d::parse_scene(text).unwrap();
        assert_eq!(doc.parts.len(), 2);
        let m = scene3d::build(&doc);
        assert!((m.size_m()[1] - 0.55).abs() < 0.01, "{:?}", m.size_m());
        // The materials survive the cut.
        assert!(m.materials[m.parts[0].material].metallic > 0.9);
    }

    #[test]
    fn paint_covers_a_photo_models_colours() {
        // A model rebuilt from a photo wears its vertex colours until Sir paints it.
        let field: Vec<f32> = (0..512).map(|i| 2.5 - (((i / 64) as f32 - 3.5).powi(2) + ((i / 8 % 8) as f32 - 3.5).powi(2) + ((i % 8) as f32 - 3.5).powi(2)).sqrt()).collect();
        let (verts, tris) = crate::photo3d::marching_cubes(&field, 8);
        let model = crate::photo3d::model_of("Vase", &verts, tris, &vec![[0.2, 0.6, 0.9]; verts.len()]);
        let glow = Edit { target: Target::All, change: Change::Glow(None) };
        assert!(effective(&model, &[glow.clone()]).0.last().unwrap().vertex_colors);
        let red = Edit { target: Target::All, change: Change::Paint(Paint::Colour("red", [0.8, 0.0, 0.0])) };
        let (materials, _, part_material) = effective(&model, &[glow, red]);
        assert!(!materials[part_material[0]].vertex_colors);
        // A saved GLB keeps the colours (glTF's COLOR_0) while they show.
        let has_colours = |m: &Model| String::from_utf8_lossy(&mesh_io::glb_bytes(m, 1.0).unwrap()).contains("COLOR_0");
        assert!(has_colours(&model));
        let mut painted = model.clone();
        painted.materials = materials;
        painted.parts[0].material = part_material[0];
        assert!(!has_colours(&painted));
    }
}
