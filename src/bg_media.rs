//! Pictures, videos and slideshows on the black of the board (Sir, 2026-11-21).
//!
//! The board is the desktop's background, and its canvas is black. Here that black can carry
//! media of Sir's own, local files only, one source for every monitor or one for each:
//! a still picture, a video (silent, or with its sound) or a folder of pictures that is a
//! slideshow (a soft cross-fade every N seconds). Two modes: `behind` (the board stands over the
//! media, which a black veil dims so the board stays readable) and `whole` (the media alone
//! fills the background; the board comes when he is spoken to or told "show the board", and goes
//! again afterwards).
//!
//! Decoding is off the HUD's thread: pictures by `image`, videos by `mp4` (demux), `openh264`
//! (H.264 video) and `symphonia` (AAC) with `cpal` for the sound. Only H.264 in MP4 plays; any
//! other file gets its reason in a plain sentence. A decoder idles whenever the background cannot
//! be seen (a game or a fullscreen program in front, the PC locked, the window hidden), and
//! presents at most 30 frames a second, skipping a frame rather than falling behind.

use crate::interface::InterfaceSettings;
use eframe::egui::{self, pos2, Color32, Rect, TextureHandle, TextureOptions};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub const PICTURES: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// The key of the source that every monitor without one of its own takes.
pub const ALL: &str = "all";
/// A video never presents more often than this.
const MAX_FPS: f64 = 30.0;
/// A frame later than this behind its time is not shown.
const LATE: Duration = Duration::from_millis(100);
/// A decoder that has not been looked at for this long idles (the window is hidden).
const STALE_MS: u64 = 1500;
/// The soft cross-fade between two pictures of a slideshow, at most (a third of the interval).
const FADE_SECS: f32 = 1.5;
/// How long the board stays once it was called and nothing more happens, and after a "show the
/// board" (whole mode).
const BOARD_LINGER: Duration = Duration::from_secs(4);
const BOARD_HELD: Duration = Duration::from_secs(20);

// ------------------------------------------------------------------------------ sources ----

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Picture(PathBuf),
    Video(PathBuf),
    /// A folder's pictures, in name order.
    Slides(Vec<PathBuf>),
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

fn is_picture(path: &Path) -> bool {
    PICTURES.contains(&extension(path).as_str())
}

/// What `path` is as a background, or why it cannot be one, in his words.
pub fn source_of(path: &Path) -> Result<Source, String> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path).map_err(|e| format!("I cannot read that folder, Sir: {e}."))?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file() && is_picture(p)).collect();
        files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        return if files.is_empty() { Err("There is no PNG, JPEG or WebP picture in that folder, Sir.".into()) } else { Ok(Source::Slides(files)) };
    }
    if !path.is_file() {
        return Err(format!("I cannot find {}, Sir.", path.display()));
    }
    match extension(path).as_str() {
        e if PICTURES.contains(&e) => Ok(Source::Picture(path.to_path_buf())),
        "mp4" | "m4v" | "mov" => probe(path).map(|_| Source::Video(path.to_path_buf())),
        e @ ("mkv" | "webm" | "avi" | "wmv" | "flv" | "mpg" | "mpeg" | "gif" | "ogv") => Err(format!("That is a {} file; I can only play H.264 MP4s, Sir.", e.to_uppercase())),
        e => Err(format!("I cannot use a {}file as a background, Sir: pictures (PNG, JPEG, WebP), an H.264 MP4 video, or a folder of pictures.", if e.is_empty() { String::new() } else { format!(".{e} ") })),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// A path as it is said: a real path, a folder by its name ("pictures", "bilder") or a file's
/// name looked for in his Videos, Pictures, Downloads and Desktop folders.
pub fn resolve(said: &str) -> Result<PathBuf, String> {
    let said = said.trim().trim_matches(|c| c == '"' || c == '\'').trim_end_matches(['\\', '/']);
    if Path::new(said).exists() {
        return Ok(PathBuf::from(said));
    }
    let home = home().ok_or("I do not know where your folders are, Sir.")?;
    let folder = |name: &str| home.join(name);
    let lower = said.to_lowercase();
    let known = match lower.trim_start_matches("my ").trim_start_matches("meinen ").trim_start_matches("the ").trim_end_matches(" folder").trim_end_matches("-ordner").trim_end_matches(" ordner") {
        "pictures" | "picture" | "photos" | "bilder" | "fotos" => Some("Pictures"),
        "videos" | "video" | "filme" => Some("Videos"),
        "downloads" | "download" => Some("Downloads"),
        "desktop" | "schreibtisch" => Some("Desktop"),
        _ => None,
    };
    if let Some(name) = known {
        return Ok(folder(name));
    }
    for name in ["Videos", "Pictures", "Downloads", "Desktop"] {
        let found = std::fs::read_dir(folder(name)).ok().and_then(|dir| dir.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == lower) || p.file_stem().is_some_and(|n| n.to_string_lossy().to_lowercase() == lower)));
        if let Some(path) = found {
            return Ok(path);
        }
    }
    Err(format!("I cannot find \"{said}\", Sir. Give me its full path, or a file in your Videos, Pictures, Downloads or Desktop folder."))
}

/// What the tracks of an MP4 hold that this module plays.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    video: u32,
    pub size: (u32, u32),
    pub fps: f64,
    frames: u32,
    /// The AAC track: (its id, sample rate, channels, the two bytes of its set-up).
    audio: Option<(u32, u32, u16, [u8; 2])>,
}

fn open_mp4(path: &Path) -> Result<mp4::Mp4Reader<std::io::BufReader<std::fs::File>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("I cannot open that video, Sir: {e}."))?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).map_err(|e| format!("That is no MP4 video I can read, Sir: {e}."))
}

/// Reads an MP4's header: Ok when it holds H.264 video, else the reason.
pub fn probe(path: &Path) -> Result<VideoInfo, String> {
    let reader = open_mp4(path)?;
    let (mut video, mut other, mut audio) = (None, None, None);
    for (id, track) in reader.tracks() {
        match track.media_type() {
            Ok(mp4::MediaType::H264) => video = video.or(Some((*id, track))),
            Ok(mp4::MediaType::H265) => other = Some("HEVC"),
            Ok(mp4::MediaType::VP9) => other = Some("VP9"),
            Ok(mp4::MediaType::AAC) => {
                if let (Ok(profile), Ok(freq), Ok(chans)) = (track.audio_profile(), track.sample_freq_index(), track.channel_config()) {
                    let word = ((profile as u16) << 11) | ((freq as u16) << 7) | ((chans as u16) << 3);
                    audio = audio.or(Some((*id, freq.freq(), chans as u16, word.to_be_bytes())));
                }
            }
            _ => {}
        }
    }
    let Some((id, track)) = video else {
        return Err(match other {
            Some(codec) => format!("That video is {codec}; I can only play H.264 MP4s, Sir."),
            None => "There is no H.264 video in that file; I can only play H.264 MP4s, Sir.".into(),
        });
    };
    let secs = track.duration().as_secs_f64();
    let fps = if secs > 0.0 { track.sample_count() as f64 / secs } else { 0.0 };
    if !(1.0..=240.0).contains(&fps) || track.width() == 0 || track.height() == 0 {
        return Err("I cannot tell the frame rate of that video, Sir.".into());
    }
    Ok(VideoInfo { video: id, size: (track.width() as u32, track.height() as u32), fps, frames: track.sample_count(), audio })
}

// --------------------------------------------------------------------------------- rules ----

/// The mode in words: "behind" (the board stands over the media) or "whole" (the media alone,
/// the board when called).
fn mode_word(word: &str) -> Option<&'static str> {
    match word.trim().to_lowercase().as_str() {
        "behind" | "behind the board" | "only the black" | "board" | "dimmed" | "hinter dem board" => Some("behind"),
        "whole" | "whole board" | "whole background" | "full" | "fill" | "everything" | "ganz" | "ganzer hintergrund" => Some("whole"),
        _ => None,
    }
}

/// A monitor as it is said ("2", "second", "all") as its key.
fn monitor_key(word: &str) -> Option<String> {
    let w = word.trim().to_lowercase();
    match w.as_str() {
        "all" | "every" | "both" | "alle" | "" => Some(ALL.into()),
        "first" | "primary" | "main" | "erste" | "ersten" => Some("1".into()),
        "second" | "zweite" | "zweiten" => Some("2".into()),
        "third" | "dritte" | "dritten" => Some("3".into()),
        n if n.parse::<usize>().is_ok_and(|n| (1..=16).contains(&n)) => Some(n.into()),
        _ => None,
    }
}

/// The picture or video for monitor `index`: its own source, else the one for all.
fn source_for<'a>(media: &'a BTreeMap<String, String>, index: usize) -> Option<&'a str> {
    media.get(&index.to_string()).or_else(|| media.get(ALL)).map(String::as_str).filter(|p| !p.is_empty())
}

/// The slide shown, the one after it and how far the cross-fade to it has come (0 to 1), `elapsed`
/// seconds into a slideshow of `n` pictures that changes every `interval` seconds. The fade
/// takes the last `fade` seconds of each picture's time; a single picture never changes.
pub fn slide_at(elapsed: f32, interval: f32, fade: f32, n: usize) -> (usize, usize, f32) {
    if n < 2 || interval <= 0.0 {
        return (0, 0, 0.0);
    }
    let slot = (elapsed.max(0.0) / interval).floor();
    let into = elapsed.max(0.0) - slot * interval;
    let cur = slot as usize % n;
    let start = interval - fade.clamp(0.0, interval);
    let mix = if fade > 0.0 && into > start { ((into - start) / fade).clamp(0.0, 1.0) } else { 0.0 };
    (cur, (cur + 1) % n, mix)
}

fn fade_for(interval: f32) -> f32 {
    FADE_SECS.min(interval / 3.0)
}

/// The part of a picture of `src` pixels that fills `dst` with no bars (cover-fit), as a UV
/// rect: the middle of the picture, cut on the long side.
pub fn cover_uv(src: (f32, f32), dst: (f32, f32)) -> Rect {
    if src.0 <= 0.0 || src.1 <= 0.0 || dst.0 <= 0.0 || dst.1 <= 0.0 {
        return Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    }
    let scale = (dst.0 / src.0).max(dst.1 / src.1);
    let (u, v) = (dst.0 / (src.0 * scale), dst.1 / (src.1 * scale));
    Rect::from_min_max(pos2((1.0 - u) / 2.0, (1.0 - v) / 2.0), pos2((1.0 + u) / 2.0, (1.0 + v) / 2.0))
}

/// The size a picture or frame of `src` is kept at for a monitor of `monitor` pixels: no larger
/// than what covers the monitor, never enlarged, both sides even.
pub fn kept_size(src: (u32, u32), monitor: (u32, u32)) -> (u32, u32) {
    let scale = (monitor.0 as f32 / src.0.max(1) as f32).max(monitor.1 as f32 / src.1.max(1) as f32).min(1.0);
    let even = |n: u32| ((n as f32 * scale).round() as u32).max(2) & !1;
    (even(src.0), even(src.1))
}

/// Whether the decoders idle: something fills the screen, the PC is locked, or the background
/// is not drawn (`unseen`).
pub fn paused(game: bool, locked: bool, unseen: bool) -> bool {
    game || locked || unseen
}

/// Where the monitors lie in a board window at `window` (screen points): each monitor's
/// number and its rect in window points (the part the window reaches).
pub fn rects_in(window: Rect, ppp: f32, monitors: &[crate::monitors::Monitor]) -> Vec<(usize, Rect)> {
    let inside = Rect::from_min_size(pos2(0.0, 0.0), window.size());
    monitors
        .iter()
        .filter_map(|m| {
            let [l, t, r, b] = m.rect;
            let rect = Rect::from_min_max(pos2(l as f32, t as f32) / ppp, pos2(r as f32, b as f32) / ppp).translate(-window.min.to_vec2()).intersect(inside);
            (rect.width() > 1.0 && rect.height() > 1.0).then_some((m.index, rect))
        })
        .collect()
}

// ------------------------------------------------------------------------------ settings ----

/// The three settings that decide what is drawn, as the HUD last published them.
#[derive(Clone, Default, PartialEq)]
struct Look {
    whole: bool,
    media: BTreeMap<String, String>,
    interval: u32,
    sound: bool,
    dim: f32,
}

static LOOK: Mutex<Option<Look>> = Mutex::new(None);
/// How much of the board shows (whole mode): 1 board, 0 media alone.
static BOARD: Mutex<(f32, Option<Instant>)> = Mutex::new((1.0, None));
static CALLED: Mutex<Option<Instant>> = Mutex::new(None);

/// "Show the board": in whole mode the board comes for a while.
pub fn call_board() {
    *lock(&CALLED) = Some(Instant::now());
    crate::gui::refresh();
}

/// Whether there is media and it is the whole background (the board waits to be called).
pub fn whole_active() -> bool {
    lock(&LOOK).as_ref().is_some_and(|l| l.whole && !l.media.is_empty())
}

/// Whether the board takes the pointer: not while the media alone is shown.
pub fn board_shown() -> bool {
    !whole_active() || lock(&BOARD).0 > 0.3
}

/// From every frame of the board: the settings, and whether he is called (he listens, thinks,
/// speaks, or a window of his is open). Moves the board's fade.
pub fn publish(iface: &InterfaceSettings, called: bool) {
    static STAMP: Mutex<Option<Instant>> = Mutex::new(None);
    let dt = lock(&STAMP).replace(Instant::now()).map_or(0.0, |at| at.elapsed().as_secs_f32());
    publish_dt(iface, called, dt);
}

fn publish_dt(iface: &InterfaceSettings, called: bool, dt: f32) {
    let look = Look { whole: iface.bg_mode == "whole", media: iface.bg_media.clone(), interval: iface.bg_interval, sound: iface.bg_sound, dim: iface.bg_dim };
    let now = Instant::now();
    let mut board = lock(&BOARD);
    let held = lock(&CALLED).is_some_and(|at| at.elapsed() < BOARD_HELD);
    if called {
        board.1 = Some(now);
    }
    let want = if !look.whole || look.media.is_empty() || held || board.1.is_some_and(|at| at.elapsed() < BOARD_LINGER) { 1.0 } else { 0.0 };
    board.0 = crate::holo_motion::approach(board.0, want, dt, 0.35);
    *lock(&LOOK) = Some(look);
}

/// Everything to know to set a source: from a typed or spoken path, the settings with it set
/// for monitor `key` (`ALL` or its number), or with it removed when `said` is empty or "none".
pub fn set_media(s: &mut InterfaceSettings, key: &str, said: &str) -> Result<String, String> {
    let where_ = if key == ALL { "every monitor".to_string() } else { format!("monitor {key}") };
    if said.trim().is_empty() || matches!(said.trim().to_lowercase().as_str(), "none" | "off" | "nothing" | "remove" | "keine" | "kein") {
        return match s.bg_media.remove(key) {
            Some(_) => Ok(format!("no background media on {where_}")),
            None if key == ALL => Err("There is no background picture or video to remove, Sir.".into()),
            None => Err(format!("{where_} has no background of its own, Sir.")),
        };
    }
    let path = resolve(said)?;
    let kind = match source_of(&path)? {
        Source::Picture(_) => "picture",
        Source::Video(_) => "video",
        Source::Slides(files) => return Ok(insert(s, key, &path, format!("a slideshow of {} pictures on {where_}", files.len()))),
    };
    Ok(insert(s, key, &path, format!("the {kind} {} on {where_}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())))
}

fn insert(s: &mut InterfaceSettings, key: &str, path: &Path, line: String) -> String {
    s.bg_media.insert(key.to_string(), path.to_string_lossy().into_owned());
    format!("background {line}")
}

/// `configure_interface`'s background arguments: the media (`background_media`, for the monitor
/// in `background_monitor`), `background_mode`, `background_interval` (seconds),
/// `background_sound` and `background_dim` (a share or percent).
pub fn apply(s: &mut InterfaceSettings, args: &Value, done: &mut Vec<String>, failed: &mut Vec<String>) {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let number = |key: &str| args.get(key).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())));
    if let Some(word) = text("background_mode") {
        match mode_word(word) {
            Some(mode) => {
                s.bg_mode = mode.into();
                done.push(if mode == "whole" { "the media fills the whole background, the board when called".to_string() } else { "the media only behind the board".to_string() });
            }
            None => failed.push(format!("unknown background mode '{word}' (behind the board, or the whole background)")),
        }
    }
    if let Some(n) = number("background_interval") {
        s.bg_interval = (n.round() as u32).clamp(3, 3600);
        done.push(format!("slideshow every {} seconds", s.bg_interval));
    }
    if let Some(on) = args.get("background_sound").and_then(Value::as_bool) {
        s.bg_sound = on;
        done.push(format!("background video sound {}", if on { "on" } else { "off" }));
    }
    if let Some(n) = number("background_dim") {
        s.bg_dim = (if n > 1.0 { n / 100.0 } else { n }).clamp(0.0, 0.9) as f32;
        done.push(format!("media dimmed by {:.0} %", s.bg_dim * 100.0));
    }
    if let Some(said) = text("background_media") {
        let key = match text("background_monitor") {
            None => Some(ALL.to_string()),
            Some(word) => monitor_key(word),
        };
        match key {
            Some(key) => match set_media(s, &key, said) {
                Ok(line) => done.push(line),
                Err(e) => failed.push(e),
            },
            None => failed.push("I do not know that monitor".into()),
        }
    }
}

// ------------------------------------------------------------------------------- phrases ----

static SET_VERB: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:set|use|make|put|change|show|play|load|take|nimm|nutze|setz|setze|stell|stelle|verwende|benutze|mach|zeig|spiel|spiele|lade)\b").expect("valid regex"));
static BACKGROUND: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:background|wallpaper|backdrop|hintergrund\w*|slideshow|diashow)\b").expect("valid regex"));
static QUOTED: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#""([^"]+)"|'([^']{3,})'"#).expect("valid regex"));
static PATH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)((?:[a-z]:[\\/]|/)[^"]*?)(?:\s+(?:as|on|with|without|for|every|and|then|muted|auf|als|mit|ohne|alle|und)\b|\s*[.!?]?\s*$)"#).expect("valid regex")
});
static FILE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)\b([\w-]+\.(?:png|jpe?g|webp|mp4|m4v|mov|mkv|webm|avi))\b").expect("valid regex"));
static FOLDER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:my |meinem |meinen |dem |the )?(pictures|photos|videos|downloads|desktop|bilder|fotos|schreibtisch)(?:[ -](?:folder|ordner))?\b").expect("valid regex")
});
static MONITOR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:(?:on|auf|for|für)\s+(?:my |the |all |every |meinem |dem |den |allen )?(?:(first|second|third|primary|main|all|every|erste[nr]?|zweite[nr]?|dritte[nr]?|alle[nr]?)\s+)?(?:monitors?|screens?|displays?|bildschirm\w*)(?:\s+(\d))?)").expect("valid regex")
});
static EVERY: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:every|alle)\s+(\d+)\s*(seconds?|secs?|minutes?|mins?|sekunden?|minuten?)\b").expect("valid regex"));

/// Spoken (or typed) orders for the background media → `configure_interface` arguments. Reads
/// the sentence as said, because a path has no place in the normalised words.
pub fn command(text: &str) -> Option<Value> {
    let text = text.trim();
    if text.is_empty() || text.len() > 400 {
        return None;
    }
    let lower = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let mut args = serde_json::Map::new();
    let monitor = MONITOR.captures(text).map(|c| c.get(2).or(c.get(1)).map_or(ALL.to_string(), |m| m.as_str().to_lowercase()));
    let monitor_arg = |args: &mut serde_json::Map<String, Value>| {
        if let Some(m) = &monitor {
            args.insert("background_monitor".into(), json!(m));
        }
    };
    let video_word = has(&["video", "clip", "film"]);
    let sound = if has(&["without sound", "no sound", "muted", "silent", "ohne ton", "ohne sound", "mute", "stumm"]) {
        Some(false)
    } else if has(&["with sound", "with audio", "mit ton", "mit sound", "sound on", "ton an", "unmute", "with its sound"]) {
        Some(true)
    } else {
        None
    };
    let every = EVERY.captures(text).and_then(|c| {
        let n: u32 = c[1].parse().ok()?;
        Some(if c[2].to_lowercase().starts_with('m') { n * 60 } else { n })
    });
    if has(&["slideshow", "diashow", "slide show"]) || every.is_some() {
        if let Some(n) = every {
            args.insert("background_interval".into(), json!(n));
        }
    }
    // A source: a path (quoted, or from a drive letter), a folder by its name, or a file name.
    let source = QUOTED
        .captures(text)
        .map(|c| c.get(1).or(c.get(2)).map_or(String::new(), |m| m.as_str().to_string()))
        .or_else(|| PATH.captures(text).map(|c| c[1].trim().to_string()))
        .or_else(|| FILE.captures(text).map(|c| c[1].to_string()))
        .or_else(|| (has(&["slideshow", "diashow", "folder", "ordner"]) || BACKGROUND.is_match(text)).then(|| FOLDER.captures(text).map(|c| c[1].to_lowercase())).flatten());
    let background = BACKGROUND.is_match(text);
    if let Some(source) = source.filter(|_| background && SET_VERB.is_match(text) && !has(&["how do", "how can", "can you", "could you", "what is", "?"]) || has(&["slideshow", "diashow"]) && !has(&["?"])) {
        args.insert("background_media".into(), json!(source));
        monitor_arg(&mut args);
    } else if background && has(&["remove", "clear", "delete", "reset", "no more", "get rid", "entfern", "lösch", "kein ", "ohne"]) && has(&["picture", "image", "photo", "video", "slideshow", "media", "clip", "bild", "diashow", "wallpaper"]) && !has(&["?"]) {
        args.insert("background_media".into(), json!("none"));
        monitor_arg(&mut args);
    }
    if !args.contains_key("background_media") && args.len() <= 1 {
        // Mode, sound, the board: orders of their own (no source in them).
        if has(&["behind the board", "hinter dem board", "only behind", "nur hinter"]) {
            args.insert("background_mode".into(), json!("behind"));
        } else if has(&["whole background", "entire background", "fill the whole", "fill the background", "ganzen hintergrund", "ganze hintergrund", "ganzen bildschirm", "whole screen with", "media alone", "only the media", "only the picture", "only the video"]) {
            args.insert("background_mode".into(), json!("whole"));
        }
    }
    if let Some(on) = sound.filter(|_| background || video_word) {
        args.insert("background_sound".into(), json!(on));
    }
    if args.is_empty() && whole_active() && has(&["show the board", "show your board", "bring up the board", "board anzeigen", "zeig das board", "zeige das board", "show the hud", "show me the board"]) {
        args.insert("show_board".into(), json!(true));
    }
    (!args.is_empty() && !(args.len() == 1 && args.contains_key("background_monitor"))).then_some(Value::Object(args))
}

// ------------------------------------------------------------------------------- decoding ----

/// One picture or frame, kept at (at most) the monitor's size.
pub struct Img {
    id: u64,
    size: (u32, u32),
    rgba: Vec<u8>,
}

fn new_img(size: (u32, u32), rgba: Vec<u8>) -> Arc<Img> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    Arc::new(Img { id: NEXT.fetch_add(1, Ordering::Relaxed), size, rgba })
}

#[derive(Default)]
struct Layers {
    a: Option<Arc<Img>>,
    b: Option<Arc<Img>>,
    /// How far `b` has faded over `a` (0 to 1).
    mix: f32,
}

struct Shared {
    layers: Mutex<Layers>,
    paused: AtomicBool,
    stop: AtomicBool,
    /// When the HUD last drew it (ms since `clock`).
    seen: AtomicU64,
}

fn now_ms() -> u64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_millis() as u64
}

impl Shared {
    fn idle(&self) -> bool {
        paused(self.paused.load(Ordering::Relaxed), false, now_ms().saturating_sub(self.seen.load(Ordering::Relaxed)) > STALE_MS)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Sleeps `time` in short steps; false when stopped meanwhile.
    fn sleep(&self, time: Duration) -> bool {
        let end = Instant::now() + time;
        while Instant::now() < end {
            if self.stopped() {
                return false;
            }
            std::thread::sleep((end - Instant::now()).min(Duration::from_millis(50)));
        }
        !self.stopped()
    }

    fn show(&self, ctx: &egui::Context, a: Option<Arc<Img>>, b: Option<Arc<Img>>, mix: f32) {
        *lock(&self.layers) = Layers { a, b, mix };
        ctx.request_repaint();
    }
}

fn load_image(path: &Path, monitor: (u32, u32)) -> Option<Arc<Img>> {
    let img = match image::open(path) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!("[BACKGROUND] {} cannot be read: {e}", path.display());
            return None;
        }
    };
    let (w, h) = (img.width(), img.height());
    let size = kept_size((w, h), monitor);
    let rgba = if size == (w, h) { img.to_rgba8() } else { image::imageops::resize(&img.to_rgba8(), size.0, size.1, image::imageops::FilterType::Triangle) };
    Some(new_img(size, rgba.into_raw()))
}

/// A still picture, or a folder's pictures with their cross-fades: its own clock, held while
/// the background cannot be seen.
fn run_slides(sh: Arc<Shared>, ctx: egui::Context, files: Vec<PathBuf>, monitor: (u32, u32), interval: f32) {
    let fade = fade_for(interval);
    let mut cache: HashMap<usize, Option<Arc<Img>>> = HashMap::new();
    let mut at = 0.0f32;
    let mut last = Instant::now();
    let mut shown: Option<(Option<u64>, Option<u64>, i32)> = None;
    while !sh.stopped() {
        if sh.idle() {
            sh.sleep(Duration::from_millis(200));
            last = Instant::now();
            continue;
        }
        at += last.elapsed().as_secs_f32();
        last = Instant::now();
        let (cur, next, mix) = slide_at(at, interval, fade, files.len());
        if cache.len() > 3 {
            cache.retain(|k, _| *k == cur || *k == next);
        }
        let mut get = |k: usize| cache.entry(k).or_insert_with(|| load_image(&files[k], monitor)).clone();
        let (a, b) = (get(cur), if mix > 0.0 { get(next) } else { None });
        let key = (a.as_ref().map(|i| i.id), b.as_ref().map(|i| i.id), (mix * 60.0) as i32);
        if shown != Some(key) {
            shown = Some(key);
            sh.show(&ctx, a, b, if b_present(&key) { mix } else { 0.0 });
        }
        if files.len() < 2 {
            return;
        }
        let wait = if mix > 0.0 { 1.0 / 30.0 } else { (interval - fade - at.rem_euclid(interval)).clamp(0.05, 0.25) };
        sh.sleep(Duration::from_secs_f32(wait));
    }
}

fn b_present(key: &(Option<u64>, Option<u64>, i32)) -> bool {
    key.1.is_some()
}

/// An H.264 NAL stream from an MP4 sample: its length-prefixed units (four bytes long, as
/// nearly every MP4 has them) become start-code units, with the parameter sets in front of a
/// key frame. shortcut: a two- or three-byte length prefix is not read, find it in the avcC
/// box if such a file turns up.
fn annex_b(sample: &[u8], key: bool, sps: &[u8], pps: &[u8]) -> Vec<u8> {
    const START: [u8; 4] = [0, 0, 0, 1];
    let mut out = Vec::with_capacity(sample.len() + 64);
    if key {
        for set in [sps, pps] {
            out.extend_from_slice(&START);
            out.extend_from_slice(set);
        }
    }
    let mut at = 0;
    while at + 4 <= sample.len() {
        let len = u32::from_be_bytes([sample[at], sample[at + 1], sample[at + 2], sample[at + 3]]) as usize;
        at += 4;
        let Some(unit) = sample.get(at..at + len) else { break };
        out.extend_from_slice(&START);
        out.extend_from_slice(unit);
        at += len;
    }
    out
}

/// A decoded frame as RGBA at `size` (nearest neighbour when it is smaller than the frame).
fn frame_rgba(yuv: &openh264::decoder::DecodedYUV<'_>, size: (u32, u32)) -> Vec<u8> {
    use openh264::formats::YUVSource;
    let (w, h) = yuv.dimensions();
    let mut out = vec![255u8; size.0 as usize * size.1 as usize * 4];
    if (w as u32, h as u32) == size {
        yuv.write_rgba8(&mut out);
        return out;
    }
    let ((ys, us, vs), (yp, up, vp)) = (yuv.strides(), (yuv.y(), yuv.u(), yuv.v()));
    for oy in 0..size.1 as usize {
        let sy = oy * h / size.1 as usize;
        for ox in 0..size.0 as usize {
            let sx = ox * w / size.0 as usize;
            let y = (yp[sy * ys + sx] as i32 - 16).max(0) * 298;
            let (u, v) = (up[(sy / 2) * us + sx / 2] as i32 - 128, vp[(sy / 2) * vs + sx / 2] as i32 - 128);
            let px = &mut out[(oy * size.0 as usize + ox) * 4..][..3];
            px[0] = ((y + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            px[1] = ((y - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            px[2] = ((y + 516 * u + 128) >> 8).clamp(0, 255) as u8;
        }
    }
    out
}

/// A video, looped: frames on their own clock (the clock stops while the background cannot be
/// seen); a frame later than `LATE` is skipped, and no more than `MAX_FPS` are shown.
fn run_video(sh: Arc<Shared>, ctx: egui::Context, path: PathBuf, monitor: (u32, u32), sound: bool) {
    let Ok(info) = probe(&path) else { return };
    let Ok(mut reader) = open_mp4(&path) else { return };
    let (sps, pps) = match reader.tracks().get(&info.video).map(|t| (t.sequence_parameter_set().map(<[u8]>::to_vec), t.picture_parameter_set().map(<[u8]>::to_vec))) {
        Some((Ok(sps), Ok(pps))) => (sps, pps),
        _ => return,
    };
    if let (true, Some(audio)) = (sound, info.audio) {
        let (sh, path) = (sh.clone(), path.clone());
        std::thread::Builder::new().name("jai-bg-audio".into()).spawn(move || run_audio(sh, path, audio)).ok();
    }
    let size = kept_size(info.size, monitor);
    let every = (info.fps / MAX_FPS).ceil().max(1.0) as u32;
    let step = Duration::from_secs_f64(every as f64 / info.fps);
    while !sh.stopped() {
        let Ok(mut decoder) = openh264::decoder::Decoder::new() else { return };
        let (mut begin, mut shown) = (Instant::now(), 0u32);
        for id in 1..=info.frames {
            // (The clock stands still while he is not seen.)
            let mut stood = None;
            while sh.idle() && !sh.stopped() {
                stood.get_or_insert_with(Instant::now);
                std::thread::sleep(Duration::from_millis(100));
            }
            if let Some(since) = stood {
                begin += since.elapsed();
            }
            if sh.stopped() {
                return;
            }
            let Ok(Some(sample)) = reader.read_sample(info.video, id) else { continue };
            let packet = annex_b(&sample.bytes, sample.is_sync, &sps, &pps);
            let Ok(Some(yuv)) = decoder.decode(&packet) else { continue };
            if (id - 1) % every != 0 {
                continue;
            }
            let due = begin + step * shown;
            shown += 1;
            if Instant::now() > due + LATE {
                continue;
            }
            if !sh.sleep(due.saturating_duration_since(Instant::now())) {
                return;
            }
            sh.show(&ctx, Some(new_img(size, frame_rgba(&yuv, size))), None, 0.0);
        }
    }
}

/// The sound of a video: its AAC track decoded into a ring that the sound card's callback reads
/// (resampled, as stereo), the track looped. The video's clock and this one start together; a
/// loop can drift by a frame or two. shortcut: f32 output only and no volume setting (the mixer
/// has one); a sound device that wants another format plays nothing, the video goes on.
fn run_audio(sh: Arc<Shared>, path: PathBuf, (track, rate, chans, asc): (u32, u32, u16, [u8; 2])) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use symphonia::core::audio::{Channels, SampleBuffer};
    use symphonia::core::codecs::{CodecParameters, DecoderOptions, CODEC_TYPE_AAC};
    use symphonia::core::formats::Packet;
    let layout = match chans {
        1 => Channels::FRONT_CENTRE,
        2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
        _ => return,
    };
    let Some(device) = cpal::default_host().default_output_device() else { return };
    let Ok(device_config) = device.default_output_config() else { return };
    if device_config.sample_format() != cpal::SampleFormat::F32 {
        return;
    }
    let out_chans = device_config.channels() as usize;
    let ratio = rate as f64 / device_config.sample_rate().0 as f64;
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let (play, quiet) = (ring.clone(), sh.clone());
    let (mut prev, mut cur, mut pos) = ([0.0f32; 2], [0.0f32; 2], 0.0f64);
    let stream = device.build_output_stream(
        &device_config.config(),
        move |out: &mut [f32], _| {
            if quiet.paused.load(Ordering::Relaxed) {
                out.fill(0.0);
                return;
            }
            let mut q = lock(&play);
            for frame in out.chunks_mut(out_chans) {
                pos += ratio;
                while pos >= 1.0 {
                    pos -= 1.0;
                    prev = cur;
                    if q.len() >= 2 {
                        cur = [q.pop_front().unwrap_or(0.0), q.pop_front().unwrap_or(0.0)];
                    }
                }
                let at = pos as f32;
                let (l, r) = (prev[0] + (cur[0] - prev[0]) * at, prev[1] + (cur[1] - prev[1]) * at);
                if frame.len() == 1 {
                    frame[0] = (l + r) / 2.0;
                } else {
                    frame[0] = l;
                    frame[1] = r;
                    frame[2..].fill(0.0);
                }
            }
        },
        |e| tracing::warn!("[BACKGROUND] sound: {e}"),
        None,
    );
    let Ok(stream) = stream else { return };
    if stream.play().is_err() {
        return;
    }
    let mut params = CodecParameters::new();
    params.for_codec(CODEC_TYPE_AAC).with_sample_rate(rate).with_channels(layout).with_extra_data(Box::new(asc));
    let Ok(mut reader) = open_mp4(&path) else { return };
    let frames = reader.tracks().get(&track).map_or(0, |t| t.sample_count());
    let (mut id, mut ts) = (1u32, 0u64);
    let Ok(mut decoder) = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()) else { return };
    let mut samples: Option<SampleBuffer<f32>> = None;
    while !sh.stopped() {
        // (A second of sound ahead is plenty; the ring stays short.)
        if lock(&ring).len() > rate as usize * 2 || sh.paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if id > frames {
            id = 1;
            ts = 0;
            match symphonia::default::get_codecs().make(&params, &DecoderOptions::default()) {
                Ok(fresh) => decoder = fresh,
                Err(_) => return,
            }
            continue;
        }
        let Ok(Some(sample)) = reader.read_sample(track, id) else {
            id += 1;
            continue;
        };
        id += 1;
        ts += sample.duration as u64;
        let Ok(decoded) = decoder.decode(&Packet::new_from_slice(track, ts, sample.duration as u64, &sample.bytes)) else { continue };
        let buf = samples.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, *decoded.spec()));
        buf.copy_interleaved_ref(decoded);
        let mut q = lock(&ring);
        for frame in buf.samples().chunks(chans as usize) {
            q.push_back(frame[0]);
            q.push_back(*frame.get(1).unwrap_or(&frame[0]));
        }
    }
}

// ------------------------------------------------------------------------------- painting ----

struct Player {
    /// What it plays: (the path, the monitor's pixels, the interval, the sound).
    what: (String, (u32, u32), u32, bool),
    shared: Arc<Shared>,
    /// The textures of the two layers, each with the picture it holds.
    textures: [Option<(u64, TextureHandle)>; 2],
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

static PLAYERS: Mutex<Option<HashMap<usize, Player>>> = Mutex::new(None);

fn start(ctx: &egui::Context, what: &(String, (u32, u32), u32, bool)) -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared { layers: Mutex::new(Layers::default()), paused: AtomicBool::new(false), stop: AtomicBool::new(false), seen: AtomicU64::new(now_ms()) });
    let source = match source_of(Path::new(&what.0)) {
        Ok(source) => source,
        Err(e) => {
            tracing::warn!("[BACKGROUND] {e}");
            return None;
        }
    };
    let (sh, ctx, monitor, interval, sound) = (shared.clone(), ctx.clone(), what.1, what.2 as f32, what.3);
    let job = std::thread::Builder::new().name("jai-bg-media".into());
    match source {
        Source::Picture(path) => job.spawn(move || run_slides(sh, ctx, vec![path], monitor, interval)),
        Source::Slides(files) => job.spawn(move || run_slides(sh, ctx, files, monitor, interval)),
        Source::Video(path) => job.spawn(move || run_video(sh, ctx, path, monitor, sound)),
    }
    .ok()?;
    Some(shared)
}

/// Whether something fills the screen or the PC is locked: asked at most every two seconds.
fn blocked() -> bool {
    static SEEN: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut seen = lock(&SEEN);
    if let Some((at, now)) = *seen {
        if at.elapsed() < Duration::from_secs(2) {
            return now;
        }
    }
    let now = !crate::platform::in_test_session() && !cfg!(test) && paused(crate::hud_feel::game_in_front() || crate::vram_guard::in_game(), crate::away_mode::desktop_locked(), false);
    *seen = Some((Instant::now(), now));
    now
}

/// Starts, changes and stops the players to match the settings, uploads what they decoded.
/// `rects`: each monitor the window reaches with its place in the window (points).
fn tick(ctx: &egui::Context, look: &Look, rects: &[(usize, Rect)]) {
    let mut players = lock(&PLAYERS);
    let players = players.get_or_insert_with(HashMap::new);
    players.retain(|index, _| rects.iter().any(|(i, _)| i == index) && source_for(&look.media, *index).is_some());
    let stood = blocked();
    for (index, rect) in rects {
        let Some(path) = source_for(&look.media, *index) else { continue };
        let ppp = ctx.pixels_per_point();
        let what = (path.to_string(), ((rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32), look.interval.max(3), look.sound);
        if players.get(index).is_none_or(|p| p.what != what) {
            players.remove(index);
            if let Some(shared) = start(ctx, &what) {
                players.insert(*index, Player { what, shared, textures: [None, None] });
            }
        }
        let Some(player) = players.get_mut(index) else { continue };
        player.shared.paused.store(stood, Ordering::Relaxed);
        player.shared.seen.store(now_ms(), Ordering::Relaxed);
        let layers = lock(&player.shared.layers);
        for (slot, img) in [(0, &layers.a), (1, &layers.b)] {
            match img {
                Some(img) if player.textures[slot].as_ref().is_none_or(|(id, _)| *id != img.id) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([img.size.0 as usize, img.size.1 as usize], &img.rgba);
                    player.textures[slot] = Some((img.id, ctx.load_texture(format!("jai_bg_{index}_{slot}"), image, TextureOptions::LINEAR)));
                }
                None => player.textures[slot] = None,
                _ => {}
            }
        }
    }
}

/// Draws the media on each monitor's rect: cover-fit (the picture's middle, no bars), `fade`
/// of it, then the black veil `dim`.
fn draw(painter: &egui::Painter, rects: &[(usize, Rect)], dim: f32, fade: f32) {
    let players = lock(&PLAYERS);
    let Some(players) = players.as_ref() else { return };
    let void = crate::holo::theme().void();
    for (index, rect) in rects {
        let Some(player) = players.get(index) else { continue };
        let mix = lock(&player.shared.layers).mix;
        let mut any = false;
        for (slot, alpha) in [(0, fade), (1, fade * mix)] {
            if let Some((_, texture)) = &player.textures[slot] {
                let size = texture.size_vec2();
                painter.image(texture.id(), *rect, cover_uv((size.x, size.y), (rect.width(), rect.height())), Color32::WHITE.gamma_multiply(alpha));
                any = true;
            }
        }
        if any && dim > 0.0 {
            painter.rect_filled(*rect, egui::Rounding::ZERO, crate::holo::a(void, dim * fade));
        }
    }
}

/// The media under the board (`gui::paint_canvas`, over the black): in whole mode dimmed only
/// as far as the board shows.
pub fn paint(ctx: &egui::Context, painter: &egui::Painter, rects: &[(usize, Rect)]) {
    let Some(look) = lock(&LOOK).clone() else { return };
    if look.media.is_empty() {
        *lock(&PLAYERS) = None;
        return;
    }
    tick(ctx, &look, rects);
    let board = lock(&BOARD).0;
    draw(painter, rects, if look.whole { look.dim * board } else { look.dim }, 1.0);
    if look.whole && board > 0.0 && board < 1.0 {
        ctx.request_repaint();
    }
}

/// In whole mode, over the board's whole picture: the media again, as far as the board is not
/// shown. (The board's keys under it take no click: `board_shown`.)
pub fn curtain(ctx: &egui::Context, rects: &[(usize, Rect)]) {
    let Some(look) = lock(&LOOK).clone() else { return };
    let board = lock(&BOARD).0;
    if look.whole && !look.media.is_empty() && board < 1.0 {
        draw(&ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("jai_bg_curtain"))), rects, 0.0, 1.0 - board);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::Monitor;
    use eframe::egui::vec2;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jai_bg_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn picture(path: &Path, size: (u32, u32), colour: [u8; 3]) {
        image::RgbImage::from_pixel(size.0, size.1, image::Rgb(colour)).save(path).expect("saves");
    }

    #[test]
    fn a_picture_covers_its_monitor_with_no_bars() {
        let uv = |src: (f32, f32), dst: (f32, f32)| cover_uv(src, dst);
        // Wider than the monitor: the sides are cut, the middle stays.
        let wide = uv((4000.0, 1000.0), (1920.0, 1080.0));
        assert!((wide.height() - 1.0).abs() < 1e-5 && wide.width() < 1.0 && (wide.center().x - 0.5).abs() < 1e-5);
        // A portrait monitor: the sides are cut, all the height stays.
        let tall = uv((1000.0, 1000.0), (1080.0, 1920.0));
        assert!((tall.height() - 1.0).abs() < 1e-5 && tall.width() < 1.0);
        // A tall picture on a wide monitor: top and bottom are cut.
        let high = uv((1000.0, 3000.0), (1920.0, 1080.0));
        assert!((high.width() - 1.0).abs() < 1e-5 && high.height() < 1.0);
        // The same shape: all of it.
        assert_eq!(uv((1920.0, 1080.0), (1280.0, 720.0)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
        // What is shown has the monitor's shape.
        let shown = uv((3000.0, 2000.0), (1000.0, 1000.0));
        assert!(((shown.width() * 3000.0) / (shown.height() * 2000.0) - 1.0).abs() < 1e-4);
        assert_eq!(kept_size((4000, 3000), (1920, 1080)), (1920, 1440));
        assert_eq!(kept_size((800, 600), (1920, 1080)), (800, 600), "never enlarged");
        assert_eq!(kept_size((1921, 1081), (1000, 1000)).0 % 2, 0);
    }

    #[test]
    fn each_monitor_has_its_own_part_of_the_window_and_its_own_source() {
        let monitor = |index: usize, rect: [i32; 4]| Monitor { index, name: String::new(), rect, work: rect, primary: index == 1 };
        let both = [monitor(1, [0, 0, 1920, 1080]), monitor(2, [1920, 0, 3840, 1080])];
        let window = Rect::from_min_max(pos2(0.0, 0.0), pos2(3840.0, 1080.0));
        let rects = rects_in(window, 1.0, &both);
        assert_eq!(rects, vec![(1, Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 1080.0))), (2, Rect::from_min_max(pos2(1920.0, 0.0), pos2(3840.0, 1080.0)))]);
        // At 150 % scaling the points are two thirds of the pixels; a window on one monitor reaches that one only.
        let scaled = rects_in(Rect::from_min_max(pos2(1280.0, 0.0), pos2(2560.0, 720.0)), 1.5, &both);
        assert_eq!(scaled, vec![(2, Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 720.0)))]);
        let media: BTreeMap<String, String> = [("all".into(), "a.png".into()), ("2".into(), "b.mp4".into())].into();
        assert_eq!((source_for(&media, 1), source_for(&media, 2), source_for(&media, 3)), (Some("a.png"), Some("b.mp4"), Some("a.png")));
        assert_eq!(source_for(&BTreeMap::new(), 1), None);
    }

    #[test]
    fn a_slideshow_changes_every_interval_with_a_soft_fade() {
        // Three pictures, 30 s each, the last 1.5 s a cross-fade.
        assert_eq!(slide_at(0.0, 30.0, 1.5, 3), (0, 1, 0.0));
        assert_eq!(slide_at(28.0, 30.0, 1.5, 3), (0, 1, 0.0), "before the fade begins");
        let (cur, next, mix) = slide_at(29.25, 30.0, 1.5, 3);
        assert_eq!((cur, next), (0, 1));
        assert!((mix - 0.5).abs() < 1e-4, "half-way through the fade: {mix}");
        assert_eq!(slide_at(30.0, 30.0, 1.5, 3), (1, 2, 0.0), "the next one stands");
        assert_eq!(slide_at(75.0, 30.0, 1.5, 3).0, 2);
        assert_eq!(slide_at(95.0, 30.0, 1.5, 3).0, 0, "round again");
        assert_eq!(slide_at(29.9, 30.0, 1.5, 1), (0, 0, 0.0), "one picture never changes");
        assert!((fade_for(30.0) - 1.5).abs() < 1e-6 && (fade_for(3.0) - 1.0).abs() < 1e-6, "a short interval fades for a third of it");
        // The mix never leaves 0..1 and never goes back inside one picture's time.
        let mut last = 0.0;
        for k in 0..=3000 {
            let (_, _, mix) = slide_at(k as f32 * 0.01, 30.0, 1.5, 4);
            assert!((0.0..=1.0).contains(&mix) && (mix >= last || mix == 0.0));
            last = mix;
        }
    }

    #[test]
    fn the_decoders_idle_whenever_the_background_cannot_be_seen() {
        assert!(!paused(false, false, false));
        assert!(paused(true, false, false), "a game or fullscreen program in front");
        assert!(paused(false, true, false), "the PC locked");
        assert!(paused(false, false, true), "the window hidden");
        let sh = Shared { layers: Mutex::new(Layers::default()), paused: AtomicBool::new(false), stop: AtomicBool::new(false), seen: AtomicU64::new(now_ms()) };
        assert!(!sh.idle());
        sh.paused.store(true, Ordering::Relaxed);
        assert!(sh.idle());
        sh.paused.store(false, Ordering::Relaxed);
        sh.seen.store(now_ms().saturating_sub(STALE_MS + 500).max(0), Ordering::Relaxed);
        // (The clock may be younger than the staleness: then nothing can be that stale yet.)
        assert_eq!(sh.idle(), now_ms() > STALE_MS + 500);
    }

    #[test]
    fn sources_are_pictures_folders_and_h264_mp4s_and_anything_else_says_why() {
        let d = dir("sources");
        picture(&d.join("b.png"), (8, 8), [10, 20, 30]);
        picture(&d.join("a.jpg"), (8, 8), [30, 20, 10]);
        std::fs::write(d.join("notes.txt"), "x").unwrap();
        assert_eq!(source_of(&d.join("b.png")), Ok(Source::Picture(d.join("b.png"))));
        assert_eq!(source_of(&d), Ok(Source::Slides(vec![d.join("a.jpg"), d.join("b.png")])), "a folder is a slideshow in name order");
        assert!(source_of(&d.join("notes.txt")).unwrap_err().contains(".txt"));
        std::fs::write(d.join("film.mkv"), "x").unwrap();
        assert_eq!(source_of(&d.join("film.mkv")).unwrap_err(), "That is a MKV file; I can only play H.264 MP4s, Sir.");
        assert!(source_of(&d.join("missing.png")).unwrap_err().starts_with("I cannot find"));
        let empty = dir("empty");
        assert!(source_of(&empty).unwrap_err().contains("no PNG"));
        assert!(source_of(&d.join("film.mkv")).is_err());
        // A file that is called an MP4 and is none.
        std::fs::write(d.join("fake.mp4"), "not a video").unwrap();
        assert!(source_of(&d.join("fake.mp4")).unwrap_err().contains("MP4"));
        // Pictures decode at the monitor's size, not their own.
        let big = d.join("big.png");
        picture(&big, (400, 200), [1, 2, 3]);
        let img = load_image(&big, (100, 100)).expect("decodes");
        assert_eq!(img.size, (200, 100));
        assert_eq!(img.rgba.len(), 200 * 100 * 4);
        assert!(load_image(&d.join("notes.txt"), (100, 100)).is_none());
    }

    /// A clip made here: a few H.264 frames from openh264's encoder, in an MP4 of the `mp4` crate.
    fn make_clip(path: &Path, size: (u16, u16), frames: u32) {
        use openh264::encoder::{Encoder, EncoderConfig};
        use openh264::formats::YUVBuffer;
        use openh264::OpenH264API;
        let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), EncoderConfig::new()).expect("encoder");
        let (mut sps, mut pps, mut samples) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..frames {
            let (w, h) = (size.0 as usize, size.1 as usize);
            let mut yuv = vec![128u8; w * h * 3 / 2];
            yuv[..w * h].fill(40 + (k * 40) as u8);
            let stream = encoder.encode(&YUVBuffer::from_vec(yuv, w, h)).expect("encodes").to_vec();
            let mut sample = Vec::new();
            for unit in nal_units(&stream) {
                match unit[0] & 0x1f {
                    7 => sps = unit.to_vec(),
                    8 => pps = unit.to_vec(),
                    _ => {
                        sample.extend_from_slice(&(unit.len() as u32).to_be_bytes());
                        sample.extend_from_slice(unit);
                    }
                }
            }
            samples.push((k == 0, sample));
        }
        let file = std::fs::File::create(path).expect("creates");
        let mut writer = mp4::Mp4Writer::write_start(file, &mp4::Mp4Config { major_brand: "isom".parse().unwrap(), minor_version: 512, compatible_brands: vec!["isom".parse().unwrap(), "avc1".parse().unwrap()], timescale: 1000 }).expect("starts");
        writer.add_track(&mp4::TrackConfig { track_type: mp4::TrackType::Video, timescale: 1000, language: "und".into(), media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig { width: size.0, height: size.1, seq_param_set: sps, pic_param_set: pps }) }).expect("track");
        for (k, (sync, bytes)) in samples.into_iter().enumerate() {
            writer.write_sample(1, &mp4::Mp4Sample { start_time: k as u64 * 100, duration: 100, rendering_offset: 0, is_sync: sync, bytes: bytes.into() }).expect("sample");
        }
        writer.write_end().expect("ends");
    }

    /// The units of an Annex B stream.
    fn nal_units(stream: &[u8]) -> Vec<&[u8]> {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 <= stream.len() {
            if stream[i..i + 3] == [0, 0, 1] {
                starts.push(i + 3);
                i += 3;
            } else {
                i += 1;
            }
        }
        starts.iter().enumerate().map(|(n, s)| {
            let end = starts.get(n + 1).map_or(stream.len(), |e| e - 3);
            let unit = &stream[*s..end];
            unit.strip_suffix(&[0]).unwrap_or(unit)
        }).collect()
    }

    #[test]
    fn a_video_is_read_and_its_frames_come_out_as_pictures() {
        let d = dir("video");
        let clip = d.join("clip.mp4");
        make_clip(&clip, (64, 48), 5);
        let info = probe(&clip).expect("an H.264 MP4 plays");
        assert_eq!((info.size, info.frames, info.audio), ((64, 48), 5, None), "no sound in it");
        assert!((info.fps - 10.0).abs() < 0.5, "{}", info.fps);
        assert_eq!(source_of(&clip), Ok(Source::Video(clip.clone())));
        // Decode it as the player does.
        let mut reader = open_mp4(&clip).unwrap();
        let (sps, pps) = {
            let t = &reader.tracks()[&info.video];
            (t.sequence_parameter_set().unwrap().to_vec(), t.picture_parameter_set().unwrap().to_vec())
        };
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let mut lights = Vec::new();
        for id in 1..=info.frames {
            let s = reader.read_sample(info.video, id).unwrap().unwrap();
            if let Some(yuv) = decoder.decode(&annex_b(&s.bytes, s.is_sync, &sps, &pps)).unwrap() {
                let rgba = frame_rgba(&yuv, (64, 48));
                assert_eq!(rgba.len(), 64 * 48 * 4);
                // A smaller copy for a smaller monitor.
                assert_eq!(frame_rgba(&yuv, (32, 24)).len(), 32 * 24 * 4);
                lights.push(rgba[0] as i32);
            }
        }
        assert!(lights.len() >= 4, "the frames came out: {lights:?}");
        assert!(lights.windows(2).all(|w| w[1] >= w[0]) && lights.last() > lights.first(), "the picture brightens as the clip does: {lights:?}");
    }

    #[test]
    fn a_player_shows_the_clip_and_a_slideshow_and_stops_when_dropped() {
        let d = dir("player");
        let clip = d.join("clip.mp4");
        make_clip(&clip, (64, 48), 5);
        let ctx = egui::Context::default();
        let what = |path: &Path| (path.to_string_lossy().into_owned(), (64, 48), 3, false);
        let shared = start(&ctx, &what(&clip)).expect("starts");
        let end = Instant::now() + Duration::from_secs(10);
        while lock(&shared.layers).a.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(lock(&shared.layers).a.is_some(), "a frame was shown");
        // Idle: the frame stands still.
        shared.paused.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(300));
        let id = lock(&shared.layers).a.as_ref().map(|i| i.id);
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(lock(&shared.layers).a.as_ref().map(|i| i.id), id, "no frame while it is not seen");
        shared.stop.store(true, Ordering::Relaxed);
        // A slideshow of two pictures at a three-second interval shows the first at once.
        picture(&d.join("1.png"), (16, 16), [200, 0, 0]);
        picture(&d.join("2.png"), (16, 16), [0, 0, 200]);
        let folder = d.join("slides");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::copy(d.join("1.png"), folder.join("1.png")).unwrap();
        std::fs::copy(d.join("2.png"), folder.join("2.png")).unwrap();
        let slides = start(&ctx, &what(&folder)).expect("starts");
        let end = Instant::now() + Duration::from_secs(10);
        while lock(&slides.layers).a.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = lock(&slides.layers).a.clone().expect("the first picture");
        assert_eq!(&first.rgba[..3], &[200, 0, 0]);
        slides.stop.store(true, Ordering::Relaxed);
        assert!(start(&ctx, &what(&d.join("missing.png"))).is_none());
    }

    #[test]
    fn the_settings_say_what_to_draw_and_default_to_nothing() {
        let s = InterfaceSettings::default();
        assert!(s.bg_media.is_empty() && s.bg_mode == "behind" && s.bg_interval == 30 && !s.bg_sound && (s.bg_dim - 0.55).abs() < 1e-6);
        let old: InterfaceSettings = serde_json::from_str(r#"{"scale": 1.1}"#).unwrap();
        assert_eq!(old.bg_media, s.bg_media);
        let mut set = s.clone();
        set.bg_media.insert("2".into(), "x.mp4".into());
        let back: InterfaceSettings = serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
        assert_eq!(back.bg_media, set.bg_media);
    }

    #[test]
    fn the_board_fades_out_in_whole_mode_and_comes_when_called() {
        let _guard = crate::hud_dock::test_lock();
        let mut s = InterfaceSettings::default();
        s.bg_media.insert(ALL.into(), "x.png".into());
        publish(&s, false);
        assert!(!whole_active() && board_shown(), "behind the board: the board is always there");
        s.bg_mode = "whole".into();
        *lock(&BOARD) = (1.0, None);
        *lock(&CALLED) = None;
        for _ in 0..200 {
            publish_dt(&s, false, 0.1);
        }
        assert!(whole_active() && !board_shown() && lock(&BOARD).0 < 0.05, "the media alone");
        call_board();
        for _ in 0..200 {
            publish_dt(&s, false, 0.1);
        }
        assert!(board_shown() && lock(&BOARD).0 > 0.95, "called by name");
        *lock(&CALLED) = None;
        *lock(&BOARD) = (0.0, None);
        for _ in 0..200 {
            publish_dt(&s, true, 0.1);
        }
        assert!(board_shown(), "spoken to");
        // Without media there is nothing to show instead of the board.
        s.bg_media.clear();
        *lock(&BOARD) = (0.0, None);
        for _ in 0..200 {
            publish_dt(&s, false, 0.1);
        }
        assert!(board_shown());
    }

    #[test]
    fn the_background_is_set_by_voice_in_english_and_german() {
        let d = dir("voice");
        let clip = d.join("my clip.mp4");
        make_clip(&clip, (32, 32), 2);
        picture(&d.join("a.png"), (8, 8), [1, 1, 1]);
        let said = |t: &str| command(t);
        let media = |t: &str| said(t).and_then(|a| a["background_media"].as_str().map(str::to_string));
        let path = clip.to_string_lossy().into_owned();
        assert_eq!(media(&format!("Set my background to {path}")), Some(path.clone()));
        assert_eq!(media(&format!("set my background to \"{path}\" on monitor 2")), Some(path.clone()));
        assert_eq!(said(&format!("set my background to \"{path}\" on monitor 2")).unwrap()["background_monitor"], "2");
        assert_eq!(said(&format!("Setz mein Hintergrundbild auf \"{path}\" auf dem zweiten Bildschirm")).unwrap()["background_monitor"], "zweiten");
        assert_eq!(media("Use my Pictures folder as a slideshow on monitor 2"), Some("pictures".into()));
        assert_eq!(said("Use my Pictures folder as a slideshow on monitor 2").unwrap()["background_monitor"], "2");
        assert_eq!(media("Diashow aus meinem Bilder-Ordner"), Some("bilder".into()));
        assert_eq!(media("set my wallpaper to sunset.png"), Some("sunset.png".into()));
        assert_eq!(said("make my background a slideshow of my pictures folder, every 10 seconds").unwrap()["background_interval"], 10);
        assert_eq!(said("change the slideshow every 2 minutes").unwrap()["background_interval"], 120);
        assert_eq!(said("wechsel die diashow alle 20 sekunden").unwrap()["background_interval"], 20);
        assert_eq!(said("background video with sound").unwrap()["background_sound"], true);
        assert_eq!(said("mute the background video").unwrap()["background_sound"], false);
        assert_eq!(said("Hintergrundvideo mit Ton").unwrap()["background_sound"], true);
        assert_eq!(said("only behind the board").unwrap()["background_mode"], "behind");
        assert_eq!(said("fill the whole background").unwrap()["background_mode"], "whole");
        assert_eq!(said("remove my background picture").unwrap()["background_media"], "none");
        assert_eq!(said("remove my background video on monitor 2").unwrap()["background_monitor"], "2");
        assert_eq!(said("entferne mein Hintergrundbild").unwrap()["background_media"], "none");
        // Not orders for it.
        for no in ["set my background to red", "make the background darker", "how do I set a wallpaper?", "what is a slideshow?", "open my pictures folder", "play the video on youtube", "mute the music"] {
            assert_eq!(said(no), None, "{no}");
        }
    }

    #[test]
    fn the_background_arguments_set_the_settings_or_say_why_not() {
        let d = dir("apply");
        picture(&d.join("a.png"), (8, 8), [1, 1, 1]);
        let (mut s, mut done, mut failed) = (InterfaceSettings::default(), Vec::new(), Vec::new());
        let path = d.to_string_lossy().into_owned();
        apply(&mut s, &json!({"background_media": path, "background_monitor": "2", "background_mode": "whole", "background_interval": 5, "background_sound": true, "background_dim": 40}), &mut done, &mut failed);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!((s.bg_media.get("2").map(String::as_str), s.bg_mode.as_str(), s.bg_interval, s.bg_sound), (Some(path.as_str()), "whole", 5, true));
        assert!((s.bg_dim - 0.4).abs() < 1e-6);
        assert!(done.iter().any(|l| l.contains("slideshow of 1 pictures on monitor 2")), "{done:?}");
        // Taken off again, and for a monitor with nothing of its own.
        let (mut done, mut failed) = (Vec::new(), Vec::new());
        apply(&mut s, &json!({"background_media": "none", "background_monitor": "2"}), &mut done, &mut failed);
        assert!(s.bg_media.is_empty() && failed.is_empty());
        apply(&mut s, &json!({"background_media": "none"}), &mut done, &mut failed);
        assert_eq!(failed, vec!["There is no background picture or video to remove, Sir.".to_string()]);
        // A video this cannot play says so.
        let (mut done, mut failed) = (Vec::new(), Vec::new());
        std::fs::write(d.join("x.webm"), "x").unwrap();
        apply(&mut s, &json!({"background_media": d.join("x.webm").to_string_lossy(), "background_interval": 1, "background_dim": 95, "background_mode": "sideways"}), &mut done, &mut failed);
        assert!(failed.iter().any(|f| f.contains("WEBM") && f.contains("H.264")) && failed.iter().any(|f| f.contains("sideways")), "{failed:?}");
        assert_eq!((s.bg_interval, s.bg_dim), (3, 0.9), "kept in their range");
        assert!(s.bg_media.is_empty());
    }

    /// The pictures of both modes, drawn as the HUD draws them (target/ui-snapshots/).
    #[test]
    fn snapshot_background_media_in_both_modes() {
        let _theme = crate::holo_theme::look_for_pictures();
        let _guard = crate::hud_dock::test_lock();
        let d = dir("pictures");
        // A picture with a horizon and a sun, so cover-fit and dimming can be seen.
        let mut img = image::RgbImage::new(960, 540);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let sky = y < 330;
            let sun = ((x as f32 - 700.0).powi(2) + (y as f32 - 250.0).powi(2)).sqrt() < 70.0;
            *px = image::Rgb(if sun { [255, 220, 120] } else if sky { [40 + (y / 4) as u8, 80 + (y / 5) as u8, 170] } else { [30, 90 - ((y - 330) / 5) as u8, 50] });
        }
        let (one, two) = (d.join("sky.png"), d.join("sky2.png"));
        img.save(&one).unwrap();
        image::imageops::flip_horizontal(&img).save(&two).unwrap();
        let size = vec2(1280.0, 720.0);
        let mut lights = Vec::new();
        // One monitor: the picture; the other: a slideshow folder.
        let rects = [(1usize, Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 720.0))), (2usize, Rect::from_min_size(pos2(640.0, 0.0), vec2(640.0, 720.0)))];
        for (mode, name) in [("behind", "background_behind_the_board"), ("whole", "background_whole_media")] {
            let mut s = InterfaceSettings::default();
            s.bg_mode = mode.into();
            s.bg_media.insert("1".into(), one.to_string_lossy().into_owned());
            s.bg_media.insert("2".into(), d.to_string_lossy().into_owned());
            *lock(&BOARD) = (if mode == "whole" { 0.0 } else { 1.0 }, None);
            *lock(&CALLED) = None;
            let mut snap = crate::ui_snapshot::Snapshot::new(size);
            let mut loaded = false;
            // (Frames until the decoder threads have delivered.)
            let mut canvas = None;
            for _ in 0..80 {
                canvas = Some(snap.render(2, |ctx| {
                    publish(&s, false);
                    *lock(&BOARD) = (if mode == "whole" { 0.0 } else { 1.0 }, None);
                    crate::gui::paint_canvas_in(ctx, &rects);
                    curtain(ctx, &rects);
                }));
                loaded = lock(&PLAYERS).as_ref().is_some_and(|p| p.len() == 2 && p.values().all(|p| p.textures[0].is_some()));
                if loaded {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(loaded, "both monitors' media came");
            let canvas = canvas.expect("rendered");
            canvas.save(name);
            // The picture is on the left, the slideshow's first on the right; behind the board both are dimmed, alone they are not.
            let (left, right) = (Rect::from_min_size(pos2(40.0, 40.0), vec2(200.0, 200.0)), Rect::from_min_size(pos2(680.0, 40.0), vec2(200.0, 200.0)));
            let (l, r) = (canvas.brightness(left), canvas.brightness(right));
            assert!(l > 5.0 && r > 5.0, "{name}: media drawn ({l}, {r})");
            lights.push(l);
            *lock(&PLAYERS) = None;
        }
        assert!(lights[1] > lights[0] * 1.3, "behind the board the media is dimmed, alone it is not: {lights:?}");
    }
}
