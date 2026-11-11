//! Pictures on the HUD (`show_image`): anything Sir names from the open web, the Earth as NASA's
//! DSCOVR satellite sees it, and his own screenshots and photos.
//!
//! * **Sources, all keyless.** Openverse (`mature=false`), Wikimedia Commons, the NASA Image and
//!   Video Library, NASA EPIC (the whole sunlit Earth, a dozen frames a day) and his own files:
//!   a path, the newest screenshot, the desktop, Pictures and Downloads, a file name.
//! * **Speed.** The block opens at once ("searching"), the thumbnails come first and the full
//!   picture follows; decoding never runs on the UI thread; fetched bytes stay in a bounded disk
//!   cache (`config/image_cache`), decoded pixels in a bounded memory cache (`pixels`).
//! * **Safe.** Openverse filters mature content; every query and every result's title also goes
//!   through a word filter (English and German).
//! * **The block** (`gui_media.rs`): fit, zoom and pan, a thumbnail strip, next and previous,
//!   title, source and licence; "detach it" gives it a window of its own; "save this picture"
//!   puts the full picture into `Pictures\JAI`.
//!
//! `pixels` is also the frame store of the live cameras (`webcams`) and street view
//! (`street_view`). Tests and dry runs never touch the network or Sir's folders: every source
//! has a stand-in (`offline`).
//!
//! The voice fast path for all three features lives here too (`voice_intent`).

use crate::intents::Intent;
use crate::tools::{str_arg, ToolContext};
use eframe::egui::ColorImage;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, info};

pub const TOOL: &str = "show_image";
const USER_AGENT: &str = "JAI/3 desktop assistant (picture viewer)";
const CACHE_DIR: &str = "config/image_cache";
const CACHE_LIMIT: u64 = 200 * 1024 * 1024;
const CACHE_TRIM_TO: u64 = 160 * 1024 * 1024;
/// The most bytes one download may have.
const MAX_DOWNLOAD: usize = 40 * 1024 * 1024;
/// Pictures one search shows at most.
const MAX_PICTURES: usize = 24;
/// Local pictures one request shows at most.
const MAX_LOCAL: usize = 60;
/// The thumbnail strip's pictures and the full picture are decoded to at most this many pixels a side.
pub const THUMB_SIDE: u32 = 360;
pub const FULL_SIDE: u32 = 2560;
const SOURCE_TIMEOUT: Duration = Duration::from_secs(7);
const IMAGE_EXT: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tests, the self-tester and dry runs: stand-ins instead of the network and Sir's folders.
pub(crate) fn offline() -> bool {
    cfg!(test) || crate::live_self_tester::running() || crate::live_self_tester::dry()
}

// ------------------------------------------------------------------------------- network ----

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::limited(6))
        .build()
        .unwrap_or_default()
});

pub(crate) fn http() -> &'static reqwest::Client {
    &HTTP
}

/// A JSON document (its URL never reaches an error text: tokens travel in headers anyway).
pub(crate) async fn get_json(url: &str, timeout: Duration) -> Result<Value, String> {
    let resp = HTTP.get(url).timeout(timeout).header("Accept", "application/json").send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.json::<Value>().await.map_err(|e| e.without_url().to_string())
}

pub(crate) async fn get_text(url: &str, timeout: Duration) -> Result<String, String> {
    let resp = HTTP.get(url).timeout(timeout).send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.text().await.map_err(|e| e.without_url().to_string())
}

/// What a picture download brought.
pub(crate) enum Fetched {
    /// The frame has not changed since the validator (`If-Modified-Since`).
    NotModified,
    Bytes { bytes: Vec<u8>, modified: Option<SystemTime>, validator: Option<String> },
}

static FETCH_SLOTS: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(6));

/// A picture's bytes: from the disk cache when allowed, else downloaded (and cached).
pub(crate) async fn fetch(url: &str, disk_cache: bool, validator: Option<&str>) -> Result<Fetched, String> {
    let cached = disk_cache.then(|| cache_file(url));
    if let Some(path) = &cached {
        if let Ok(bytes) = tokio::fs::read(path).await {
            return Ok(Fetched::Bytes { bytes, modified: None, validator: None });
        }
    }
    let _slot = FETCH_SLOTS.acquire().await.map_err(|e| e.to_string())?;
    let mut req = HTTP.get(url).header("Accept", "image/webp,image/png,image/jpeg,image/*;q=0.8,*/*;q=0.5");
    if let Some(v) = validator.filter(|v| !v.is_empty()) {
        req = req.header("If-Modified-Since", v);
    }
    let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
    let status = resp.status().as_u16();
    if status == 304 {
        return Ok(Fetched::NotModified);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {status}"));
    }
    if resp.content_length().is_some_and(|n| n as usize > MAX_DOWNLOAD) {
        return Err("the picture is larger than 40 MB".into());
    }
    let last_modified = resp.headers().get("last-modified").and_then(|v| v.to_str().ok()).map(str::to_string);
    let modified = last_modified.as_deref().and_then(http_date);
    let bytes = resp.bytes().await.map_err(|e| e.without_url().to_string())?.to_vec();
    if bytes.len() > MAX_DOWNLOAD {
        return Err("the picture is larger than 40 MB".into());
    }
    if let Some(path) = cached {
        cache_write(path, bytes.clone()).await;
    }
    Ok(Fetched::Bytes { bytes, modified, validator: last_modified })
}

/// "Sun, 08 Nov 2026 23:12:35 GMT".
pub(crate) fn http_date(s: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc2822(s.trim()).ok().map(SystemTime::from)
}

fn stable_hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn cache_file(url: &str) -> PathBuf {
    Path::new(CACHE_DIR).join(format!("{:016x}.img", stable_hash(url)))
}

static CACHE_WRITES: AtomicUsize = AtomicUsize::new(0);

async fn cache_write(path: PathBuf, bytes: Vec<u8>) {
    if let Some(dir) = path.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    if let Err(e) = tokio::fs::write(&path, bytes).await {
        debug!("[IMAGES] Could not cache a picture: {e}");
    }
    // The cache is measured on the first write of a session and every 40 writes after it.
    if CACHE_WRITES.fetch_add(1, Ordering::AcqRel) % 40 == 0 {
        let _ = tokio::task::spawn_blocking(|| crate::earth::trim_cache(Path::new(CACHE_DIR), CACHE_LIMIT, CACHE_TRIM_TO)).await;
    }
}

/// A picture the way up its camera meant (phones store portraits sideways, with an EXIF note).
pub(crate) fn load_upright(bytes: &[u8]) -> Option<image::DynamicImage> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(img)
}

/// Bytes → pixels, at most `max_side` a side (JPEG, PNG, WebP).
pub(crate) fn decode(bytes: &[u8], max_side: u32) -> Option<ColorImage> {
    let img = load_upright(bytes)?;
    let img = if img.width().max(img.height()) > max_side {
        if max_side <= 512 {
            img.thumbnail(max_side, max_side)
        } else {
            img.resize(max_side, max_side, image::imageops::FilterType::Triangle)
        }
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

async fn decode_off_thread(bytes: Vec<u8>, max_side: u32) -> Result<ColorImage, String> {
    tokio::task::spawn_blocking(move || decode(&bytes, max_side)).await.map_err(|e| e.to_string())?.ok_or_else(|| "not a picture I can read (JPEG, PNG or WebP)".to_string())
}

/// A stand-in picture for tests and dry runs: sky, horizon and ground in a hue from `seed`, with
/// eight meridian lines (it doubles as a panorama: twice as wide as high).
pub fn demo_image(seed: u64, side: u32) -> ColorImage {
    let w = side.clamp(16, 512) as usize;
    let h = (w / 2).max(8);
    let hue = (seed % 360) as f32;
    let sky = crate::theme_engine::from_hsv(hue, 0.55, 0.85);
    let ground = crate::theme_engine::from_hsv(hue + 140.0, 0.45, 0.35);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let t = y as f32 / h as f32;
            let base = if t < 0.5 { sky } else { ground };
            let shade = if t < 0.5 { 0.6 + 0.4 * (t * 2.0) } else { 1.0 - 0.5 * (t - 0.5) };
            let line = x % (w / 8).max(1) == 0 || y == h / 2;
            let c = if line { [240, 248, 255] } else { base.map(|v| (v as f32 * shade).clamp(0.0, 255.0) as u8) };
            pixels.push(eframe::egui::Color32::from_rgb(c[0], c[1], c[2]));
        }
    }
    ColorImage { size: [w, h], pixels }
}

// ------------------------------------------------------------------------ the pixel store ----

/// Decoded pictures shared by the picture viewer, the cameras and street view, by key. Loads run
/// on the runtime (decoding on its blocking pool); the GUI turns frames into textures.
pub mod pixels {
    use super::*;

    /// Where a picture comes from.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Origin {
        /// Addresses tried in order until one gives a picture.
        Web(Vec<String>),
        File(PathBuf),
    }

    #[derive(Clone)]
    pub struct Frame {
        pub image: Arc<ColorImage>,
        /// Changes with every new frame (the GUI's texture follows it).
        pub version: u64,
        /// The source's own time for the frame (`Last-Modified`, a file's time).
        pub modified: Option<SystemTime>,
        /// When it was last fetched or confirmed unchanged.
        pub fetched: SystemTime,
    }

    struct Slot {
        frame: Option<Frame>,
        loading: bool,
        error: Option<String>,
        failed_at: Option<Instant>,
        used: Instant,
        validator: Option<String>,
    }

    static SLOTS: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(Default::default);
    static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    static VERSION: AtomicU64 = AtomicU64::new(1);
    const MAX_SLOTS: usize = 160;
    const MAX_BYTES: usize = 420 * 1024 * 1024;
    /// A failed load is tried again after this long.
    const RETRY: Duration = Duration::from_secs(30);

    /// The runtime loads run on when they are asked for from the GUI thread.
    pub fn set_runtime(rt: &tokio::runtime::Handle) {
        let _ = RUNTIME.set(rt.clone());
    }

    fn runtime() -> Option<tokio::runtime::Handle> {
        tokio::runtime::Handle::try_current().ok().or_else(|| RUNTIME.get().cloned())
    }

    fn slot() -> Slot {
        Slot { frame: None, loading: true, error: None, failed_at: None, used: Instant::now(), validator: None }
    }

    /// Picture `key` is wanted: loaded once (cheap to call every frame).
    pub fn want(key: &str, origin: &Origin, max_side: u32, disk_cache: bool) {
        {
            let mut slots = lock(&SLOTS);
            match slots.get_mut(key) {
                Some(s) => {
                    s.used = Instant::now();
                    if s.frame.is_some() || s.loading || s.failed_at.is_some_and(|t| t.elapsed() < RETRY) {
                        return;
                    }
                    s.loading = true;
                }
                None => {
                    slots.insert(key.to_string(), slot());
                    evict(&mut slots);
                }
            }
        }
        start(key, origin.clone(), max_side, disk_cache, None);
    }

    /// The next frame of a camera: fetched again with `If-Modified-Since` (an unchanged frame
    /// costs a 304 and no decoding).
    pub fn reload(key: &str, origin: &Origin, max_side: u32) {
        let validator = {
            let mut slots = lock(&SLOTS);
            match slots.get_mut(key) {
                // (A camera that failed is asked again after a pause, not every frame.)
                Some(s) if s.loading || s.failed_at.is_some_and(|t| t.elapsed() < RETRY) => return,
                Some(s) => {
                    s.loading = true;
                    s.used = Instant::now();
                    s.validator.clone()
                }
                None => {
                    slots.insert(key.to_string(), slot());
                    None
                }
            }
        };
        start(key, origin.clone(), max_side, false, validator);
    }

    fn start(key: &str, origin: Origin, max_side: u32, disk_cache: bool, validator: Option<String>) {
        if offline() {
            // Tests and dry runs: a stand-in, at once.
            finish(key, Ok(Loaded::Image(demo_image(stable_hash(key), max_side.min(256)), None, None)));
            return;
        }
        let Some(rt) = runtime() else {
            finish(key, Err("no runtime to load pictures on".into()));
            return;
        };
        let key = key.to_string();
        rt.spawn(async move {
            let out = load(&origin, max_side, disk_cache, validator.as_deref()).await;
            if let Err(e) = &out {
                debug!("[IMAGES] {key}: {e}");
            }
            finish(&key, out);
            crate::gui::refresh();
        });
    }

    enum Loaded {
        Image(ColorImage, Option<SystemTime>, Option<String>),
        Unchanged,
    }

    async fn load(origin: &Origin, max_side: u32, disk_cache: bool, validator: Option<&str>) -> Result<Loaded, String> {
        match origin {
            Origin::File(path) => {
                let meta = tokio::fs::metadata(path).await.map_err(|e| e.to_string())?;
                if meta.len() > 80 * 1024 * 1024 {
                    return Err("the file is larger than 80 MB".into());
                }
                let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
                Ok(Loaded::Image(decode_off_thread(bytes, max_side).await?, meta.modified().ok(), None))
            }
            Origin::Web(urls) => {
                let mut last = "no address to load".to_string();
                for url in urls.iter().filter(|u| !u.is_empty()) {
                    match fetch(url, disk_cache, validator).await {
                        Ok(Fetched::NotModified) => return Ok(Loaded::Unchanged),
                        Ok(Fetched::Bytes { bytes, modified, validator }) => match decode_off_thread(bytes, max_side).await {
                            Ok(img) => return Ok(Loaded::Image(img, modified, validator)),
                            Err(e) => last = e,
                        },
                        Err(e) => last = e,
                    }
                }
                Err(last)
            }
        }
    }

    fn finish(key: &str, out: Result<Loaded, String>) {
        let mut slots = lock(&SLOTS);
        let Some(s) = slots.get_mut(key) else { return };
        s.loading = false;
        match out {
            Ok(Loaded::Image(image, modified, validator)) => {
                s.frame = Some(Frame { image: Arc::new(image), version: VERSION.fetch_add(1, Ordering::AcqRel), modified, fetched: SystemTime::now() });
                s.validator = validator;
                s.error = None;
                s.failed_at = None;
            }
            Ok(Loaded::Unchanged) => {
                if let Some(f) = &mut s.frame {
                    f.fetched = SystemTime::now();
                }
            }
            Err(e) => {
                s.error = Some(e);
                s.failed_at = Some(Instant::now());
            }
        }
    }

    /// The least recently used pictures go once there are too many or they take too much memory.
    fn evict(slots: &mut HashMap<String, Slot>) {
        let bytes = |s: &Slot| s.frame.as_ref().map_or(0, |f| f.image.size[0] * f.image.size[1] * 4);
        let mut total: usize = slots.values().map(bytes).sum();
        if slots.len() <= MAX_SLOTS && total <= MAX_BYTES {
            return;
        }
        let mut order: Vec<(Instant, String)> = slots.iter().filter(|(_, s)| !s.loading).map(|(k, s)| (s.used, k.clone())).collect();
        order.sort();
        for (_, key) in order {
            if slots.len() <= MAX_SLOTS * 3 / 4 && total <= MAX_BYTES * 3 / 4 {
                break;
            }
            if let Some(s) = slots.remove(&key) {
                total -= bytes(&s);
            }
        }
    }

    /// The picture under `key`, if it is loaded.
    pub fn get(key: &str) -> Option<Frame> {
        let mut slots = lock(&SLOTS);
        let s = slots.get_mut(key)?;
        s.used = Instant::now();
        s.frame.clone()
    }

    /// (loading, the last error) of `key`.
    pub fn status(key: &str) -> Option<(bool, Option<String>)> {
        lock(&SLOTS).get(key).map(|s| (s.loading, s.error.clone()))
    }
}

// ------------------------------------------------------------------------------ pictures ----

/// One picture of an album.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picture {
    pub title: String,
    pub thumb: String,
    pub full: String,
    /// Addresses tried after `full` (a smaller rendition, the original).
    pub alt: Vec<String>,
    /// "Openverse (Flickr)", "Wikimedia Commons", "NASA Image Library", "NASA EPIC", "Your files".
    pub source: String,
    pub creator: String,
    pub license: String,
    /// The page the picture comes from.
    pub page: String,
    pub local: Option<PathBuf>,
    /// When it was taken or saved, as text.
    pub when: String,
}

impl Picture {
    fn id(&self) -> String {
        match &self.local {
            Some(p) => p.display().to_string(),
            None => self.full.clone(),
        }
    }

    pub fn thumb_key(&self) -> String {
        format!("img:t:{}", self.id())
    }

    pub fn full_key(&self) -> String {
        format!("img:f:{}", self.id())
    }

    pub fn thumb_origin(&self) -> pixels::Origin {
        match &self.local {
            Some(p) => pixels::Origin::File(p.clone()),
            None => pixels::Origin::Web(vec![self.thumb.clone(), self.full.clone()]),
        }
    }

    pub fn full_origin(&self) -> pixels::Origin {
        match &self.local {
            Some(p) => pixels::Origin::File(p.clone()),
            None => pixels::Origin::Web(std::iter::once(self.full.clone()).chain(self.alt.iter().cloned()).chain(std::iter::once(self.thumb.clone())).collect()),
        }
    }

    /// "Photo: Jane Doe · CC BY 2.0 · Openverse (Flickr)".
    pub fn credit(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.creator.is_empty() {
            parts.push(format!("{}: {}", if self.local.is_some() { "File" } else { "By" }, self.creator));
        }
        if !self.license.is_empty() {
            parts.push(self.license.clone());
        }
        if !self.source.is_empty() {
            parts.push(self.source.clone());
        }
        if !self.when.is_empty() {
            parts.push(self.when.clone());
        }
        parts.join(" · ")
    }
}

/// What the viewer shows.
#[derive(Debug, Clone, Default)]
pub struct Album {
    /// What Sir asked for, in words ("Tokyo", "your latest screenshot").
    pub label: String,
    pub items: Vec<Picture>,
    pub index: usize,
    /// Changes whenever the album does.
    pub version: u64,
    /// A line under the picture ("Searching Openverse and Wikimedia Commons…", "Saved to …").
    pub note: String,
    pub busy: bool,
}

impl Album {
    pub fn current(&self) -> Option<&Picture> {
        self.items.get(self.index)
    }
}

/// Every picture block, oldest first, by block id ("image_viewer", "image_viewer_2" …): a new
/// subject opens a block of its own beside the others (2026-11-09: the Statue of Liberty
/// replaced the Eiffel Tower). The last is the one "next picture" and "save it" mean.
static ALBUMS: LazyLock<Mutex<Vec<(String, Album)>>> = LazyLock::new(Default::default);
static ALBUM_VERSION: AtomicU64 = AtomicU64::new(1);
/// At most this many picture blocks; the oldest makes way.
const MAX_BLOCKS: usize = 6;
/// "Spread them", "four at once": this many blocks when no number is said.
const SPREAD: usize = 4;

pub fn is_open() -> bool {
    !lock(&ALBUMS).is_empty()
}

/// Every picture block closes.
pub fn close() {
    lock(&ALBUMS).clear();
    crate::gui::refresh();
}

/// Picture block `id` closes.
pub fn close_block(id: &str) {
    lock(&ALBUMS).retain(|(b, _)| b != id);
    crate::gui::refresh();
}

/// The picture blocks' ids, oldest first.
pub fn block_ids() -> Vec<String> {
    lock(&ALBUMS).iter().map(|(id, _)| id.clone()).collect()
}

/// Block `id`'s album (None: the latest block's).
pub fn album_of(id: Option<&str>) -> Album {
    let albums = lock(&ALBUMS);
    id.map_or_else(|| albums.last(), |id| albums.iter().find(|(b, _)| b == id)).map(|(_, a)| a.clone()).unwrap_or_default()
}

pub fn album() -> Album {
    album_of(None)
}

fn update_in(id: Option<&str>, f: impl FnOnce(&mut Album)) {
    let mut albums = lock(&ALBUMS);
    let slot = match id {
        Some(id) => albums.iter_mut().find(|(b, _)| b == id),
        None => albums.last_mut(),
    };
    if let Some((_, album)) = slot {
        f(album);
        album.version = ALBUM_VERSION.fetch_add(1, Ordering::AcqRel);
    }
    drop(albums);
    crate::gui::refresh();
}

/// A block for `album`: the latest one again when it shows the same subject (and `fresh` is
/// false), else a new one beside the others. Its id.
fn open_block(mut album: Album, fresh: bool) -> String {
    album.version = ALBUM_VERSION.fetch_add(1, Ordering::AcqRel);
    let mut albums = lock(&ALBUMS);
    let id = match albums.last_mut().filter(|(_, a)| !fresh && a.label.eq_ignore_ascii_case(&album.label)) {
        Some((id, a)) => {
            *a = album;
            id.clone()
        }
        None => {
            if albums.len() >= MAX_BLOCKS {
                let (oldest, _) = albums.remove(0);
                crate::hud_dock::forget(&oldest);
            }
            let id = (1..).map(|n| if n == 1 { "image_viewer".to_string() } else { format!("image_viewer_{n}") }).find(|id| albums.iter().all(|(b, _)| b != id)).unwrap_or_default();
            albums.push((id.clone(), album));
            id
        }
    };
    drop(albums);
    // Asked for again while it was fading out: it stays.
    crate::hud_dock::cancel_close(&id);
    crate::gui::show();
    crate::gui::refresh();
    id
}

/// "Spread them", "open each picture in its own window": pictures 2 … `n` of the latest block
/// each in a block of their own beside it; how many blocks show the subject now.
fn spread(n: usize) -> usize {
    let album = album();
    let n = n.clamp(2, MAX_BLOCKS).min(album.items.len());
    for i in 1..n {
        let index = (album.index + i) % album.items.len();
        prefetch(&album.items, index);
        open_block(Album { index, ..album.clone() }, true);
    }
    n.max(1)
}

/// The strip's click.
pub fn select(id: &str, index: usize) {
    update_in(Some(id), |a| {
        if index < a.items.len() {
            a.index = index;
        }
    });
}

/// Next (`+1`) or previous (`-1`), round block `id`'s album (None: the latest block).
pub fn step(id: Option<&str>, by: isize) -> Option<Picture> {
    let mut out = None;
    update_in(id, |a| {
        let n = a.items.len() as isize;
        if n > 0 {
            a.index = (a.index as isize + by).rem_euclid(n) as usize;
            out = a.current().cloned();
        }
    });
    out
}

pub fn set_note(id: Option<&str>, text: &str) {
    update_in(id, |a| a.note = text.to_string());
}

// ---------------------------------------------------------------------- what is searched ----

/// "The Earth", "die Erde", "planet earth", "our planet".
pub fn is_the_earth(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    let q = q.strip_prefix("the ").or_else(|| q.strip_prefix("die ")).or_else(|| q.strip_prefix("our ")).unwrap_or(&q).trim();
    matches!(q, "earth" | "planet earth" | "erde" | "planet" | "planet erde" | "earth from space" | "erde aus dem all" | "blue marble" | "whole earth" | "globe")
}

/// Space subjects NASA's library knows best.
fn is_space(query: &str) -> bool {
    const WORDS: &[&str] = &[
        "moon", "mond", "mars", "jupiter", "saturn", "venus", "mercury", "merkur", "neptune", "neptun", "uranus", "pluto", "galaxy", "galaxie", "nebula", "nebel", "milky way",
        "milchstraße", "black hole", "schwarzes loch", "iss", "space station", "raumstation", "astronaut", "apollo", "artemis", "rocket", "rakete", "hubble", "webb", "comet", "komet",
        "asteroid", "nasa", "solar system", "sonnensystem", "supernova", "space shuttle", "spacewalk", "lunar", "planet", "orion", "eclipse", "finsternis",
    ];
    let q = query.to_lowercase();
    WORDS.iter().any(|w| q.split(|c: char| !c.is_alphanumeric() && c != ' ').any(|part| part.contains(w)))
}

/// Words that keep a query or a result off Sir's HUD (English and German).
const UNSAFE: &[&str] = &[
    "nude", "nudes", "naked", "nsfw", "porn", "porno", "pornography", "sex", "sexy", "erotic", "erotica", "explicit", "xxx", "hentai", "fetish", "topless", "nackt", "nacktbild",
    "erotik", "busen", "genitals", "penis", "vagina", "gore", "beheading", "corpse", "leiche", "dead body", "suicide", "selbstmord", "lingerie",
];

pub fn is_unsafe(text: &str) -> bool {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    UNSAFE.iter().any(|bad| if bad.contains(' ') { t.contains(bad) } else { words.contains(bad) })
}

fn strip_html(s: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").expect("tag regex"));
    let text = TAG.replace_all(s, "");
    html_unescape(text.trim())
}

pub(crate) fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&#039;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ")
}

fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------------------- sources ----

fn openverse_url(query: &str, n: usize) -> String {
    format!("https://api.openverse.org/v1/images/?q={}&mature=false&page_size={n}", urlencoding::encode(query))
}

/// Openverse's results ("CC BY 2.0", the creator, the provider).
pub fn parse_openverse(v: &Value) -> Vec<Picture> {
    let Some(results) = v["results"].as_array() else { return Vec::new() };
    results
        .iter()
        .filter(|r| r["mature"].as_bool() != Some(true))
        .filter_map(|r| {
            let full = r["url"].as_str()?.to_string();
            let thumb = r["thumbnail"].as_str().unwrap_or(&full).to_string();
            let license = match (r["license"].as_str().unwrap_or("").to_lowercase().as_str(), r["license_version"].as_str().unwrap_or("")) {
                ("cc0", _) => "CC0".to_string(),
                ("pdm", _) => "Public domain".to_string(),
                ("", _) => String::new(),
                (l, v) => format!("CC {} {v}", l.to_uppercase()).trim().to_string(),
            };
            let provider = r["provider"].as_str().or(r["source"].as_str()).unwrap_or("");
            Some(Picture {
                title: r["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).unwrap_or("Untitled").to_string(),
                thumb,
                full,
                alt: Vec::new(),
                source: if provider.is_empty() { "Openverse".into() } else { format!("Openverse ({})", title_case(provider)) },
                creator: r["creator"].as_str().unwrap_or("").to_string(),
                license,
                page: r["foreign_landing_url"].as_str().unwrap_or("").to_string(),
                local: None,
                when: String::new(),
            })
        })
        .collect()
}

fn wikimedia_url(query: &str, n: usize) -> String {
    format!(
        "https://commons.wikimedia.org/w/api.php?action=query&format=json&generator=search&gsrsearch={}&gsrnamespace=6&gsrlimit={n}&prop=imageinfo&iiprop=url|mime|size|extmetadata&iiurlwidth=400&iiextmetadatafilter=LicenseShortName|Artist|ObjectName",
        urlencoding::encode(&format!("{query} filetype:bitmap"))
    )
}

/// Wikimedia Commons' search (JPEG, PNG and WebP only; a 1920 px rendition of large originals).
pub fn parse_wikimedia(v: &Value) -> Vec<Picture> {
    let Some(pages) = v["query"]["pages"].as_object() else { return Vec::new() };
    let mut found: Vec<(i64, Picture)> = pages
        .values()
        .filter_map(|page| {
            let info = page["imageinfo"].as_array()?.first()?;
            let mime = info["mime"].as_str().unwrap_or("");
            if !matches!(mime, "image/jpeg" | "image/png" | "image/webp") {
                return None;
            }
            let original = info["url"].as_str()?.to_string();
            let thumb = info["thumburl"].as_str().unwrap_or(&original).to_string();
            let width = info["width"].as_u64().unwrap_or(0);
            // (A rendition wider than the original is an error, so only large ones get one.)
            let full = if width > 1920 && thumb.contains("/400px-") { thumb.replace("/400px-", "/1920px-") } else { original.clone() };
            let meta = &info["extmetadata"];
            let title = meta["ObjectName"]["value"].as_str().map(strip_html).filter(|t| !t.is_empty()).unwrap_or_else(|| {
                let raw = page["title"].as_str().unwrap_or("").trim_start_matches("File:");
                raw.rsplit_once('.').map_or(raw, |(stem, _)| stem).replace('_', " ")
            });
            Some((
                page["index"].as_i64().unwrap_or(i64::MAX),
                Picture {
                    title,
                    thumb,
                    full: full.clone(),
                    alt: if full != original { vec![original] } else { Vec::new() },
                    source: "Wikimedia Commons".into(),
                    creator: meta["Artist"]["value"].as_str().map(strip_html).unwrap_or_default().chars().take(80).collect(),
                    license: meta["LicenseShortName"]["value"].as_str().map(strip_html).unwrap_or_default(),
                    page: info["descriptionurl"].as_str().unwrap_or("").to_string(),
                    local: None,
                    when: String::new(),
                },
            ))
        })
        .collect();
    found.sort_by_key(|(index, _)| *index);
    found.into_iter().map(|(_, p)| p).collect()
}

fn nasa_url(query: &str, n: usize) -> String {
    format!("https://images-api.nasa.gov/search?q={}&media_type=image&page_size={n}", urlencoding::encode(query))
}

/// The NASA Image and Video Library (public domain): its preview, and the largest rendition up
/// to about 2600 px (else the original).
pub fn parse_nasa(v: &Value) -> Vec<Picture> {
    let Some(items) = v["collection"]["items"].as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let data = item["data"].as_array()?.first()?;
            let links: Vec<&Value> = item["links"].as_array()?.iter().filter(|l| l["render"] == "image").collect();
            let thumb = links.iter().find(|l| l["rel"] == "preview").or(links.first())?["href"].as_str()?.to_string();
            let width = |l: &Value| l["width"].as_u64().unwrap_or(0);
            let full = links
                .iter()
                .filter(|l| width(l) > 0 && width(l) <= 2600)
                .max_by_key(|l| width(l))
                .or_else(|| links.iter().find(|l| l["rel"] == "canonical"))
                .and_then(|l| l["href"].as_str())
                .unwrap_or(&thumb)
                .to_string();
            let id = data["nasa_id"].as_str().unwrap_or("");
            let center = data["center"].as_str().unwrap_or("");
            Some(Picture {
                title: data["title"].as_str().unwrap_or("NASA").trim().to_string(),
                thumb,
                full,
                alt: Vec::new(),
                source: "NASA Image Library".into(),
                creator: data["secondary_creator"].as_str().or(data["photographer"].as_str()).map(str::to_string).unwrap_or_else(|| if center.is_empty() { "NASA".into() } else { format!("NASA {center}") }),
                license: "Public domain (NASA)".into(),
                page: if id.is_empty() { String::new() } else { format!("https://images.nasa.gov/details/{id}") },
                local: None,
                when: data["date_created"].as_str().map(|d| d.chars().take(10).collect()).unwrap_or_default(),
            })
        })
        .collect()
}

const EPIC_API: &str = "https://epic.gsfc.nasa.gov/api/natural";

/// NASA EPIC: the whole sunlit Earth from DSCOVR, 1.5 million km out, a frame every hour or two.
pub fn parse_epic(v: &Value) -> Vec<Picture> {
    let Some(frames) = v.as_array() else { return Vec::new() };
    frames
        .iter()
        .filter_map(|f| {
            let image = f["image"].as_str()?;
            let date = f["date"].as_str()?;
            let day = date.get(..10)?;
            let path = day.replace('-', "/");
            Some(Picture {
                title: format!("The Earth from DSCOVR, {} UTC", date.get(..16).unwrap_or(date)),
                thumb: format!("https://epic.gsfc.nasa.gov/archive/natural/{path}/thumbs/{image}.jpg"),
                full: format!("https://epic.gsfc.nasa.gov/archive/natural/{path}/jpg/{image}.jpg"),
                alt: Vec::new(),
                source: "NASA EPIC".into(),
                creator: "NASA EPIC team (DSCOVR)".into(),
                license: "Public domain (NASA)".into(),
                page: "https://epic.gsfc.nasa.gov/".into(),
                local: None,
                when: day.to_string(),
            })
        })
        .collect()
}

async fn from_source(name: &'static str, url: String, parse: fn(&Value) -> Vec<Picture>) -> (&'static str, Result<Vec<Picture>, String>) {
    let out = match tokio::time::timeout(SOURCE_TIMEOUT, get_json(&url, SOURCE_TIMEOUT)).await {
        Ok(Ok(v)) => Ok(parse(&v)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("no answer within seven seconds".into()),
    };
    (name, out)
}

/// Two lists, one after the other in turns.
fn interleave(a: Vec<Picture>, b: Vec<Picture>) -> Vec<Picture> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter(), b.into_iter());
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => out.extend(x.into_iter().chain(y)),
        }
    }
    out
}

/// What was found, and which sources answered (or why not).
#[derive(Debug, Default)]
struct Found {
    pictures: Vec<Picture>,
    used: Vec<&'static str>,
    failed: Vec<String>,
}

async fn search(query: &str, source: &str) -> Found {
    let q = query.trim().strip_prefix("the ").unwrap_or(query.trim()).to_string();
    let earth = is_the_earth(query);
    let mut found = Found::default();
    let mut lists: Vec<(&'static str, Result<Vec<Picture>, String>)> = Vec::new();
    match source {
        "openverse" => lists.push(from_source("Openverse", openverse_url(&q, 16), parse_openverse).await),
        "wikimedia" => lists.push(from_source("Wikimedia Commons", wikimedia_url(&q, 16), parse_wikimedia).await),
        "nasa" => lists.push(from_source("NASA", nasa_url(&q, 16), parse_nasa).await),
        "epic" => lists.push(from_source("NASA EPIC", EPIC_API.to_string(), parse_epic).await),
        _ if earth => {
            let (epic, nasa) = tokio::join!(from_source("NASA EPIC", EPIC_API.to_string(), parse_epic), from_source("NASA", nasa_url("earth from space blue marble", 10), parse_nasa));
            lists.extend([epic, nasa]);
        }
        _ if is_space(&q) => {
            let (nasa, ov, wm) = tokio::join!(from_source("NASA", nasa_url(&q, 12), parse_nasa), from_source("Openverse", openverse_url(&q, 8), parse_openverse), from_source("Wikimedia Commons", wikimedia_url(&q, 6), parse_wikimedia));
            lists.extend([nasa, ov, wm]);
        }
        _ => {
            let (ov, wm) = tokio::join!(from_source("Openverse", openverse_url(&q, 14), parse_openverse), from_source("Wikimedia Commons", wikimedia_url(&q, 10), parse_wikimedia));
            lists.extend([ov, wm]);
        }
    }
    let mut ordered: Vec<Vec<Picture>> = Vec::new();
    for (name, list) in lists {
        match list {
            Ok(list) if !list.is_empty() => {
                found.used.push(name);
                ordered.push(list);
            }
            Ok(_) => {}
            Err(e) => found.failed.push(format!("{name}: {e}")),
        }
    }
    // The Earth and space: the first source's in order, then the rest in turns.
    let merged = if earth || is_space(&q) {
        let mut it = ordered.into_iter();
        let first = it.next().unwrap_or_default();
        let rest = it.fold(Vec::new(), interleave);
        first.into_iter().chain(rest).collect()
    } else {
        ordered.into_iter().fold(Vec::new(), interleave)
    };
    let mut seen = std::collections::HashSet::new();
    found.pictures = merged.into_iter().filter(|p| !is_unsafe(&p.title) && seen.insert(p.full.clone())).take(MAX_PICTURES).collect();
    found
}

/// Pictures of a subject from the web sources, best first (the 3D studio rebuilds things from
/// them); none in tests and dry runs.
pub(crate) async fn photos(query: &str) -> Vec<Picture> {
    if offline() {
        return Vec::new();
    }
    search(query, "auto").await.pictures
}

/// The stand-in album of tests and dry runs.
fn demo_pictures(query: &str) -> Found {
    let label = if is_the_earth(query) { "The Earth".to_string() } else { title_case(query) };
    let pictures = (1..=6)
        .map(|n| Picture {
            title: format!("{label} — demo picture {n}"),
            thumb: format!("demo://{label}/{n}/thumb"),
            full: format!("demo://{label}/{n}"),
            alt: Vec::new(),
            source: if is_the_earth(query) { "NASA EPIC".into() } else { "Openverse".into() },
            creator: "JAI self-test".into(),
            license: "CC0".into(),
            page: String::new(),
            local: None,
            when: String::new(),
        })
        .collect();
    Found { pictures, used: vec!["demo"], failed: Vec::new() }
}

// ---------------------------------------------------------------------------- his files ----

/// Which of Sir's pictures.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalAsk {
    LatestScreenshot,
    Desktop,
    Pictures,
    Downloads,
    /// A file name (or words of it).
    Named(String),
    Path(PathBuf),
}

impl LocalAsk {
    fn parse(kind: &str, query: &str) -> Option<Self> {
        Some(match kind.trim().to_lowercase().as_str() {
            "latest_screenshot" | "screenshot" | "screenshots" => Self::LatestScreenshot,
            "desktop" => Self::Desktop,
            "pictures" | "photos" | "my_pictures" => Self::Pictures,
            "downloads" => Self::Downloads,
            "search" | "named" | "file" if !query.trim().is_empty() => Self::Named(query.trim().to_string()),
            _ => return None,
        })
    }

    fn label(&self) -> String {
        match self {
            Self::LatestScreenshot => "your latest screenshot".into(),
            Self::Desktop => "the pictures on your desktop".into(),
            Self::Pictures => "your Pictures folder".into(),
            Self::Downloads => "the pictures in your Downloads".into(),
            Self::Named(n) => format!("\"{n}\""),
            Self::Path(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string()),
        }
    }
}

/// One of Sir's known folders, where Windows says it is (he may have moved it to OneDrive).
#[cfg(windows)]
pub(crate) fn known_folder(name: &str) -> Option<PathBuf> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Desktop, FOLDERID_Downloads, FOLDERID_Pictures, FOLDERID_Screenshots, KF_FLAG_DEFAULT};
    let id = match name {
        "Desktop" => FOLDERID_Desktop,
        "Downloads" => FOLDERID_Downloads,
        "Pictures" => FOLDERID_Pictures,
        "Screenshots" => FOLDERID_Screenshots,
        _ => return None,
    };
    // SAFETY: the shell allocates the returned string; it is copied and freed with
    // CoTaskMemFree, as the API requires.
    let found = unsafe {
        match SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, HANDLE(0)) {
            Ok(text) => {
                let path = text.to_string().ok();
                CoTaskMemFree(Some(text.0 as *const _));
                path
            }
            Err(_) => None,
        }
    };
    found.map(PathBuf::from).filter(|p| p.is_dir())
}

#[cfg(not(windows))]
pub(crate) fn known_folder(_name: &str) -> Option<PathBuf> {
    None
}

/// Sir's folder `name` (the shell's answer, else the usual place).
pub(crate) fn folder(name: &str) -> PathBuf {
    known_folder(name).unwrap_or_else(|| crate::features::user_folder(name))
}

fn is_image_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXT.contains(&e.to_lowercase().as_str()))
}

/// The pictures under `roots` (each with its depth) that `keep` lets through, newest first.
pub fn local_pictures(roots: &[(PathBuf, usize)], keep: &dyn Fn(&Path) -> bool, limit: usize) -> Vec<(PathBuf, SystemTime)> {
    let mut found: Vec<(PathBuf, SystemTime)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (root, depth) in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root).max_depth(*depth).into_iter().flatten().take(30_000) {
            let path = entry.path();
            if !entry.file_type().is_file() || !is_image_file(path) || !keep(path) || !seen.insert(path.to_path_buf()) {
                continue;
            }
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((path.to_path_buf(), modified));
        }
    }
    found.sort_by(|a, b| b.1.cmp(&a.1));
    found.truncate(limit);
    found
}

fn local_picture(path: &Path, modified: SystemTime) -> Picture {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let when: chrono::DateTime<chrono::Local> = modified.into();
    Picture {
        title: name.clone(),
        thumb: path.display().to_string(),
        full: path.display().to_string(),
        alt: Vec::new(),
        source: "Your files".into(),
        creator: path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
        license: String::new(),
        page: String::new(),
        local: Some(path.to_path_buf()),
        when: when.format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// Sir's pictures for a request, newest first (blocking: walks folders).
fn collect_local(ask: &LocalAsk) -> Result<Vec<Picture>, String> {
    let onedrive = std::env::var_os("OneDrive").map(PathBuf::from);
    let pictures = folder("Pictures");
    let found = match ask {
        LocalAsk::Path(path) => {
            if path.is_file() {
                if !is_image_file(path) {
                    return Err(format!("{} is not a picture I can show (JPEG, PNG or WebP).", path.display()));
                }
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                let mut all = local_pictures(&[(dir, 1)], &|_| true, MAX_LOCAL);
                all.retain(|(p, _)| p != path);
                let modified = std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                std::iter::once((path.clone(), modified)).chain(all).collect()
            } else if path.is_dir() {
                local_pictures(&[(path.clone(), 1)], &|_| true, MAX_LOCAL)
            } else {
                return Err(format!("I can't find {} on this PC, Sir.", path.display()));
            }
        }
        LocalAsk::LatestScreenshot => {
            let mut roots = vec![(folder("Screenshots"), 1), (pictures.join("Screenshots"), 1)];
            if let Some(od) = &onedrive {
                roots.extend([(od.join("Pictures").join("Screenshots"), 1), (od.join("Bilder").join("Screenshots"), 1)]);
            }
            let in_folder = local_pictures(&roots, &|_| true, MAX_LOCAL);
            // Screenshots saved elsewhere (the desktop, Pictures) by their names.
            let named = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).is_some_and(|n| n.contains("screenshot") || n.contains("bildschirmfoto") || n.contains("screen shot"));
            let by_name = local_pictures(&[(folder("Desktop"), 1), (pictures.clone(), 2)], &named, MAX_LOCAL);
            let mut all: Vec<(PathBuf, SystemTime)> = in_folder.into_iter().chain(by_name).collect();
            all.sort_by(|a, b| b.1.cmp(&a.1));
            all.dedup_by(|a, b| a.0 == b.0);
            all.truncate(MAX_LOCAL);
            all
        }
        LocalAsk::Desktop => local_pictures(&[(folder("Desktop"), 1)], &|_| true, MAX_LOCAL),
        LocalAsk::Pictures => local_pictures(&[(pictures.clone(), 3)], &|_| true, MAX_LOCAL),
        LocalAsk::Downloads => local_pictures(&[(folder("Downloads"), 1)], &|_| true, MAX_LOCAL),
        LocalAsk::Named(name) => {
            let words: Vec<String> = name.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !IMAGE_EXT.contains(w)).map(str::to_string).collect();
            if words.is_empty() {
                return Err("Which file, Sir? Tell me its name.".into());
            }
            let matches = move |p: &Path| {
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                words.iter().all(|w| stem.contains(w.as_str()))
            };
            local_pictures(&[(pictures.clone(), 4), (folder("Desktop"), 2), (folder("Downloads"), 2)], &matches, MAX_LOCAL)
        }
    };
    if found.is_empty() {
        return Err(format!("I found no pictures for {}, Sir.", ask.label()));
    }
    Ok(found.iter().map(|(p, t)| local_picture(p, *t)).collect())
}

fn demo_local(ask: &LocalAsk) -> Vec<Picture> {
    (1..=3)
        .map(|n| {
            let path = PathBuf::from(format!("C:/Users/Example/Pictures/Screenshots/Screenshot demo {n}.png"));
            let mut p = local_picture(&path, SystemTime::UNIX_EPOCH);
            p.title = format!("{} (demo {n})", ask.label());
            p
        })
        .collect()
}

// --------------------------------------------------------------------------------- tool ----

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        // (Kept short: the whole prompt must fit the context window. "close" works too, unlisted.)
        "description": "Pictures on the HUD: the web (the Earth: NASA live) or Sir's files; each subject its own block. count: blocks side by side; spread: the open pictures each in its own block. Not spawn_native_window.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string"},
            "local": {"type": "string", "enum": ["latest_screenshot", "desktop", "pictures", "downloads", "search"]},
            "path": {"type": "string"},
            "action": {"type": "string", "enum": ["show", "next", "previous", "save", "spread"]},
            "count": {"type": "integer"}
        }}
    }})]
}

pub async fn run_tool(_tools: &ToolContext, args: &Value) -> Result<String, String> {
    run_tool_args(args).await
}

async fn run_tool_args(args: &Value) -> Result<String, String> {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        pixels::set_runtime(&rt);
    }
    let action = str_arg(args, &["action"]).unwrap_or("show").trim().to_lowercase();
    match action.as_str() {
        "next" | "forward" => navigate(1),
        "previous" | "prev" | "back" | "last" => navigate(-1),
        "save" | "download" => save_current(None).await,
        "spread" | "split" | "each" | "side_by_side" => {
            if album().items.len() < 2 {
                return Ok("Fewer than two pictures are open, so there is nothing to spread.\nSpoken: There's only one picture up, Sir. Tell me what else you'd like to see.".into());
            }
            let n = spread(args.get("count").and_then(|c| c.as_u64().or_else(|| c.as_str()?.trim().parse().ok())).map_or(SPREAD, |c| c as usize));
            Ok(format!("{n} pictures of \"{}\" side by side, each in a block of its own.\nSpoken: {n} of them side by side, Sir.", album().label))
        }
        "close" | "hide" => {
            if !is_open() {
                return Ok("The picture viewer is not open.\nSpoken: There's no picture up, Sir.".into());
            }
            if crate::hud_dock::close_named("pictures").is_empty() {
                close();
            }
            Ok("The picture viewer is closed.\nSpoken: Picture closed, Sir.".into())
        }
        _ => show(args).await,
    }
}

fn navigate(by: isize) -> Result<String, String> {
    if !is_open() || album().items.is_empty() {
        return Ok("No pictures are open.\nSpoken: There are no pictures up, Sir. Tell me what you'd like to see.".into());
    }
    let pic = step(None, by).ok_or("No pictures are open.")?;
    let a = album();
    Ok(format!("Picture {} of {}: \"{}\" ({}).\nSpoken: {}", a.index + 1, a.items.len(), pic.title, pic.credit(), short_title(&pic.title)))
}

fn short_title(title: &str) -> String {
    let words: Vec<&str> = title.split_whitespace().collect();
    let t = if words.len() > 9 { format!("{}…", words[..9].join(" ")) } else { title.to_string() };
    format!("{t}, Sir.")
}

/// The words of a request, tidied ("of the eiffel tower please" → "the eiffel tower").
fn clean_query(raw: &str) -> String {
    let mut q = raw.trim().trim_matches(|c: char| matches!(c, '"' | '\'' | '.' | '?' | '!')).trim().to_string();
    for tail in [" please", " for me", " on the hud", " on my screen", " on the screen", " in a window", " bitte", " für mich", " right now", " now"] {
        if let Some(rest) = q.strip_suffix(tail) {
            q = rest.trim().to_string();
        }
    }
    for lead in ["of ", "from ", "von ", "vom "] {
        if let Some(rest) = q.strip_prefix(lead) {
            q = rest.trim().to_string();
        }
    }
    q
}

async fn show(args: &Value) -> Result<String, String> {
    let query = str_arg(args, &["query", "q", "subject", "title"]).map(clean_query).unwrap_or_default();
    let local = str_arg(args, &["local"]).and_then(|k| LocalAsk::parse(k, &query));
    let path = str_arg(args, &["path", "file"]).map(|p| PathBuf::from(p.trim().trim_matches('"')));
    if let Some(ask) = path.map(LocalAsk::Path).or(local) {
        return show_local(ask).await;
    }
    if query.is_empty() {
        return Err("Which picture, Sir? Name a subject (\"a picture of the Eiffel Tower\") or a file.".into());
    }
    if is_unsafe(&query) {
        return Ok("Refused: the picture viewer shows safe-for-work pictures only.\nSpoken: I keep the pictures on the HUD safe for work, Sir.".into());
    }
    let source = str_arg(args, &["source"]).unwrap_or("auto").to_lowercase();
    let earth = is_the_earth(&query);
    let label = if earth { "the Earth".to_string() } else { query.clone() };
    // The block is up at once; the pictures follow.
    let searching = match source.as_str() {
        "openverse" => "Searching Openverse…".to_string(),
        "wikimedia" => "Searching Wikimedia Commons…".to_string(),
        "nasa" => "Searching NASA's library…".to_string(),
        "epic" => "Fetching NASA EPIC…".to_string(),
        _ if earth => "Fetching today's Earth from NASA EPIC…".to_string(),
        _ => "Searching Openverse and Wikimedia Commons…".to_string(),
    };
    let id = open_block(Album { label: label.clone(), items: Vec::new(), index: 0, version: 0, note: searching, busy: true }, false);
    let block = Some(id.as_str());
    let started = Instant::now();
    let found = if offline() { demo_pictures(&query) } else { search(&query, &source).await };
    info!("[IMAGES] '{query}': {} pictures from {:?} in {} ms{}", found.pictures.len(), found.used, started.elapsed().as_millis(), if found.failed.is_empty() { String::new() } else { format!(" (failed: {})", found.failed.join("; ")) });
    if found.pictures.is_empty() {
        update_in(block, |a| {
            a.busy = false;
            a.note = if found.failed.is_empty() { format!("Nothing found for \"{label}\".") } else { format!("The picture sources did not answer ({}).", found.failed.join("; ")) };
        });
        let why = if found.failed.is_empty() { format!("I found no pictures of {label}, Sir.") } else { "The picture libraries aren't answering right now, Sir.".to_string() };
        return Ok(format!("No pictures for \"{label}\" ({}).\nSpoken: {why}", if found.failed.is_empty() { "no results".to_string() } else { found.failed.join("; ") }));
    }
    let pictures = found.pictures;
    prefetch(&pictures, 0);
    let first = pictures[0].clone();
    let n = pictures.len();
    update_in(block, |a| {
        a.items = pictures;
        a.index = 0;
        a.busy = false;
        a.note.clear();
    });
    crate::hud_dock::describe_block(&id, &format!("PICTURES // {label}: {n} pictures. 1: {} ({})", first.title, first.credit()));
    let sources = found.used.join(", ");
    // "Show me four pictures of Paris at once": each in a block of its own.
    let blocks = match args.get("count").and_then(|c| c.as_u64().or_else(|| c.as_str()?.trim().parse().ok())) {
        Some(k) if k > 1 && n > 1 => spread(k as usize),
        _ => 1,
    };
    let spoken = if earth {
        format!("The Earth as DSCOVR sees it, Sir — {n} frames from a million and a half kilometres out.")
    } else if blocks > 1 {
        format!("Here's {label}, Sir — {blocks} pictures side by side.")
    } else {
        format!("Here's {label}, Sir — {n} pictures on the HUD.")
    };
    Ok(format!(
        "The picture viewer is open on the HUD with {n} pictures of \"{label}\" ({sources}). Showing 1 of {n}: \"{}\" — {}. Sir can say \"next picture\", \"save this picture\" or \"detach it\".\nSpoken: {spoken}",
        first.title,
        first.credit()
    ))
}

/// The first full picture and every thumbnail start loading at once.
fn prefetch(pictures: &[Picture], index: usize) {
    for p in pictures.iter() {
        pixels::want(&p.thumb_key(), &p.thumb_origin(), THUMB_SIDE, p.local.is_none());
    }
    if let Some(p) = pictures.get(index) {
        pixels::want(&p.full_key(), &p.full_origin(), FULL_SIDE, p.local.is_none());
    }
}

async fn show_local(ask: LocalAsk) -> Result<String, String> {
    let label = ask.label();
    let pictures = if offline() {
        demo_local(&ask)
    } else {
        let ask2 = ask.clone();
        tokio::task::spawn_blocking(move || collect_local(&ask2)).await.map_err(|e| e.to_string())??
    };
    let n = pictures.len();
    let first = pictures[0].clone();
    prefetch(&pictures, 0);
    let id = open_block(Album { label: label.clone(), items: pictures, index: 0, version: 0, note: String::new(), busy: false }, false);
    crate::hud_dock::describe_block(&id, &format!("PICTURES // {label}: {n} files. 1: {}", first.title));
    let spoken = match ask {
        LocalAsk::LatestScreenshot => format!("Your latest screenshot, Sir — from {}.", first.when),
        _ if n == 1 => format!("{}, Sir.", first.title),
        _ => format!("{n} pictures, Sir — the newest first."),
    };
    Ok(format!("The picture viewer shows {label} ({n} file(s), newest first). 1: {} ({}).\nSpoken: {spoken}", first.title, first.when))
}

/// "jpg", "png", "webp" from the bytes themselves.
fn sniff_extension(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        [b'G', b'I', b'F', ..] => "gif",
        _ => "jpg",
    }
}

/// A file name in `dir` that is not taken yet ("Tokyo.jpg", "Tokyo (2).jpg").
fn free_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..1000).map(|n| dir.join(format!("{stem} ({n}).{ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// Saves block `id`'s picture on show (None: the latest block's).
pub(crate) async fn save_current(id: Option<&str>) -> Result<String, String> {
    let set_note = |text: &str| set_note(id, text);
    let Some(pic) = album_of(id).current().cloned().filter(|_| is_open()) else {
        return Ok("No picture is open.\nSpoken: There's no picture up to save, Sir.".into());
    };
    if let Some(path) = &pic.local {
        return Ok(format!("That picture is already on the PC: {}.\nSpoken: That one is already on your PC, Sir — {}.", path.display(), pic.title));
    }
    if offline() {
        return Ok(format!("Dry run: \"{}\" would be saved to Pictures\\JAI.\nSpoken: Saved, Sir.", pic.title));
    }
    set_note("Saving…");
    let mut bytes = None;
    for url in std::iter::once(&pic.full).chain(pic.alt.iter()).chain(std::iter::once(&pic.thumb)) {
        if let Ok(Fetched::Bytes { bytes: b, .. }) = fetch(url, true, None).await {
            bytes = Some(b);
            break;
        }
    }
    let Some(bytes) = bytes else {
        set_note("Saving failed: the picture could not be downloaded.");
        return Err("The picture could not be downloaded, so nothing was saved.".into());
    };
    let dir = folder("Pictures").join("Jarvis");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let stem: String = crate::features::safe_name(&pic.title).chars().take(70).collect();
    let path = free_name(&dir, stem.trim(), sniff_extension(&bytes));
    tokio::fs::write(&path, &bytes).await.map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    info!("[IMAGES] Saved '{}' to {}", pic.title, path.display());
    set_note(&format!("Saved to {}", path.display()));
    let credit = if pic.license.is_empty() { String::new() } else { format!(" It's {}, credit {}.", pic.license, if pic.creator.is_empty() { pic.source.clone() } else { pic.creator.clone() }) };
    Ok(format!("Saved \"{}\" to {} ({}).\nSpoken: Saved to your Pictures folder, under JAI, Sir.{credit}", pic.title, path.display(), pic.credit()))
}

/// A window asked for with nothing in it whose title names a picture, a camera or street view
/// (live 2026-11-08: "open an image of the Earth" became `spawn_native_window {"title": "Earth
/// Image"}` and showed CPU telemetry): the viewer that shows it instead.
pub fn reroute_window(args: &Value) -> Option<(&'static str, Value)> {
    const CONTENT: &[&str] = &["markdown", "text", "content", "body", "code", "blocks", "widgets", "widget_ids", "live", "restore", "from_last_reply"];
    if CONTENT.iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null() && *v != json!(false) && *v != json!(""))) || args["telemetry"] == json!(true) {
        return None;
    }
    let title = str_arg(args, &["title", "name", "id"])?.replace(['_', '-'], " ").to_lowercase();
    let words: Vec<&str> = title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(w));
    const FILLER: &[&str] = &["image", "images", "picture", "pictures", "photo", "photos", "bild", "bilder", "foto", "fotos", "viewer", "window", "of", "the", "a", "an", "live", "camera", "cameras", "webcam", "webcams", "cam", "cams", "feed", "street", "view", "streetview", "von", "der", "die", "das", "jai"];
    let subject = || words.iter().filter(|w| !FILLER.contains(w)).copied().collect::<Vec<_>>().join(" ");
    if title.contains("street view") || has(&["streetview", "straßenansicht"]) {
        let place = subject();
        return Some(("show_street_view", if place.is_empty() { json!({"here": true}) } else { json!({"place": place}) }));
    }
    if has(&["camera", "cameras", "webcam", "webcams", "cam", "cams", "livecam", "kamera"]) {
        let place = subject();
        return (!place.is_empty()).then(|| ("show_live_camera", json!({"place": place})));
    }
    if has(&["image", "images", "picture", "pictures", "photo", "photos", "bild", "bilder", "foto", "fotos", "wallpaper"]) {
        let query = subject();
        return (!query.is_empty()).then(|| (TOOL, json!({"query": query})));
    }
    None
}

// ------------------------------------------------------------------------- Sir's words ----

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid media regex")
}

/// "Show me a picture of the Eiffel Tower", "zeig mir Bilder von Tokio", "open my latest
/// screenshot", "next picture", "save this picture"; then the cameras' and street view's own
/// phrases (`webcams::phrase`, `street_view::phrase`). The fast path of all three
/// (`intents::hud_view_command`).
pub fn voice_intent(text: &str) -> Option<Intent> {
    let s = crate::intents::normalize_phrase(text);
    if s.is_empty() || s.split_whitespace().count() > 22 {
        return None;
    }
    if let Some(args) = crate::street_view::phrase(&s) {
        return Some(Intent::Tool { name: crate::street_view::TOOL, args });
    }
    if let Some(args) = crate::webcams::phrase(&s) {
        return Some(Intent::Tool { name: crate::webcams::TOOL, args });
    }
    phrase(text, &s).map(|args| Intent::Tool { name: TOOL, args })
}

const NOUN: &str = r"(?:pictures?|images?|photos?|photographs?|pics?|wallpapers?|snapshots?)";
const NOUN_DE: &str = r"(?:bild|bilder|foto|fotos|fotografien?|aufnahmen?)";

static NEXT: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:show (?:me )?|go to )?(?:the )?(?:next|another|other) {NOUN}$|^(?:zeig (?:mir )?)?(?:das )?(?:nächste|naechste|nächstes|naechstes|weitere|weiteres|anderes) {NOUN_DE}$")));
static PREVIOUS: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:show (?:me )?|go (?:back )?to )?(?:the )?(?:previous|prior|last) {NOUN}$|^(?:go )?back (?:one|a) {NOUN}$|^(?:zeig (?:mir )?)?(?:das )?(?:vorherige|vorheriges|letzte|letztes) {NOUN_DE}$|^ein {NOUN_DE} zurück$")));
static SAVE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:save|download|keep|store) (?:this|that|the|the current|current)? ?{NOUN}(?: for me)?(?: (?:to|in|into) my (?:pictures|photos)(?: folder)?)?$|^(?:speicher|speichere|speichern|sichere|lade) (?:das|dieses|dies) {NOUN_DE}(?: ab| herunter| runter)?$|^(?:das|dieses) {NOUN_DE} (?:speichern|abspeichern|herunterladen)$")));
static SCREENSHOT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:show|open|display|pull up|bring up|get)(?: me)? )?(?:my |the )?(?:latest|last|newest|most recent|recent|new) (?:screenshot|screen shot|screen capture|snip)$|^(?:zeig|zeige|öffne|oeffne)(?: mir)? (?:meinen |den )?(?:letzten|neuesten|aktuellsten) (?:screenshot|bildschirmfoto)$"));
static DESKTOP: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:show|open|display)(?: me)? (?:the |that |my )?(?:{NOUN}|screenshots?) (?:on|from) (?:my|the) desktop$|^(?:zeig|zeige|öffne|oeffne)(?: mir)? (?:das |die )?{NOUN_DE} (?:auf|von) (?:meinem|dem) desktop$")));
static MY_PICTURES: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show|open|display)(?: me)? my (?:pictures|photos|images|photo library|pictures folder)$|^(?:zeig|zeige|öffne|oeffne)(?: mir)? meine (?:bilder|fotos)$"));
static DOWNLOADS: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:show|open|display)(?: me)? (?:the )?{NOUN} (?:in|from) my downloads$|^(?:zeig|zeige|öffne)(?: mir)? (?:die )?{NOUN_DE} (?:in|aus) meinen downloads$")));
static NAMED: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:show|open|display|find)(?: me)? (?:the |my )?(?:{NOUN}|screenshot|file) (?:called|named) (?P<q>.+)$|^(?:zeig|zeige|öffne|oeffne|finde)(?: mir)? (?:das )?{NOUN_DE} (?:namens|mit dem namen) (?P<q2>.+)$")));
static FILE_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show|open|display|zeig|zeige|öffne|oeffne)(?: me| mir)? (?P<q>[\w .-]{1,80}\.(?:jpg|jpeg|png|webp))$"));
static PATH: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)(?P<p>[a-z]:\\[^"<>|?*\r\n]+?\.(?:jpe?g|png|webp))"#));
static WEB: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:(?:show|display|find|get|open|pull up|bring up|give|fetch|load|put up|search(?: for)?|look up)(?: me| us)? )?(?:(?:a|an|some|the|a few|few|several|multiple|more|any|one|two|three|four|five|six|\d+) )?(?:(?:nice|cool|good|great|beautiful|real|recent|new|current|high resolution|hd) )?{NOUN} (?:of|from|showing|with|about) (?P<q>.+)$"
    ))
});
static LOOKS_LIKE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show me|zeig mir) (?:what|wie) (?P<q>.+?) (?:looks like|look like|aussieht|aussehen)$"));
static WEB_DE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:zeig|zeige|öffne|oeffne|hol|such|suche|finde|lade)(?: mir| uns)? (?:(?:ein|eine|einige|ein paar|mehrere|paar|das|die|\d+|zwei|drei|vier|fünf) )?(?:(?:schöne|schönes|tolle|tolles|aktuelle|aktuelles|neue|neues) )?{NOUN_DE} (?:von|vom|der|des|aus|zu|zum|zur|über|mit) (?P<q>.+)$"
    ))
});
static SPREAD_OUT: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:open|show|put|display)(?: me)? (?:each|every|all(?: the)?|the) {NOUN} (?:(?:in|into|as) )?(?:(?:its|their) own|an?|separate|individual|own|single|different)? ?(?:own |individual |separate )?(?:windows?|blocks?)$|^(?:spread|lay|split|fan)(?: out)? (?:the |these |those |my )?(?:{NOUN}|windows|blocks|them)(?: out)?(?: side by side)?$|^(?:show|put|lay|place)(?: me)? (?:the )?(?:{NOUN}|them|windows) (?:out )?side by side$|^(?:show|open)(?: me)? (?:multiple|several|more|many|a few|some|\d+|two|three|four|five|six)(?: {NOUN}| of them| windows)? (?:at (?:the same time|once)|side by side|together|simultaneously)$|^(?:zeig|zeige|öffne|oeffne)(?: mir)? (?:die {NOUN_DE}|sie) (?:nebeneinander|gleichzeitig)$|^(?:öffne|oeffne|zeig|zeige)(?: mir)? jedes {NOUN_DE} in einem eigenen fenster$"
    ))
});
/// "… at once", "… side by side": each picture in a block of its own.
static TOGETHER: LazyLock<Regex> = LazyLock::new(|| re(r" (?:at (?:the same time|once)|side by side|simultaneously|together|in (?:separate|different|multiple|individual) (?:windows|blocks)|each in (?:its|their) own window|nebeneinander|gleichzeitig)$"));
static COUNT: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b(two|three|four|five|six|[2-6]|multiple|several|a few|zwei|drei|vier|mehrere) (?:{NOUN}|{NOUN_DE})\b")));
static SUBJECT_FIRST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show|display|find|open)(?: me)? (?P<q>[a-z0-9äöüß][a-z0-9äöüß .-]{1,50}?) (?:pictures|photos|images|pics|wallpapers)$"));

/// `show_image` arguments for one of Sir's sentences (`s` normalised, `raw` as said).
fn phrase(raw: &str, s: &str) -> Option<Value> {
    if let Some(c) = PATH.captures(raw) {
        return Some(json!({"path": c["p"].trim()}));
    }
    if NEXT.is_match(s) {
        return Some(json!({"action": "next"}));
    }
    if PREVIOUS.is_match(s) {
        return Some(json!({"action": "previous"}));
    }
    if SAVE.is_match(s) {
        return Some(json!({"action": "save"}));
    }
    if SCREENSHOT.is_match(s) {
        return Some(json!({"local": "latest_screenshot"}));
    }
    if DESKTOP.is_match(s) {
        return Some(json!({"local": "desktop"}));
    }
    if MY_PICTURES.is_match(s) {
        return Some(json!({"local": "pictures"}));
    }
    if DOWNLOADS.is_match(s) {
        return Some(json!({"local": "downloads"}));
    }
    if let Some(c) = NAMED.captures(s) {
        let q = c.name("q").or_else(|| c.name("q2")).map(|m| clean_query(m.as_str())).filter(|q| !q.is_empty())?;
        return Some(json!({"local": "search", "query": q}));
    }
    if let Some(c) = FILE_NAME.captures(s) {
        return Some(json!({"local": "search", "query": c["q"].trim()}));
    }
    // ("Spread the windows" with no pictures up is the HUD's.)
    static NAMES_PICTURES: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b(?:{NOUN}|{NOUN_DE})\b")));
    if SPREAD_OUT.is_match(s) && (is_open() || NAMES_PICTURES.is_match(s)) {
        return Some(json!({"action": "spread"}));
    }
    let together = TOGETHER.find(s);
    let s = together.map_or(s, |m| s[..m.start()].trim_end());
    let count = COUNT
        .captures(s)
        .map(|c| match &c[1] {
            "two" | "2" | "zwei" => 2,
            "three" | "3" | "drei" => 3,
            "five" | "5" => 5,
            "six" | "6" => 6,
            _ => SPREAD,
        })
        .or(together.map(|_| SPREAD));
    let subject = WEB
        .captures(s)
        .or_else(|| WEB_DE.captures(s))
        .or_else(|| LOOKS_LIKE.captures(s))
        .or_else(|| SUBJECT_FIRST.captures(s).filter(|c| !matches!(c["q"].split_whitespace().next(), Some("my" | "your" | "the" | "some" | "a" | "me" | "all"))))
        .map(|c| clean_query(&c["q"]))?;
    // (Words that only point at something else: "a picture of that", "photos of my screen".)
    if subject.is_empty() || matches!(subject.as_str(), "it" | "that" | "this" | "them" | "me" | "him" | "her" | "es" | "das" | "dem") || subject.starts_with("my screen") || subject.starts_with("meinem bildschirm") {
        return None;
    }
    let query = if is_the_earth(&subject) { "the Earth".to_string() } else { subject };
    Some(match count {
        Some(k) => json!({"query": query, "count": k}),
        None => json!({"query": query}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Option<(&'static str, Value)> {
        match voice_intent(text) {
            Some(Intent::Tool { name, args }) => Some((name, args)),
            _ => None,
        }
    }

    #[test]
    fn sirs_picture_requests_reach_the_viewer() {
        let cases: &[(&str, Value)] = &[
            ("Show me a picture of the Eiffel Tower.", json!({"query": "the eiffel tower"})),
            ("JAI, open an image of the Earth.", json!({"query": "the Earth"})),
            ("Zeig mir ein Bild von Neuschwanstein", json!({"query": "neuschwanstein"})),
            ("Zeig mir ein Bild der Erde", json!({"query": "the Earth"})),
            ("show me photos of Tokyo", json!({"query": "tokyo"})),
            ("can you find me some pictures of red pandas please", json!({"query": "red pandas"})),
            ("show me what a pangolin looks like", json!({"query": "a pangolin"})),
            ("show me the photo on my desktop", json!({"local": "desktop"})),
            ("open my latest screenshot", json!({"local": "latest_screenshot"})),
            ("Zeig mir meinen letzten Screenshot", json!({"local": "latest_screenshot"})),
            ("show me my photos", json!({"local": "pictures"})),
            ("open the picture called holiday beach", json!({"local": "search", "query": "holiday beach"})),
            ("next picture", json!({"action": "next"})),
            ("nächstes Bild", json!({"action": "next"})),
            ("previous image", json!({"action": "previous"})),
            ("save this picture", json!({"action": "save"})),
            ("Speichere das Bild", json!({"action": "save"})),
            ("show me Tokyo pictures", json!({"query": "tokyo"})),
            // (2026-11-09) Several at once, each in a block of its own.
            ("Can you open each image individual window?", json!({"action": "spread"})),
            ("open every picture in its own window", json!({"action": "spread"})),
            ("spread the pictures", json!({"action": "spread"})),
            ("Show the photos side by side", json!({"action": "spread"})),
            ("Zeig mir die Bilder nebeneinander", json!({"action": "spread"})),
            ("show me four pictures of Paris at once", json!({"query": "paris", "count": 4})),
            ("show me multiple pictures of the Statue of Liberty at the same time", json!({"query": "the statue of liberty", "count": 4})),
            ("show me two photos of Tokyo", json!({"query": "tokyo", "count": 2})),
        ];
        for (text, want) in cases {
            assert_eq!(said(text), Some((TOOL, want.clone())), "{text}");
        }
        let path = said(r"Open C:\Users\Example\Pictures\cat.png").expect("a path");
        assert_eq!(path.1, json!({"path": r"C:\Users\Example\Pictures\cat.png"}));
    }

    #[test]
    fn other_requests_stay_with_their_handlers() {
        for text in [
            "next",
            "save it",
            "show me my calendar",
            "open my pictures folder in explorer and delete it",
            "take a picture of me",
            "show me a picture of that",
            "what's on my screen",
            "show me the earth",
            "play the next song",
            "show me some pictures",
            "show me my camera",
        ] {
            assert_eq!(said(text), None, "{text}");
        }
    }

    #[test]
    fn unsafe_words_are_caught_in_both_languages() {
        assert!(is_unsafe("nude beach"));
        assert!(is_unsafe("Nacktbild"));
        assert!(is_unsafe("a dead body"));
        assert!(!is_unsafe("Essex coastline"), "whole words only");
        assert!(!is_unsafe("the Eiffel Tower"));
    }

    #[test]
    fn source_formats_parse() {
        let ov = json!({"results": [
            {"title": "Shibuya Crossing", "url": "https://live.staticflickr.com/1/2.jpg", "thumbnail": "https://api.openverse.org/v1/images/x/thumb/", "creator": "Jane", "license": "by", "license_version": "2.0", "provider": "flickr", "foreign_landing_url": "https://flickr.com/p/2"},
            {"title": "Flagged", "url": "https://example.org/m.jpg", "mature": true}
        ]});
        let p = parse_openverse(&ov);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].license.as_str(), p[0].source.as_str(), p[0].creator.as_str()), ("CC BY 2.0", "Openverse (Flickr)", "Jane"));

        let wm = json!({"query": {"pages": {
            "2": {"index": 2, "title": "File:Small_tower.png", "imageinfo": [{"mime": "image/png", "url": "https://upload.wikimedia.org/a/b/Small_tower.png", "thumburl": "https://upload.wikimedia.org/thumb/a/b/Small_tower.png/400px-Small_tower.png", "width": 800, "descriptionurl": "https://commons.wikimedia.org/wiki/File:Small_tower.png", "extmetadata": {"Artist": {"value": "<a href=\"x\">Max &amp; Co</a>"}, "LicenseShortName": {"value": "CC BY-SA 4.0"}}}]},
            "1": {"index": 1, "title": "File:Big_tower.jpg", "imageinfo": [{"mime": "image/jpeg", "url": "https://upload.wikimedia.org/c/d/Big_tower.jpg", "thumburl": "https://upload.wikimedia.org/thumb/c/d/Big_tower.jpg/400px-Big_tower.jpg", "width": 6000, "descriptionurl": "d", "extmetadata": {"ObjectName": {"value": "Eiffel Tower at night"}}}]},
            "3": {"index": 3, "title": "File:Anim.gif", "imageinfo": [{"mime": "image/gif", "url": "u"}]}
        }}});
        let p = parse_wikimedia(&wm);
        assert_eq!(p.len(), 2, "the GIF is left out");
        assert_eq!(p[0].title, "Eiffel Tower at night");
        assert_eq!(p[0].full, "https://upload.wikimedia.org/thumb/c/d/Big_tower.jpg/1920px-Big_tower.jpg");
        assert_eq!(p[0].alt, vec!["https://upload.wikimedia.org/c/d/Big_tower.jpg".to_string()]);
        assert_eq!((p[1].title.as_str(), p[1].creator.as_str(), p[1].full.as_str()), ("Small tower", "Max & Co", "https://upload.wikimedia.org/a/b/Small_tower.png"));

        let nasa = json!({"collection": {"items": [{"data": [{"title": "Saturn Atmosphere", "nasa_id": "PIA01973", "center": "JPL", "date_created": "1999-06-16T10:03:17Z"}], "links": [
            {"href": "https://images-assets.nasa.gov/image/PIA01973/PIA01973~thumb.jpg", "rel": "preview", "render": "image", "width": 425},
            {"href": "https://images-assets.nasa.gov/image/PIA01973/PIA01973~orig.jpg", "rel": "canonical", "render": "image", "width": 4000},
            {"href": "https://images-assets.nasa.gov/image/PIA01973/PIA01973~medium.jpg", "rel": "alternate", "render": "image", "width": 1280}
        ]}]}});
        let p = parse_nasa(&nasa);
        assert_eq!(p[0].full, "https://images-assets.nasa.gov/image/PIA01973/PIA01973~medium.jpg");
        assert_eq!((p[0].page.as_str(), p[0].when.as_str()), ("https://images.nasa.gov/details/PIA01973", "1999-06-16"));

        let epic = json!([{"image": "epic_1b_20261108003634", "date": "2026-11-08 00:31:45", "caption": "x"}]);
        let p = parse_epic(&epic);
        assert_eq!(p[0].full, "https://epic.gsfc.nasa.gov/archive/natural/2026/11/08/jpg/epic_1b_20261108003634.jpg");
        assert_eq!(p[0].thumb, "https://epic.gsfc.nasa.gov/archive/natural/2026/11/08/thumbs/epic_1b_20261108003634.jpg");
        assert!(is_the_earth("the Earth") && is_the_earth("die Erde") && !is_the_earth("earthquakes"));
    }

    #[test]
    fn an_empty_window_for_a_picture_goes_to_the_viewer() {
        assert_eq!(reroute_window(&json!({"telemetry": false, "title": "Earth Image"})), Some((TOOL, json!({"query": "earth"}))));
        assert_eq!(reroute_window(&json!({"title": "Live camera London"})), Some(("show_live_camera", json!({"place": "london"}))));
        assert_eq!(reroute_window(&json!({"title": "Street View - Eiffel Tower"})), Some(("show_street_view", json!({"place": "eiffel tower"}))));
        assert_eq!(reroute_window(&json!({"title": "Earth Image", "markdown": "text"})), None);
        assert_eq!(reroute_window(&json!({"title": "PC Stats", "live": "pc_stats"})), None);
        assert_eq!(reroute_window(&json!({"title": "Notes"})), None);
    }

    #[test]
    fn local_pictures_come_newest_first_and_names_match() {
        let dir = std::env::temp_dir().join(format!("jarvis_image_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for (name, secs) in [("Screenshot 2026-11-01.png", 10), ("holiday beach.jpg", 20), ("notes.txt", 30), ("sub/deep.webp", 40)] {
            let path = dir.join(name);
            std::fs::write(&path, b"x").unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)).unwrap();
        }
        let all = local_pictures(&[(dir.clone(), 2)], &|_| true, 10);
        let names: Vec<String> = all.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["deep.webp", "holiday beach.jpg", "Screenshot 2026-11-01.png"]);
        assert_eq!(local_pictures(&[(dir.clone(), 1)], &|_| true, 10).len(), 2, "depth 1 stays out of sub");
        let beach = local_pictures(&[(dir.clone(), 2)], &|p: &Path| p.file_stem().unwrap().to_string_lossy().contains("beach"), 10);
        assert_eq!(beach.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(free_name(Path::new("Z:/nowhere"), "Tokyo", "jpg"), PathBuf::from("Z:/nowhere/Tokyo.jpg"));
        assert_eq!(sniff_extension(&[0x89, b'P', b'N', b'G', 0, 0]), "png");
        assert_eq!(sniff_extension(&[0xFF, 0xD8, 0xFF]), "jpg");
    }

    #[test]
    fn the_viewer_runs_offline_on_stand_ins() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let out = rt.block_on(show(&json!({"query": "the Earth"}))).unwrap();
        assert!(out.contains("Spoken: The Earth as DSCOVR sees it"), "{out}");
        assert!(is_open());
        let a = album();
        assert_eq!(a.items.len(), 6);
        assert_eq!(a.items[0].source, "NASA EPIC");
        // The stand-in pixels are there at once.
        assert!(pixels::get(&a.items[0].full_key()).is_some());
        assert!(navigate(1).unwrap().contains("Picture 2 of 6"));
        assert!(navigate(-2).unwrap().contains("Picture 6 of 6"));
        let saved = rt.block_on(save_current(None)).unwrap();
        assert!(saved.starts_with("Dry run"), "{saved}");
        let refused = rt.block_on(show(&json!({"query": "nude beach"}))).unwrap();
        assert!(refused.contains("safe for work"));
        // (2026-11-09) A second subject opens beside the first; the same subject again stays in
        // its block; "spread them" and "four at once" give each picture a block of its own.
        rt.block_on(show(&json!({"query": "the Eiffel Tower"}))).unwrap();
        assert_eq!(block_ids(), ["image_viewer", "image_viewer_2"]);
        rt.block_on(show(&json!({"query": "the Eiffel Tower"}))).unwrap();
        assert_eq!(block_ids().len(), 2);
        let spread_out = rt.block_on(run_tool_args(&json!({"action": "spread", "count": 3}))).unwrap();
        assert!(spread_out.starts_with("3 pictures"), "{spread_out}");
        let ids = block_ids();
        assert_eq!(ids.len(), 4);
        let shown: Vec<usize> = ids[1..].iter().map(|id| album_of(Some(id)).index).collect();
        assert_eq!(shown, [0, 1, 2], "each block a different picture");
        let out = rt.block_on(show(&json!({"query": "the Statue of Liberty", "count": 4}))).unwrap();
        assert!(out.contains("side by side"), "{out}");
        assert_eq!(block_ids().len(), MAX_BLOCKS, "the oldest made way");
        close_block(&block_ids()[0]);
        assert_eq!(block_ids().len(), MAX_BLOCKS - 1);
        close();
        assert!(!is_open());
    }

    #[test]
    fn demo_pictures_are_panorama_shaped() {
        let img = demo_image(7, 256);
        assert_eq!(img.size, [256, 128]);
        assert_eq!(decode(&[1, 2, 3], 100), None);
        assert!(http_date("Sun, 08 Nov 2026 23:12:35 GMT").is_some());
    }

    #[test]
    fn a_phone_portrait_stands_upright() {
        // 32×16, red left and blue right, saved as a phone does: sideways, with EXIF orientation
        // 6 ("turn it a quarter clockwise"). Upright it is 16×32, red on top.
        let img = image::RgbImage::from_fn(32, 16, |x, _| if x < 16 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut jpeg = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        let mut exif = vec![0xFF, 0xE1, 0, 34];
        exif.extend_from_slice(b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
        jpeg.splice(2..2, exif);
        let up = load_upright(&jpeg).unwrap().to_rgb8();
        assert_eq!(up.dimensions(), (16, 32));
        assert!(up.get_pixel(8, 4)[0] > 200 && up.get_pixel(8, 28)[2] > 200, "red on top, blue below");
        assert_eq!(decode(&jpeg, 100).unwrap().size, [16, 32]);
    }
}
