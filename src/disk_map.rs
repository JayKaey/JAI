//! DISK MAP ("what is taking up space?", "map my D drive"): a drive or a folder read live into a
//! treemap on the board, with what fills it by file type, its largest items and a spoken verdict.
//!
//! * The scan runs on a thread of its own at below-normal priority (`FindFirstFileExW` with large
//!   fetches on Windows, `std::fs` elsewhere). It never follows junctions, symbolic links or
//!   mount points, never opens a cloud folder whose contents are not on this PC, counts cloud
//!   placeholders apart ("IN THE CLOUD") without adding them to the used space, and counts the
//!   folders it may not read. A new scan, `< BACK` out of a folder being read again, or closing
//!   the map stops it.
//! * Memory stays bounded on a drive with a million files: every folder is one node with its
//!   totals; per folder only its largest files are nodes of their own and everything else is one
//!   "smaller items" bucket; past `KEEP_DEPTH` levels or `NODE_BUDGET` nodes a folder folds into
//!   its parent, which is read again when Sir opens it (`drill`).
//! * The drive's total and free space (`GetDiskFreeSpaceExW`) give the progress ("62.0 GB TO
//!   GO"); what the walk cannot see becomes "System & hidden" at the end; the Recycle Bin is asked
//!   for its size (`SHQueryRecycleBinW`).
//! * When a scan Sir asked for is done, the advice is queued for JAI to say
//!   (`announcements`).
//!
//! Verifier and dry runs (and unit tests) never touch a disk: they open a fixed demo drive.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::{info, warn};

// ----------------------------------------------------------------------------- settings ----

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;
/// Folders deeper than this below the scanned root fold into their parent (read again when Sir
/// opens that parent).
const KEEP_DEPTH: usize = 12;
/// At most this many nodes per scan (about a hundred bytes each with their names).
const NODE_BUDGET: usize = 400_000;
/// Per folder, its largest files of at least `KEEP_MIN` become nodes of their own.
const KEEP_FILES: usize = 8;
const KEEP_MIN: u64 = MB;
/// Folders this close to the root keep their bytes per file type (FILE TYPES when opened).
const CAT_DEPTH: usize = 4;
/// The largest files of the scan, kept for the advice ("a single video over 10 GB").
const TOP_FILES: usize = 24;
/// How often a running scan publishes its progress (and wakes the HUD).
const PUBLISH_EVERY: Duration = Duration::from_millis(100);
/// The most tiles the live map shows (the rest is one tile).
const LIVE_TILES: usize = 40;
/// A scan of the same root this recent is shown again instead of read again.
const REUSE_FOR: Duration = Duration::from_secs(600);
/// From this used share on a drive is nearly full (red ring, "nearly full").
pub(crate) const NEARLY_FULL: f64 = 0.90;
/// What the walk did not see becomes a tile from this size on.
const HIDDEN_MIN: u64 = 64 * MB;
/// The tile for what the drive uses that no folder shows.
pub(crate) const HIDDEN_NAME: &str = "System & hidden";
/// The Recycle Bin's folder at a drive's root.
pub(crate) const RECYCLE_NAME: &str = "$Recycle.Bin";
/// Root files that are always in use (their size is asked of the file itself when the folder
/// listing shows none).
const SYSTEM_FILES: [&str; 3] = ["pagefile.sys", "hiberfil.sys", "swapfile.sys"];
/// Folders whose every file counts as CODE & DEV.
const DEV_DIRS: [&str; 11] = ["node_modules", ".git", "__pycache__", ".venv", "venv", "site-packages", ".cargo", ".rustup", ".gradle", ".m2", ".nuget"];
/// Sir's own folders by name (the names `path` may carry).
const KNOWN: [&str; 8] = ["Downloads", "Documents", "Desktop", "Videos", "Pictures", "Music", "AppData", "Temp"];

static OPEN: AtomicBool = AtomicBool::new(false);
/// Bumped by every new scan and by closing the map: a scan whose number is no longer current
/// stops at its next folder.
static SCAN_GEN: AtomicU64 = AtomicU64::new(0);
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------- file types ----

/// What a file is, by its extension (the treemap's colours and FILE TYPES).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) enum Category {
    Programs,
    Video,
    Archives,
    Code,
    Images,
    Documents,
    Audio,
    #[default]
    Other,
}

impl Category {
    pub(crate) const ALL: [Category; 8] = [Category::Programs, Category::Video, Category::Archives, Category::Code, Category::Images, Category::Documents, Category::Audio, Category::Other];

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Category::Programs => "PROGRAMS",
            Category::Video => "VIDEO",
            Category::Archives => "ARCHIVES",
            Category::Code => "CODE & DEV",
            Category::Images => "IMAGES",
            Category::Documents => "DOCUMENTS",
            Category::Audio => "AUDIO",
            Category::Other => "OTHER",
        }
    }

    /// The type's fixed colour (legend swatches, tiles, bars), whatever the theme.
    pub(crate) fn rgb(self) -> [u8; 3] {
        match self {
            Category::Programs => [0x3C, 0xC3, 0xD6],
            Category::Video => [0xE2, 0x56, 0x6A],
            Category::Archives => [0xF2, 0xA5, 0x2A],
            Category::Code => [0xA5, 0x8B, 0xE8],
            Category::Images => [0xF6, 0xC8, 0x4C],
            Category::Documents => [0xB9, 0xD8, 0x6A],
            Category::Audio => [0xF2, 0x8A, 0xC0],
            Category::Other => [0xB8, 0xC2, 0xCC],
        }
    }

    /// One file of the type, as JAI says it ("a single video over 10 GB").
    fn noun(self) -> &'static str {
        match self {
            Category::Programs => "program file",
            Category::Video => "video",
            Category::Archives => "archive",
            Category::Code => "developer file",
            Category::Images => "image",
            Category::Documents => "document",
            Category::Audio => "audio file",
            Category::Other => "file",
        }
    }
}

static EXTENSIONS: LazyLock<HashMap<&'static str, Category>> = LazyLock::new(|| {
    let lists: [(Category, &[&str]); 7] = [
        (Category::Programs, &["exe", "dll", "sys", "msi", "msp", "msu", "ocx", "cpl", "drv", "efi", "mui", "scr", "appx", "msix", "appxbundle", "msixbundle", "winmd", "ax", "com", "so", "dylib", "apk", "vsix"]),
        (Category::Video, &["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "m2ts", "mts", "3gp", "vob", "ogv"]),
        (Category::Archives, &["zip", "7z", "rar", "iso", "cab", "tar", "gz", "tgz", "bz2", "xz", "zst", "lz4", "lzma", "wim", "esd", "vhd", "vhdx", "vmdk", "img", "dmg", "jar"]),
        (
            Category::Code,
            &[
                "rs", "py", "pyc", "pyd", "pyi", "ipynb", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cc", "cxx", "hxx", "cs", "java", "class", "kt", "go", "rb", "php", "swift", "lua", "dart", "json", "toml", "yaml", "yml", "xml", "html", "htm", "css", "scss", "vue", "svelte", "pdb", "obj", "o", "a", "lib", "exp", "ilk", "idb", "pch", "ipch", "rlib", "rmeta", "d", "wasm", "map", "sln", "vcxproj", "csproj", "props", "targets", "cmake", "gradle", "lock", "sql", "sh", "ps1", "psm1", "bat", "cmd", "whl", "nupkg", "node", "asm", "tlog",
            ],
        ),
        (Category::Images, &["png", "jpg", "jpeg", "jfif", "webp", "gif", "bmp", "tif", "tiff", "psd", "psb", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "heic", "heif", "svg", "ico", "tga", "exr", "hdr", "dds", "avif", "jxl", "xcf", "kra", "ai", "eps"]),
        (Category::Documents, &["pdf", "doc", "docx", "docm", "xls", "xlsx", "xlsm", "ppt", "pptx", "odt", "ods", "odp", "txt", "md", "rtf", "csv", "epub", "mobi", "one", "msg", "eml", "pages", "numbers", "key", "tex", "xps", "oxps", "djvu"]),
        (Category::Audio, &["mp3", "wav", "flac", "ogg", "oga", "m4a", "aac", "wma", "opus", "aiff", "aif", "mid", "midi", "ape", "alac", "wv", "amr"]),
    ];
    let mut map = HashMap::new();
    for (category, extensions) in lists {
        for extension in extensions {
            map.insert(*extension, category);
        }
    }
    map
});

/// A file's type by its extension ("clip.MKV" is a video, "Cargo.lock" developer's).
pub(crate) fn category_of(name: &str) -> Category {
    let Some(dot) = name.rfind('.') else { return Category::Other };
    let extension = &name[dot + 1..];
    if extension.is_empty() || extension.len() > 12 || !extension.is_ascii() {
        return Category::Other;
    }
    let mut lower = [0u8; 12];
    for (slot, byte) in lower.iter_mut().zip(extension.bytes()) {
        *slot = byte.to_ascii_lowercase();
    }
    std::str::from_utf8(&lower[..extension.len()]).ok().and_then(|e| EXTENSIONS.get(e).copied()).unwrap_or(Category::Other)
}

fn is_dev_dir(name: &str) -> bool {
    DEV_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// The type with the most bytes (OTHER when there are none).
fn dominant(cats: &[u64; 8]) -> Category {
    let mut best = (0u64, Category::Other);
    for c in Category::ALL {
        if cats[c.index()] > best.0 {
            best = (cats[c.index()], c);
        }
    }
    best.1
}

// ----------------------------------------------------------------------------- wording ----

/// A size as the panel shows it: three digits at most ("1.6 GB", "84.9 GB", "754 MB", "0 B").
pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 999.5 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 99.95 => format!("{value:.0} {}", UNITS[unit]),
        _ if value >= 0.995 => format!("{value:.1} {}", UNITS[unit]),
        _ => format!("{value:.2} {}", UNITS[unit]),
    }
}

/// A size as JAI says it ("9 GB", not "9.0 GB").
pub(crate) fn spoken_size(bytes: u64) -> String {
    format_size(bytes).replace(".0 ", " ")
}

/// A count with thousands separators ("1,336,317").
pub(crate) fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "231 of 234 GB" (one unit when both share it).
fn used_of(used: u64, total: u64) -> String {
    let (a, b) = (spoken_size(used), spoken_size(total));
    match (a.rsplit_once(' '), b.rsplit_once(' ')) {
        (Some((n, unit)), Some((m, unit_b))) if unit == unit_b => format!("{n} of {m} {unit}"),
        _ => format!("{a} of {b}"),
    }
}

// ------------------------------------------------------------------------------ targets ----

static DRIVE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:my|the|on)\s+)?(?:(?:drive|laufwerk|partition)\s+)?([a-z])(?::[\\/]?)?(?:\s+(?:drive|laufwerk|partition))?$").expect("the drive pattern")
});

/// One of Sir's known folders by the words he used ("my downloads", "documents folder").
fn known_name(words: &str) -> Option<&'static str> {
    let lower = words.trim().to_lowercase();
    let w = lower.strip_prefix("my ").or_else(|| lower.strip_prefix("the ")).unwrap_or(&lower);
    let w = w.strip_suffix(" folder").or_else(|| w.strip_suffix(" ordner")).unwrap_or(w).trim();
    Some(match w {
        "downloads" | "download" => "Downloads",
        "documents" | "document" => "Documents",
        "desktop" => "Desktop",
        "videos" | "video" => "Videos",
        "pictures" | "picture" | "photos" | "images" => "Pictures",
        "music" => "Music",
        "appdata" | "app data" => "AppData",
        "temp" | "tmp" | "temp files" | "temporary files" => "Temp",
        _ => return None,
    })
}

/// `C:\` for drive `C`.
fn drive_root(letter: char) -> PathBuf {
    PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase()))
}

/// The drive letter Windows runs from (`C` when it cannot tell).
fn system_drive() -> char {
    std::env::var("SystemDrive").ok().and_then(|d| d.chars().next()).filter(char::is_ascii_alphabetic).map(|c| c.to_ascii_uppercase()).unwrap_or('C')
}

/// The scan's default root: the system drive (the file system's root elsewhere).
fn default_root() -> PathBuf {
    if cfg!(windows) {
        drive_root(system_drive())
    } else {
        PathBuf::from("/")
    }
}

/// The drive letter a path is on ("C:\Users" → `C`).
pub(crate) fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut chars = text.chars();
    let (letter, colon) = (chars.next()?, chars.next()?);
    (letter.is_ascii_alphabetic() && colon == ':').then(|| letter.to_ascii_uppercase())
}

/// The path is a whole drive ("D:\").
fn is_drive_root(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches(['\\', '/']).len() == 2 && drive_letter(path).is_some()
}

/// `.` and `..` resolved without touching the disk (the scan prefixes `\\?\`, which takes the
/// path literally).
fn normalise(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(out.components().next_back(), Some(Component::RootDir | Component::Prefix(_)) | None) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What `raw` names, without looking at the disk: nothing (the system drive), a drive ("D",
/// "d:", "D:\", "my D drive"), one of Sir's known folders ("Downloads", "my documents folder",
/// found by `known`), his user folder, or a path (`~` and `%VARIABLES%` expanded).
fn parse_target(raw: &str, known: &dyn Fn(&str) -> Option<PathBuf>, home: Option<&Path>, system: char) -> Result<PathBuf, String> {
    let t = raw.trim().trim_matches(['"', '\'']).trim();
    if t.is_empty() {
        return Ok(if cfg!(windows) { drive_root(system) } else { default_root() });
    }
    if let Some(c) = DRIVE_WORDS.captures(t) {
        let letter = c[1].chars().next().unwrap_or(system);
        return Ok(drive_root(letter));
    }
    if let Some(name) = known_name(t) {
        return known(name).ok_or_else(|| format!("I can't find your {name} folder, Sir."));
    }
    let lower = t.to_lowercase();
    if matches!(lower.as_str(), "home" | "my home" | "user folder" | "my user folder" | "profile" | "my profile" | "my files") {
        return home.map(Path::to_path_buf).ok_or_else(|| "I can't find your user folder, Sir.".to_string());
    }
    let slashes = |p: String| if cfg!(windows) { p.replace('/', "\\") } else { p };
    if let Some(rest) = t.strip_prefix('~') {
        let home = home.ok_or_else(|| "I can't find your user folder, Sir.".to_string())?;
        let rest = slashes(crate::apps::expand_env(rest));
        return Ok(normalise(home.join(rest.trim_start_matches(['\\', '/']))));
    }
    Ok(normalise(PathBuf::from(slashes(crate::apps::expand_env(t)))))
}

/// The user's temp folder, without a trailing separator.
fn temp_folder() -> PathBuf {
    let temp = std::env::temp_dir();
    let text = temp.to_string_lossy();
    let trimmed = text.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        temp.clone()
    } else {
        PathBuf::from(trimmed)
    }
}

/// Where one of Sir's known folders is (Windows knows where he moved them).
#[cfg(windows)]
fn known_folder(name: &str) -> Option<PathBuf> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT};
    let id = match name {
        "Downloads" => FOLDERID_Downloads,
        "Documents" => FOLDERID_Documents,
        "Desktop" => FOLDERID_Desktop,
        "Videos" => FOLDERID_Videos,
        "Pictures" => FOLDERID_Pictures,
        "Music" => FOLDERID_Music,
        "Temp" => return Some(temp_folder()),
        "AppData" => return crate::apps::home_dir().map(|h| h.join("AppData")),
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
    found.map(PathBuf::from).filter(|p| p.is_dir()).or_else(|| Some(crate::features::user_folder(name)))
}

#[cfg(not(windows))]
fn known_folder(name: &str) -> Option<PathBuf> {
    match name {
        "Temp" => Some(temp_folder()),
        "AppData" => crate::apps::home_dir().map(|h| h.join(".local").join("share")),
        _ => Some(crate::features::user_folder(name)),
    }
}

/// The folder or drive `raw` names, checked to exist (nothing: the system drive).
pub(crate) fn resolve(raw: Option<&str>) -> Result<PathBuf, String> {
    let raw = raw.unwrap_or("").trim();
    let system = system_drive();
    let home = crate::apps::home_dir();
    let path = parse_target(raw, &known_folder, home.as_deref(), system)?;
    let candidates: Vec<PathBuf> = if path.is_absolute() {
        vec![path]
    } else {
        // "Games": a folder of that name in his user folder or at the system drive's root.
        [home.map(|h| h.join(&path)), Some(default_root().join(&path))].into_iter().flatten().collect()
    };
    candidates.into_iter().find(|p| p.is_dir()).ok_or_else(|| format!("I can't find {} on this PC, Sir.", if raw.is_empty() { "the system drive" } else { raw }))
}

/// How the panel names the scanned root ("C:\", or the whole folder path).
fn label_of(root: &Path) -> String {
    if is_drive_root(root) {
        drive_letter(root).map_or_else(|| root.display().to_string(), |l| format!("{l}:\\"))
    } else {
        root.display().to_string()
    }
}

/// The tree's root name ("C:\", "Downloads").
fn root_name(root: &Path) -> String {
    if is_drive_root(root) {
        label_of(root)
    } else {
        root.file_name().map_or_else(|| label_of(root), |n| n.to_string_lossy().into_owned())
    }
}

/// How JAI names the scanned root ("your drive", "your D drive", "your Downloads folder").
fn spoken_target(root: &Path) -> String {
    if is_drive_root(root) {
        let letter = drive_letter(root).unwrap_or('C');
        return if letter == system_drive() { "your drive".to_string() } else { format!("your {letter} drive") };
    }
    let name = root_name(root);
    if KNOWN.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
        format!("your {name} folder")
    } else {
        format!("the {name} folder")
    }
}

/// A folder name as said aloud ("C:\" is "the C drive").
fn speakable(name: &str) -> String {
    match drive_letter(Path::new(name)).filter(|_| name.trim_end_matches(['\\', '/']).len() == 2) {
        Some(letter) => format!("the {letter} drive"),
        None => name.to_string(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

// -------------------------------------------------------------------------------- drive ----

/// A drive's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Drive {
    pub letter: char,
    pub total: u64,
    pub free: u64,
}

impl Drive {
    pub(crate) fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub(crate) fn used_share(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f64 / self.total as f64
        }
    }
}

/// The total and free bytes of the volume `path` is on.
#[cfg(windows)]
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut total, mut free) = (0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and both out-pointers are valid for the call.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), None, Some(&mut total as *mut u64), Some(&mut free as *mut u64)) }.ok()?;
    (total > 0).then_some((total, free))
}

#[cfg(not(windows))]
fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// What the Recycle Bin holds on drive `letter`.
#[cfg(windows)]
fn recycle_bin(letter: char) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, i64Size: 0, i64NumItems: 0 };
    // SAFETY: `root` is NUL-terminated and `info` carries its own size, as the API requires.
    unsafe { SHQueryRecycleBinW(PCWSTR(root.as_ptr()), &mut info) }.ok()?;
    u64::try_from(info.i64Size).ok()
}

#[cfg(not(windows))]
fn recycle_bin(_letter: char) -> Option<u64> {
    None
}

// --------------------------------------------------------------------------------- tree ----

/// A node of the scanned tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
    /// What the drive uses that the walk did not see ("System & hidden").
    Hidden,
}

/// A folder (with its totals), one of its larger files, or the hidden rest of a drive.
#[derive(Clone, Debug)]
pub(crate) struct Node {
    /// Everything below it (a file: its size).
    pub size: u64,
    /// What is not a node of its own here: smaller files and folders folded in.
    pub rest_bytes: u64,
    pub rest_items: u32,
    /// Files and folders below it, all levels.
    pub files: u32,
    pub dirs: u32,
    pub parent: u32,
    name_at: u32,
    name_len: u32,
    pub kind: Kind,
    /// Its type (a folder: the type with the most bytes in it).
    pub cat: Category,
    /// Some of its folders are not nodes (past the depth or the budget): opening it reads it
    /// again.
    pub truncated: bool,
    pub unreadable: bool,
    /// Largest first once the folder is read.
    pub children: Vec<u32>,
    /// Bytes per type (folders near the root).
    pub cats: Option<Box<[u64; 8]>>,
}

/// A scanned folder tree: node 0 is the root; the names live in one string.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tree {
    pub nodes: Vec<Node>,
    names: String,
    /// The folder or drive that was read.
    pub root: PathBuf,
}

impl Tree {
    fn new(root: PathBuf, name: &str) -> Self {
        let mut tree = Tree { nodes: Vec::new(), names: String::new(), root };
        tree.push(u32::MAX, name, Kind::Dir, 0, Category::Other);
        tree
    }

    fn push(&mut self, parent: u32, name: &str, kind: Kind, size: u64, cat: Category) -> u32 {
        let id = self.nodes.len() as u32;
        let at = self.names.len() as u32;
        self.names.push_str(name);
        self.nodes.push(Node {
            size,
            rest_bytes: 0,
            rest_items: 0,
            files: u32::from(kind == Kind::File),
            dirs: 0,
            parent,
            name_at: at,
            name_len: name.len() as u32,
            kind,
            cat,
            truncated: false,
            unreadable: false,
            children: Vec::new(),
            cats: None,
        });
        id
    }

    /// A new child of `parent`.
    fn add(&mut self, parent: u32, name: &str, kind: Kind, size: u64, cat: Category) -> u32 {
        let id = self.push(parent, name, kind, size, cat);
        self.nodes[parent as usize].children.push(id);
        id
    }

    pub(crate) fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    pub(crate) fn name(&self, id: u32) -> &str {
        self.node(id).map_or("", |n| &self.names[n.name_at as usize..(n.name_at + n.name_len) as usize])
    }

    /// The nodes from the root's child down to `id` (empty for the root).
    pub(crate) fn chain(&self, id: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut at = id;
        while at != 0 && (at as usize) < self.nodes.len() {
            chain.push(at);
            at = self.nodes[at as usize].parent;
        }
        chain.reverse();
        chain
    }

    /// Where node `id` is on disk (the hidden rest: its drive).
    pub(crate) fn path(&self, id: u32) -> PathBuf {
        let mut path = self.root.clone();
        for c in self.chain(id) {
            if self.nodes[c as usize].kind == Kind::Hidden {
                break;
            }
            path.push(self.name(c));
        }
        path
    }

    /// The folder node at `path`, if it is inside the tree.
    pub(crate) fn find(&self, path: &Path) -> Option<u32> {
        let norm = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
        let (full, root) = (norm(path), norm(&self.root));
        let rest = full.strip_prefix(root.trim_end_matches('\\'))?;
        if !rest.is_empty() && !rest.starts_with('\\') {
            return None;
        }
        let mut at = 0u32;
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            at = *self.node(at)?.children.iter().find(|&&c| self.nodes[c as usize].kind == Kind::Dir && self.name(c).to_lowercase() == part)?;
        }
        Some(at)
    }

    /// Bytes per type below node `id` (exact near the root, from the children's types deeper).
    pub(crate) fn cats(&self, id: u32) -> [u64; 8] {
        let mut out = [0u64; 8];
        let Some(n) = self.node(id) else { return out };
        if let Some(cats) = &n.cats {
            return **cats;
        }
        match n.kind {
            Kind::Dir => {
                for &c in &n.children {
                    let child = &self.nodes[c as usize];
                    match &child.cats {
                        Some(cats) => {
                            for (o, c) in out.iter_mut().zip(cats.iter()) {
                                *o += c;
                            }
                        }
                        None if child.kind != Kind::Hidden => out[child.cat.index()] += child.size,
                        None => {}
                    }
                }
                out[n.cat.index()] += n.rest_bytes;
            }
            Kind::File => out[n.cat.index()] += n.size,
            Kind::Hidden => {}
        }
        out
    }

    fn sort_children(&mut self, id: u32) {
        let mut children = std::mem::take(&mut self.nodes[id as usize].children);
        let nodes = &self.nodes;
        children.sort_by_key(|&c| std::cmp::Reverse(nodes[c as usize].size));
        self.nodes[id as usize].children = children;
    }
}

/// "System & hidden": what drive uses beyond what the walk saw (system files the walk may not
/// read, shadow copies, the file system's own tables), as a tile of the root.
fn add_hidden(tree: &mut Tree, used: u64, scanned: u64) {
    let hidden = used.saturating_sub(scanned);
    if hidden < HIDDEN_MIN || tree.nodes.is_empty() {
        return;
    }
    tree.add(0, HIDDEN_NAME, Kind::Hidden, hidden, Category::Other);
    tree.nodes[0].size += hidden;
    tree.sort_children(0);
}

/// Folder `at` read again (`sub`, rooted at that folder) takes the place of what was known of
/// it; the folders above it see the change in size.
fn graft(tree: &mut Tree, at: u32, sub: Tree) {
    if (at as usize) >= tree.nodes.len() || sub.nodes.is_empty() {
        return;
    }
    let offset = tree.nodes.len() as u32 - 1;
    let names_offset = tree.names.len() as u32;
    tree.names.push_str(&sub.names);
    let mut nodes = sub.nodes.into_iter();
    let Some(root) = nodes.next() else { return };
    for mut n in nodes {
        n.parent = if n.parent == 0 { at } else { n.parent + offset };
        for c in n.children.iter_mut() {
            *c += offset;
        }
        n.name_at += names_offset;
        tree.nodes.push(n);
    }
    let old = {
        let t = &mut tree.nodes[at as usize];
        let old = (t.size, t.files, t.dirs);
        t.children = root.children.iter().map(|c| c + offset).collect();
        t.size = root.size;
        t.rest_bytes = root.rest_bytes;
        t.rest_items = root.rest_items;
        t.files = root.files;
        t.dirs = root.dirs;
        t.cat = root.cat;
        t.truncated = root.truncated;
        t.unreadable = root.unreadable;
        if root.cats.is_some() {
            t.cats = root.cats;
        }
        old
    };
    let mut up = tree.nodes[at as usize].parent;
    while (up as usize) < tree.nodes.len() {
        let n = &mut tree.nodes[up as usize];
        n.size = (n.size + root.size).saturating_sub(old.0);
        n.files = (n.files + root.files).saturating_sub(old.1);
        n.dirs = (n.dirs + root.dirs).saturating_sub(old.2);
        let next = n.parent;
        tree.sort_children(up);
        up = next;
    }
}

// --------------------------------------------------------------------------------- scan ----

/// What a scan counted.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Stats {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub unreadable: u64,
    pub cloud_bytes: u64,
    pub cloud_files: u64,
    pub elapsed: Duration,
}

/// A tile of the live map: a top-level folder or file as far as it has been read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveTile {
    pub name: String,
    pub bytes: u64,
    pub cat: Category,
    /// Read to the end.
    pub done: bool,
    /// A bucket of this many smaller items (0: a folder or a file of its own).
    pub items: u32,
}

/// A running scan as the panel sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Live {
    pub stats: Stats,
    /// The folder being read.
    pub current: String,
    /// Largest first.
    pub tiles: Vec<LiveTile>,
    /// Bytes per file type read so far.
    pub cats: [u64; 8],
}

/// One of the largest files found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BigFile {
    pub path: String,
    pub size: u64,
    pub cat: Category,
}

/// A finished scan.
pub(crate) struct Scanned {
    pub tree: Tree,
    pub stats: Stats,
    pub largest: Vec<BigFile>,
}

/// The scan's limits (the defaults for a whole drive; tests take small ones).
#[derive(Clone, Debug)]
pub(crate) struct ScanOptions {
    pub keep_depth: usize,
    pub node_budget: usize,
    pub keep_files: usize,
    pub keep_min: u64,
    pub top_files: usize,
    pub publish_every: Duration,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { keep_depth: KEEP_DEPTH, node_budget: NODE_BUDGET, keep_files: KEEP_FILES, keep_min: KEEP_MIN, top_files: TOP_FILES, publish_every: PUBLISH_EVERY }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    File,
    Dir,
    /// A cloud placeholder: its bytes are not on this PC.
    Cloud,
}

struct Entry {
    at: u32,
    len: u32,
    kind: EntryKind,
    size: u64,
    /// A folder's exact name, to read it next.
    os: Option<OsString>,
}

/// One folder's entries (reused from folder to folder: no allocation per file).
#[derive(Default)]
struct Listing {
    text: String,
    entries: Vec<Entry>,
}

impl Listing {
    fn clear(&mut self) {
        self.text.clear();
        self.entries.clear();
    }

    fn push(&mut self, name: impl Iterator<Item = char>, kind: EntryKind, size: u64, os: Option<OsString>) {
        let at = self.text.len() as u32;
        self.text.extend(name);
        let len = self.text.len() as u32 - at;
        self.entries.push(Entry { at, len, kind, size, os });
    }

    fn name(&self, e: &Entry) -> &str {
        &self.text[e.at as usize..(e.at + e.len) as usize]
    }
}

enum ListError {
    /// Access denied (or any other refusal): an unreadable folder.
    Denied,
    /// It vanished while the scan ran.
    Gone,
}

/// `\\?\` + the folder + `\*` (long paths, UNC shares), NUL-terminated.
#[cfg(windows)]
fn find_pattern(dir: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let (slash, backslash) = (u16::from(b'/'), u16::from(b'\\'));
    let raw: Vec<u16> = dir.as_os_str().encode_wide().map(|c| if c == slash { backslash } else { c }).collect();
    let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let mut pattern: Vec<u16> = Vec::with_capacity(raw.len() + 12);
    if raw.starts_with(&wide(r"\\?\")) || raw.starts_with(&wide(r"\\.\")) {
        pattern.extend(&raw);
    } else if raw.starts_with(&wide(r"\\")) {
        pattern.extend(wide(r"\\?\UNC\"));
        pattern.extend(&raw[2..]);
    } else if raw.len() >= 2 && raw[1] == u16::from(b':') {
        pattern.extend(wide(r"\\?\"));
        pattern.extend(&raw);
    } else {
        pattern.extend(&raw);
    }
    if pattern.last() != Some(&backslash) {
        pattern.push(backslash);
    }
    pattern.push(u16::from(b'*'));
    pattern.push(0);
    pattern
}

/// Reads one folder's entries (large fetches, basic info: no short names).
#[cfg(windows)]
fn list_dir(dir: &Path, out: &mut Listing) -> Result<(), ListError> {
    use std::os::windows::ffi::OsStringExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT,
        FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
    };
    /// Junctions, symbolic links and mount points are name surrogates: they point elsewhere.
    const NAME_SURROGATE: u32 = 0x2000_0000;
    let pattern = find_pattern(dir);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL-terminated and `data` is a WIN32_FIND_DATAW, which the basic
    // info level fills.
    let found = unsafe { FindFirstFileExW(PCWSTR(pattern.as_ptr()), FindExInfoBasic, (&mut data as *mut WIN32_FIND_DATAW).cast(), FindExSearchNameMatch, None, FIND_FIRST_EX_LARGE_FETCH) };
    let handle = match found {
        Ok(handle) => handle,
        Err(e) => {
            return match e.code().0 as u32 & 0xFFFF {
                // An empty drive has not even "." to find.
                2 | 18 => Ok(()),
                3 => Err(ListError::Gone),
                _ => Err(ListError::Denied),
            };
        }
    };
    let skip_dir = FILE_ATTRIBUTE_RECALL_ON_OPEN.0 | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0 | FILE_ATTRIBUTE_OFFLINE.0;
    let cloud_file = FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0 | FILE_ATTRIBUTE_OFFLINE.0;
    loop {
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
        let wide = &data.cFileName[..len];
        let attrs = data.dwFileAttributes;
        let dots = wide == [46] || wide == [46, 46];
        let link = attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 && data.dwReserved0 & NAME_SURROGATE != 0;
        if !dots && !link {
            let name = char::decode_utf16(wide.iter().copied()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER));
            if attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
                // A cloud folder whose contents are not here would be fetched by reading it.
                if attrs & skip_dir == 0 {
                    out.push(name, EntryKind::Dir, 0, Some(OsString::from_wide(wide)));
                }
            } else {
                let size = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
                out.push(name, if attrs & cloud_file != 0 { EntryKind::Cloud } else { EntryKind::File }, size, None);
            }
        }
        // SAFETY: the search handle is open and `data` is valid for the call.
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    // SAFETY: closes the search handle opened above, once.
    unsafe {
        let _ = FindClose(handle);
    }
    Ok(())
}

#[cfg(not(windows))]
fn list_dir(dir: &Path, out: &mut Listing) -> Result<(), ListError> {
    let read = std::fs::read_dir(dir).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { ListError::Gone } else { ListError::Denied })?;
    for entry in read.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        let os = entry.file_name();
        let name = os.to_string_lossy().into_owned();
        if kind.is_dir() {
            out.push(name.chars(), EntryKind::Dir, 0, Some(os));
        } else if kind.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(name.chars(), EntryKind::File, size, None);
        }
    }
    Ok(())
}

/// A folder entered and not yet left (the chain from the root to the folder being read).
struct Open {
    node: Option<u32>,
    path: PathBuf,
    depth: usize,
    size: u64,
    files: u32,
    dirs: u32,
    cats: [u64; 8],
}

enum Step {
    Enter { path: PathBuf, node: Option<u32>, depth: usize, dev: bool },
    Exit,
}

struct Walker<'a> {
    opts: &'a ScanOptions,
    tree: Tree,
    stats: Stats,
    largest: Vec<BigFile>,
    open: Vec<Open>,
    todo: Vec<Step>,
    listing: Listing,
    kept: Vec<(u64, usize)>,
    /// Bytes per file type so far.
    cats: [u64; 8],
}

impl Walker<'_> {
    /// Reads folder `path`: its files are counted (its largest ones become nodes), its folders
    /// are queued to be read next, in name order.
    fn enter(&mut self, path: PathBuf, node: Option<u32>, depth: usize, dev: bool) {
        let mut listing = std::mem::take(&mut self.listing);
        listing.clear();
        match list_dir(&path, &mut listing) {
            Ok(()) | Err(ListError::Gone) => {}
            Err(ListError::Denied) => {
                self.stats.unreadable += 1;
                if let Some(n) = node {
                    self.tree.nodes[n as usize].unreadable = true;
                }
            }
        }
        let mut here = Open { node, path, depth, size: 0, files: 0, dirs: 0, cats: [0; 8] };
        self.kept.clear();
        for (i, e) in listing.entries.iter().enumerate() {
            match e.kind {
                EntryKind::Dir => {}
                EntryKind::Cloud => {
                    self.stats.cloud_bytes += e.size;
                    self.stats.cloud_files += 1;
                }
                EntryKind::File => {
                    let name = listing.name(e);
                    // (The page file and its kin are in use: the listing may show them empty.)
                    let size = if depth == 0 && e.size == 0 && SYSTEM_FILES.iter().any(|f| f.eq_ignore_ascii_case(name)) {
                        std::fs::metadata(here.path.join(name)).map(|m| m.len()).unwrap_or(0)
                    } else {
                        e.size
                    };
                    let cat = if dev { Category::Code } else { category_of(name) };
                    here.size += size;
                    here.files += 1;
                    here.cats[cat.index()] += size;
                    self.cats[cat.index()] += size;
                    self.stats.bytes += size;
                    self.stats.files += 1;
                    if size > self.largest_floor() {
                        let path = here.path.join(name).display().to_string();
                        self.note_largest(BigFile { path, size, cat });
                    }
                    match node {
                        Some(_) if size >= self.opts.keep_min => self.kept.push((size, i)),
                        Some(n) => {
                            let folder = &mut self.tree.nodes[n as usize];
                            folder.rest_bytes += size;
                            folder.rest_items += 1;
                        }
                        None => {}
                    }
                }
            }
        }
        if let Some(n) = node {
            self.kept.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            for (rank, &(size, i)) in self.kept.iter().enumerate() {
                if rank < self.opts.keep_files && self.tree.nodes.len() < self.opts.node_budget {
                    let name = listing.name(&listing.entries[i]);
                    let cat = if dev { Category::Code } else { category_of(name) };
                    self.tree.add(n, name, Kind::File, size, cat);
                } else {
                    let folder = &mut self.tree.nodes[n as usize];
                    folder.rest_bytes += size;
                    folder.rest_items += 1;
                }
            }
        }
        let mut subfolders = Vec::new();
        for e in listing.entries.iter().filter(|e| e.kind == EntryKind::Dir) {
            let Some(os) = e.os.as_ref() else { continue };
            let name = listing.name(e);
            let child = match node {
                Some(n) if depth < self.opts.keep_depth && self.tree.nodes.len() < self.opts.node_budget => Some(self.tree.add(n, name, Kind::Dir, 0, Category::Other)),
                Some(n) => {
                    self.tree.nodes[n as usize].truncated = true;
                    None
                }
                None => None,
            };
            subfolders.push(Step::Enter { path: here.path.join(os), node: child, depth: depth + 1, dev: dev || is_dev_dir(name) });
        }
        self.stats.dirs += subfolders.len() as u64;
        self.open.push(here);
        self.todo.push(Step::Exit);
        self.todo.extend(subfolders.into_iter().rev());
        self.listing = listing;
    }

    /// The folder read last is done with everything below it: its totals go up a level.
    fn exit(&mut self) {
        let Some(done) = self.open.pop() else { return };
        if let Some(n) = done.node {
            let folder = &mut self.tree.nodes[n as usize];
            folder.size = done.size;
            folder.files = done.files;
            folder.dirs = done.dirs;
            folder.cat = dominant(&done.cats);
            if done.depth < CAT_DEPTH {
                folder.cats = Some(Box::new(done.cats));
            }
            folder.children.shrink_to_fit();
            self.tree.sort_children(n);
        }
        if let Some(parent) = self.open.last_mut() {
            parent.size += done.size;
            parent.files += done.files;
            parent.dirs += done.dirs + 1;
            for (p, c) in parent.cats.iter_mut().zip(done.cats) {
                *p += c;
            }
            // A folder that is no node of its own joins its parent's smaller items.
            if let (None, Some(p)) = (done.node, parent.node) {
                let folder = &mut self.tree.nodes[p as usize];
                folder.rest_bytes += done.size;
                folder.rest_items += 1;
            }
        }
    }

    fn largest_floor(&self) -> u64 {
        if self.largest.len() < self.opts.top_files {
            0
        } else {
            self.largest.last().map_or(0, |b| b.size)
        }
    }

    fn note_largest(&mut self, file: BigFile) {
        let at = self.largest.partition_point(|b| b.size >= file.size);
        self.largest.insert(at, file);
        self.largest.truncate(self.opts.top_files);
    }

    /// The live map: the root's folders read so far, the one being read with what it has so far.
    fn live(&self, started: Instant) -> Live {
        let root = &self.tree.nodes[0];
        let busy = self.open.get(1);
        let (busy_size, busy_cats) = self.open.iter().skip(1).fold((0u64, [0u64; 8]), |(size, mut cats), o| {
            for (c, add) in cats.iter_mut().zip(o.cats) {
                *c += add;
            }
            (size + o.size, cats)
        });
        let mut tiles = Vec::new();
        for &c in &root.children {
            let n = &self.tree.nodes[c as usize];
            if busy.is_some_and(|b| b.node == Some(c)) {
                if busy_size > 0 {
                    tiles.push(LiveTile { name: self.tree.name(c).to_string(), bytes: busy_size, cat: dominant(&busy_cats), done: false, items: 0 });
                }
            } else if n.size > 0 {
                tiles.push(LiveTile { name: self.tree.name(c).to_string(), bytes: n.size, cat: n.cat, done: true, items: 0 });
            }
        }
        if let Some(b) = busy.filter(|b| b.node.is_none() && busy_size > 0) {
            let name = b.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            tiles.push(LiveTile { name, bytes: busy_size, cat: dominant(&busy_cats), done: false, items: 0 });
        }
        if root.rest_bytes > 0 {
            tiles.push(LiveTile { name: smaller_items(root.rest_items), bytes: root.rest_bytes, cat: Category::Other, done: true, items: root.rest_items.max(1) });
        }
        tiles.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        if tiles.len() > LIVE_TILES {
            let extra = tiles.split_off(LIVE_TILES - 1);
            let items: u32 = extra.iter().map(|t| t.items.max(1)).sum();
            tiles.push(LiveTile { name: smaller_items(items), bytes: extra.iter().map(|t| t.bytes).sum(), cat: Category::Other, done: true, items });
        }
        let mut stats = self.stats.clone();
        stats.elapsed = started.elapsed();
        Live { stats, current: self.open.last().map(|o| o.path.display().to_string()).unwrap_or_default(), tiles, cats: self.cats }
    }
}

/// "83,907 smaller items".
pub(crate) fn smaller_items(n: u32) -> String {
    if n == 1 {
        "1 smaller item".to_string()
    } else {
        format!("{} smaller items", group(u64::from(n)))
    }
}

/// Reads `root` and everything below it (folder by folder, depth first, in name order). `publish`
/// gets the live state every `publish_every`; None when `cancelled` said stop.
pub(crate) fn scan(root: &Path, name: &str, opts: &ScanOptions, cancelled: &dyn Fn() -> bool, publish: &mut dyn FnMut(Live)) -> Option<Scanned> {
    let started = Instant::now();
    let mut walker = Walker {
        opts,
        tree: Tree::new(root.to_path_buf(), name),
        stats: Stats::default(),
        largest: Vec::new(),
        open: Vec::new(),
        todo: vec![Step::Enter { path: root.to_path_buf(), node: Some(0), depth: 0, dev: is_dev_dir(name) }],
        listing: Listing::default(),
        kept: Vec::new(),
        cats: [0; 8],
    };
    publish(walker.live(started));
    let mut published = Instant::now();
    while let Some(step) = walker.todo.pop() {
        if cancelled() {
            return None;
        }
        match step {
            Step::Enter { path, node, depth, dev } => walker.enter(path, node, depth, dev),
            Step::Exit => walker.exit(),
        }
        if published.elapsed() >= opts.publish_every {
            publish(walker.live(started));
            published = Instant::now();
        }
    }
    walker.stats.elapsed = started.elapsed();
    Some(Scanned { tree: walker.tree, stats: walker.stats, largest: walker.largest })
}

// ---------------------------------------------------------------------------- the advice ----

/// What the spoken verdict is made of.
#[derive(Clone, Debug, Default)]
pub(crate) struct Summary {
    /// The drive's letter when a whole drive was read.
    pub drive: Option<char>,
    pub total: u64,
    pub free: u64,
    /// The folder that was read, as said aloud ("your Downloads folder").
    pub folder: Option<String>,
    pub scanned: u64,
    /// The largest item (a folder or a file).
    pub top: Option<(String, u64)>,
    /// The largest single file: its size and type.
    pub biggest: Option<(u64, Category)>,
    pub recycle: u64,
    pub temp: u64,
}

/// JAI's verdict, two sentences at most: "Your C drive is nearly full, Sir: 231 of 234 GB.
/// Users takes 127 GB, there's a single video over 10 GB, and emptying the Recycle Bin would
/// free 9 GB."
pub(crate) fn advice(s: &Summary) -> String {
    let first = match (s.drive, &s.folder) {
        (Some(letter), _) if s.total > 0 => {
            let used = s.total.saturating_sub(s.free);
            if used as f64 >= s.total as f64 * NEARLY_FULL {
                format!("Your {letter} drive is nearly full, Sir: {}.", used_of(used, s.total))
            } else {
                format!("Your {letter} drive has {} free of {}, Sir.", spoken_size(s.free), spoken_size(s.total))
            }
        }
        (_, Some(folder)) => {
            let mut chars = folder.chars();
            let folder: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
            format!("{folder} holds {}, Sir.", spoken_size(s.scanned))
        }
        _ => format!("I've read {}, Sir.", spoken_size(s.scanned)),
    };
    let mut clauses = Vec::new();
    if let Some((name, size)) = s.top.as_ref().filter(|(_, size)| *size > 0) {
        clauses.push(format!("{name} takes {}", spoken_size(*size)));
    }
    if let Some((size, cat)) = s.biggest {
        if let Some(over) = [100u64, 50, 20, 10, 5, 2].into_iter().find(|g| size >= g * GB) {
            clauses.push(format!("there's a single {} over {over} GB", cat.noun()));
        }
    }
    if s.recycle >= 512 * MB {
        clauses.push(format!("emptying the Recycle Bin would free {}", spoken_size(s.recycle)));
    }
    if s.temp >= GB {
        clauses.push(format!("clearing the temp files would free {}", spoken_size(s.temp)));
    }
    clauses.truncate(3);
    let second = match clauses.len() {
        0 => return first,
        1 => clauses[0].clone(),
        2 => format!("{} and {}", clauses[0], clauses[1]),
        _ => format!("{}, {}, and {}", clauses[0], clauses[1], clauses[2]),
    };
    let mut chars = second.chars();
    let second: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
    format!("{first} {second}.")
}

fn summarize(s: &State) -> Summary {
    let tree = s.tree.as_deref();
    let top = tree.and_then(|t| {
        t.nodes[0].children.iter().find(|&&c| t.nodes[c as usize].kind != Kind::Hidden && !t.name(c).starts_with('$')).map(|&c| (t.name(c).to_string(), t.nodes[c as usize].size))
    });
    let temp = tree.map_or(0, |t| {
        let windows_temp = default_root().join("Windows").join("Temp");
        [temp_folder(), windows_temp].iter().filter_map(|p| t.find(p)).filter(|&id| id != 0).map(|id| t.nodes[id as usize].size).sum()
    });
    let bin = tree.and_then(|t| t.find(&t.root.join(RECYCLE_NAME))).filter(|&id| id != 0).and_then(|id| tree.map(|t| t.nodes[id as usize].size)).unwrap_or(0);
    let drive = s.drive.filter(|_| s.is_drive);
    Summary {
        drive: drive.map(|d| d.letter),
        total: drive.map_or(0, |d| d.total),
        free: drive.map_or(0, |d| d.free),
        folder: (!s.is_drive).then(|| spoken_target(&s.root)),
        scanned: s.stats.bytes,
        top,
        biggest: s.largest.first().map(|b| (b.size, b.cat)),
        recycle: if s.is_drive { s.recycle.unwrap_or(0).max(bin) } else { 0 },
        temp,
    }
}

// -------------------------------------------------------------------------------- state ----

/// Where the map is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Scanning,
    Done,
    Failed,
}

#[derive(Default)]
struct State {
    phase: Phase,
    demo: bool,
    root: PathBuf,
    label: String,
    is_drive: bool,
    drive: Option<Drive>,
    recycle: Option<u64>,
    expected: Option<u64>,
    live: Live,
    started: Option<Instant>,
    /// The demo's fixed clock.
    demo_elapsed: Option<Duration>,
    stats: Stats,
    tree: Option<Arc<Tree>>,
    largest: Vec<BigFile>,
    view: Vec<u32>,
    graft: Option<u32>,
    version: u64,
    finished: Option<Instant>,
    error: Option<String>,
}

/// What the panel draws, copied once per frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub phase: Phase,
    /// The demo drive (verifier, dry runs): nothing was read.
    pub demo: bool,
    /// "C:\" or the folder's whole path.
    pub label: String,
    pub is_drive: bool,
    pub drive: Option<Drive>,
    pub recycle: Option<u64>,
    /// The bytes the running scan will find (a drive: its used space).
    pub expected: Option<u64>,
    pub live: Live,
    /// Since the scan began (finished: how long it took).
    pub elapsed: Duration,
    pub stats: Stats,
    pub tree: Option<Arc<Tree>>,
    /// The folders opened below the root (the last one is on screen).
    pub view: Vec<u32>,
    /// A folder being read again.
    pub graft: Option<u32>,
    /// Changes whenever anything above does.
    pub version: u64,
    pub error: Option<String>,
}

impl Snapshot {
    /// The folder on screen.
    pub(crate) fn current(&self) -> u32 {
        self.view.last().copied().unwrap_or(0)
    }
}

fn snapshot_of(s: &State) -> Snapshot {
    let elapsed = match s.phase {
        Phase::Scanning => s.demo_elapsed.or_else(|| s.started.map(|t| t.elapsed())).unwrap_or_default(),
        _ => s.demo_elapsed.unwrap_or(s.stats.elapsed),
    };
    Snapshot {
        phase: s.phase,
        demo: s.demo,
        label: s.label.clone(),
        is_drive: s.is_drive,
        drive: s.drive,
        recycle: s.recycle,
        expected: s.expected,
        live: s.live.clone(),
        elapsed,
        stats: s.stats.clone(),
        tree: s.tree.clone(),
        view: s.view.clone(),
        graft: s.graft,
        version: s.version,
        error: s.error.clone(),
    }
}

/// The map as it is now (the panel calls this every frame).
pub(crate) fn snapshot() -> Snapshot {
    snapshot_of(&state())
}

/// The disk map is up (a block on the board, or its own window).
pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Closes the disk map (its `[ CLOSE ]`, "close the disk map"): a running scan stops and the
/// tree is let go.
pub fn close() {
    OPEN.store(false, Ordering::Release);
    SCAN_GEN.fetch_add(1, Ordering::AcqRel);
    let mut s = state();
    let version = s.version + 1;
    *s = State { version, ..State::default() };
}

/// A scan to run.
struct Job {
    root: PathBuf,
    /// The tree's root name.
    name: String,
    label: String,
    is_drive: bool,
    /// JAI says the verdict when it is done.
    announce: bool,
    /// The folder of the finished tree it reads again (the map stays up meanwhile).
    graft: Option<u32>,
}

impl Job {
    fn whole(root: PathBuf, announce: bool) -> Self {
        Job { name: root_name(&root), label: label_of(&root), is_drive: is_drive_root(&root), root, announce, graft: None }
    }
}

/// Starts `job` on its own thread (a running scan stops).
fn start(job: Job) {
    let generation = SCAN_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    {
        let mut s = state();
        s.live = Live::default();
        s.started = Some(Instant::now());
        s.error = None;
        s.graft = job.graft;
        s.version += 1;
        if job.graft.is_none() {
            s.phase = Phase::Scanning;
            s.demo_elapsed = None;
            s.expected = None;
            s.demo = false;
            s.root = job.root.clone();
            s.label = job.label.clone();
            s.is_drive = job.is_drive;
            s.tree = None;
            s.view.clear();
            s.largest.clear();
            s.stats = Stats::default();
            s.drive = None;
            s.recycle = None;
            s.finished = None;
        }
    }
    info!("[DISK MAP] Reading {}", job.root.display());
    let spawned = std::thread::Builder::new().name("disk-map-scan".into()).spawn(move || work(generation, job));
    if let Err(e) = spawned {
        warn!("[DISK MAP] Could not start the scan: {e}");
        let mut s = state();
        s.phase = Phase::Failed;
        s.error = Some(format!("The scan could not start: {e}"));
        s.version += 1;
    }
    crate::gui::refresh();
}

/// The scan thread: the drive's size, the walk, then the tree and the verdict.
fn work(generation: u64, job: Job) {
    crate::platform::lower_thread_priority();
    let current = || SCAN_GEN.load(Ordering::Acquire) == generation;
    let drive = drive_letter(&job.root).and_then(|letter| disk_space(&job.root).map(|(total, free)| Drive { letter, total, free }));
    let recycle = if job.graft.is_none() && job.is_drive { drive.and_then(|d| recycle_bin(d.letter)) } else { None };
    {
        let mut s = state();
        if !current() {
            return;
        }
        if job.graft.is_none() {
            s.drive = drive;
            s.recycle = recycle;
            if job.is_drive {
                s.expected = drive.map(|d| d.used());
            }
        }
        s.version += 1;
    }
    let mut woke = Instant::now();
    let scanned = scan(&job.root, &job.name, &ScanOptions::default(), &|| !current(), &mut |live| {
        {
            let mut s = state();
            if !current() {
                return;
            }
            s.live = live;
            s.version += 1;
        }
        // (The HUD is woken ten times a second at most.)
        if woke.elapsed() >= PUBLISH_EVERY {
            crate::gui::refresh();
            woke = Instant::now();
        }
    });
    let Some(mut scanned) = scanned else {
        info!("[DISK MAP] The scan of {} stopped.", job.root.display());
        return;
    };
    let st = &scanned.stats;
    info!(
        "[DISK MAP] {}: {} in {} files and {} folders, {} unreadable, {} in the cloud, {:.1} s",
        job.root.display(),
        format_size(st.bytes),
        group(st.files),
        group(st.dirs),
        group(st.unreadable),
        format_size(st.cloud_bytes),
        st.elapsed.as_secs_f64()
    );
    let line = {
        let mut s = state();
        if !current() {
            return;
        }
        match job.graft {
            Some(at) => {
                if let Some(tree) = s.tree.take() {
                    let mut tree = Arc::try_unwrap(tree).unwrap_or_else(|shared| (*shared).clone());
                    graft(&mut tree, at, scanned.tree);
                    s.tree = Some(Arc::new(tree));
                }
            }
            None => {
                if let Some(d) = s.drive.filter(|_| job.is_drive) {
                    add_hidden(&mut scanned.tree, d.used(), scanned.stats.bytes);
                }
                if let Some(b) = scanned.largest.first() {
                    info!("[DISK MAP] Largest file: {} ({})", b.path, format_size(b.size));
                }
                s.tree = Some(Arc::new(scanned.tree));
                s.stats = scanned.stats;
                s.largest = scanned.largest;
            }
        }
        s.phase = Phase::Done;
        s.graft = None;
        s.finished = Some(Instant::now());
        s.version += 1;
        job.announce.then(|| advice(&summarize(&s)))
    };
    crate::gui::refresh();
    if let Some(line) = line {
        info!("[DISK MAP] {line}");
        crate::announcements::say(line);
    }
}

/// Opens folder `node` of the tree on screen (a click on its tile); a folder whose subfolders
/// were folded in is read again.
pub(crate) fn drill(node: u32) {
    let job = {
        let mut s = state();
        let Some(tree) = s.tree.clone() else { return };
        let Some(n) = tree.node(node) else { return };
        if node == 0 || n.kind != Kind::Dir || s.graft.is_some() {
            return;
        }
        s.view = tree.chain(node);
        s.version += 1;
        (n.truncated && !s.demo && s.phase == Phase::Done).then(|| Job {
            root: tree.path(node),
            name: tree.name(node).to_string(),
            label: s.label.clone(),
            is_drive: false,
            announce: false,
            graft: Some(node),
        })
    };
    if let Some(job) = job {
        start(job);
    }
    crate::gui::refresh();
}

/// A folder being read again stops when Sir leaves it.
fn drop_graft(s: &mut State) {
    if s.graft.take().is_some() {
        SCAN_GEN.fetch_add(1, Ordering::AcqRel);
    }
}

/// One folder up (right-click, `< BACK`, "back"); false at the root.
pub(crate) fn back() -> bool {
    let mut s = state();
    if s.view.pop().is_none() {
        return false;
    }
    drop_graft(&mut s);
    s.version += 1;
    drop(s);
    crate::gui::refresh();
    true
}

/// Back to the breadcrumb's `depth`-th folder (0: the root).
pub(crate) fn jump(depth: usize) {
    let mut s = state();
    if depth >= s.view.len() {
        return;
    }
    s.view.truncate(depth);
    drop_graft(&mut s);
    s.version += 1;
    drop(s);
    crate::gui::refresh();
}

/// `RESCAN`: the root read again (no verdict spoken: Sir is looking at it).
pub(crate) fn rescan() {
    let (demo, root) = {
        let s = state();
        (s.demo, s.root.clone())
    };
    if demo || demo_mode() {
        open_demo(false);
    } else if !root.as_os_str().is_empty() {
        start(Job::whole(root, false));
    }
    crate::gui::refresh();
}

/// `EXPLORER`: the folder on screen in Explorer (never for the demo drive or in a test run).
pub(crate) fn open_in_explorer() {
    let path = {
        let s = state();
        if s.demo || s.root.as_os_str().is_empty() {
            return;
        }
        let at = s.view.last().copied().unwrap_or(0);
        s.tree.as_ref().map_or_else(|| s.root.clone(), |t| t.path(at))
    };
    if demo_mode() {
        return;
    }
    #[cfg(windows)]
    {
        if let Err(e) = std::process::Command::new("explorer.exe").arg(&path).spawn() {
            warn!("[DISK MAP] Explorer did not open {}: {e}", path.display());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

/// The verifier, dry runs and unit tests never read a disk.
fn demo_mode() -> bool {
    cfg!(test) || crate::live_self_tester::running() || crate::live_self_tester::dry()
}

/// `open_jarvis_view {"view": "disk_map", "path": …, "action": "show" | "rescan" | "back"}`:
/// opens the map and starts a scan (or reads it again, or goes one folder up). The result is
/// the display text and a "Spoken:" line; the verdict follows once the scan is done.
pub async fn run(args: &Value) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("show").trim().to_ascii_lowercase();
    let path = args.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
    match action.as_str() {
        "back" | "up" | "go_back" | "out" => Ok(back_action()),
        "rescan" | "refresh" | "scan_again" | "again" => rescan_action(path),
        _ => show_action(path),
    }
}

fn show_action(path: Option<&str>) -> Result<String, String> {
    if demo_mode() {
        open_demo(false);
        crate::gui::show();
        crate::gui::refresh();
        return Ok("Disk map of C:\\ (the demo drive: nothing was read).\nSpoken: I'll check your drive, Sir. A live map of it is on your screen.".to_string());
    }
    let root = resolve(path)?;
    let (label, spoken) = (label_of(&root), spoken_target(&root));
    let reuse = {
        let s = state();
        let recent = s.phase == Phase::Scanning || (s.phase == Phase::Done && s.finished.is_some_and(|t| t.elapsed() < REUSE_FOR));
        (is_open() && !s.demo && same_path(&s.root, &root) && recent).then(|| if s.phase == Phase::Done { advice(&summarize(&s)) } else { format!("I'm still reading {spoken}, Sir. The map fills in as I go.") })
    };
    OPEN.store(true, Ordering::Release);
    crate::gui::show();
    if let Some(line) = reuse {
        crate::gui::refresh();
        return Ok(format!("Disk map of {label}: on screen.\nSpoken: {line}"));
    }
    start(Job::whole(root, true));
    Ok(format!("Disk map of {label}: scanning.\nSpoken: I'll check {spoken}, Sir. A live map of it is on your screen."))
}

fn rescan_action(path: Option<&str>) -> Result<String, String> {
    if demo_mode() {
        open_demo(false);
        crate::gui::show();
        crate::gui::refresh();
        return Ok("Disk map of C:\\ (the demo drive: nothing was read).\nSpoken: Reading your drive again, Sir.".to_string());
    }
    let known = state().root.clone();
    let root = match path {
        Some(p) => resolve(Some(p))?,
        None if !known.as_os_str().is_empty() => known,
        None => resolve(None)?,
    };
    let (label, spoken) = (label_of(&root), spoken_target(&root));
    OPEN.store(true, Ordering::Release);
    crate::gui::show();
    start(Job::whole(root, true));
    Ok(format!("Disk map of {label}: scanning again.\nSpoken: Reading {spoken} again, Sir. The map is on your screen."))
}

fn back_action() -> String {
    if !is_open() {
        return "The disk map is not open.\nSpoken: The disk map isn't open, Sir.".to_string();
    }
    let moved = back();
    let s = snapshot();
    let here = s.tree.as_ref().map_or_else(|| s.label.clone(), |t| t.name(s.current()).to_string());
    let line = if moved { format!("Back to {}, Sir.", speakable(&here)) } else { "That's already the top of the map, Sir.".to_string() };
    format!("Disk map: {here}.\nSpoken: {line}")
}

// --------------------------------------------------------------------------------- demo ----

/// Builds the demo drive.
struct Demo {
    tree: Tree,
    /// The types of each folder's smaller items.
    own: HashMap<u32, [u64; 8]>,
}

fn gib(gb: f64) -> u64 {
    (gb * GB as f64) as u64
}

impl Demo {
    fn dir(&mut self, parent: u32, name: &str) -> u32 {
        self.tree.add(parent, name, Kind::Dir, 0, Category::Other)
    }

    fn file(&mut self, parent: u32, name: &str, gb: f64, cat: Category) {
        self.tree.add(parent, name, Kind::File, gib(gb), cat);
    }

    /// Smaller items of folder `at`: `gb` of type `cat` in `items` entries.
    fn rest(&mut self, at: u32, gb: f64, items: u32, cat: Category) {
        let bytes = gib(gb);
        let n = &mut self.tree.nodes[at as usize];
        n.rest_bytes += bytes;
        n.rest_items += items;
        self.own.entry(at).or_insert([0; 8])[cat.index()] += bytes;
    }

    /// Everything the demo holds so far.
    fn total(&self) -> u64 {
        self.tree.nodes.iter().map(|n| if n.kind == Kind::File { n.size } else { n.rest_bytes }).sum()
    }

    /// Sizes, counts and types summed up from the leaves.
    fn roll(&mut self, id: u32) -> [u64; 8] {
        let mut cats = self.own.get(&id).copied().unwrap_or([0; 8]);
        let (mut size, mut files, mut dirs) = (self.tree.nodes[id as usize].rest_bytes, self.tree.nodes[id as usize].rest_items, 0u32);
        for c in self.tree.nodes[id as usize].children.clone() {
            let below = match self.tree.nodes[c as usize].kind {
                Kind::Dir => self.roll(c),
                _ => {
                    let mut one = [0u64; 8];
                    one[self.tree.nodes[c as usize].cat.index()] = self.tree.nodes[c as usize].size;
                    one
                }
            };
            for (a, b) in cats.iter_mut().zip(below) {
                *a += b;
            }
            let n = &self.tree.nodes[c as usize];
            size += n.size;
            files += n.files;
            if n.kind == Kind::Dir {
                dirs += n.dirs + 1;
            }
        }
        let n = &mut self.tree.nodes[id as usize];
        n.size = size;
        n.files = files;
        n.dirs = dirs;
        n.cat = dominant(&cats);
        n.cats = Some(Box::new(cats));
        self.tree.sort_children(id);
        cats
    }
}

/// The demo drive: a nearly full C:\ of 234 GB with a placeholder user, as in the reference.
fn demo_drive() -> (Tree, Stats, Drive, Vec<BigFile>) {
    use Category::*;
    let mut d = Demo { tree: Tree::new(PathBuf::from("C:\\"), "C:\\"), own: HashMap::new() };
    let users = d.dir(0, "Users");
    let max = d.dir(users, "max");
    let appdata = d.dir(max, "AppData");
    let local = d.dir(appdata, "Local");
    let packages = d.dir(local, "Packages");
    d.rest(packages, 9.0, 61_230, Other);
    d.rest(packages, 5.2, 8_115, Programs);
    let docker = d.dir(local, "Docker");
    d.file(docker, "ext4.vhdx", 8.1, Archives);
    let temp = d.dir(local, "Temp");
    d.rest(temp, 7.0, 14_920, Other);
    d.rest(temp, 2.4, 310, Archives);
    let nvidia = d.dir(local, "NVIDIA");
    d.rest(nvidia, 5.3, 2_204, Programs);
    let microsoft = d.dir(local, "Microsoft");
    d.rest(microsoft, 4.6, 19_877, Programs);
    let jetbrains = d.dir(local, "JetBrains");
    d.rest(jetbrains, 3.2, 12_406, Code);
    d.rest(local, 6.5, 40_118, Other);
    let roaming = d.dir(appdata, "Roaming");
    let discord = d.dir(roaming, "discord");
    d.rest(discord, 1.2, 1_342, Programs);
    let code = d.dir(roaming, "Code");
    d.rest(code, 2.4, 9_871, Code);
    d.rest(roaming, 6.9, 22_190, Other);
    let videos = d.dir(max, "Videos");
    d.file(videos, "2026-10-31 Studio Session.mp4", 12.4, Video);
    let captures = d.dir(videos, "Captures");
    d.rest(captures, 6.8, 214, Video);
    d.rest(videos, 4.8, 96, Video);
    let downloads = d.dir(max, "Downloads");
    let setups = d.dir(downloads, "Setups");
    d.rest(setups, 6.2, 48, Programs);
    d.file(downloads, "ubuntu-24.04-desktop-amd64.iso", 5.9, Archives);
    d.rest(downloads, 3.0, 311, Archives);
    d.rest(downloads, 1.8, 902, Other);
    d.rest(downloads, 0.4, 655, Images);
    d.rest(downloads, 0.3, 240, Documents);
    let source = d.dir(max, "source");
    let orbit = d.dir(source, "orbit");
    let target = d.dir(orbit, "target");
    d.rest(target, 5.1, 18_331, Code);
    let modules = d.dir(orbit, "node_modules");
    d.rest(modules, 2.3, 41_207, Code);
    d.rest(source, 2.4, 9_512, Code);
    let desktop = d.dir(max, "Desktop");
    let new_folder = d.dir(desktop, "New folder (6)");
    d.rest(new_folder, 4.0, 1_220, Other);
    d.rest(desktop, 1.2, 341, Images);
    d.rest(desktop, 0.4, 87, Documents);
    d.rest(desktop, 0.6, 64, Other);
    let documents = d.dir(max, "Documents");
    d.rest(documents, 1.2, 31, Video);
    d.rest(documents, 1.6, 7_455, Code);
    d.rest(documents, 0.4, 1_021, Documents);
    d.rest(documents, 0.9, 2_310, Other);
    let pictures = d.dir(max, "Pictures");
    d.rest(pictures, 2.2, 6_120, Images);
    d.rest(pictures, 0.4, 233, Other);
    let music = d.dir(max, "Music");
    d.rest(music, 0.13, 412, Audio);
    d.rest(max, 0.4, 1_904, Other);
    let public = d.dir(users, "Public");
    d.rest(public, 0.6, 2_870, Other);

    let windows = d.dir(0, "Windows");
    let winsxs = d.dir(windows, "WinSxS");
    d.rest(winsxs, 8.0, 64_120, Programs);
    d.rest(winsxs, 2.4, 19_787, Other);
    let system32 = d.dir(windows, "System32");
    d.rest(system32, 5.6, 17_311, Programs);
    d.rest(system32, 1.3, 9_402, Other);
    let wtemp = d.dir(windows, "Temp");
    d.rest(wtemp, 1.9, 120, Archives);
    d.rest(wtemp, 0.4, 2_015, Other);
    let assembly = d.dir(windows, "assembly");
    d.rest(assembly, 2.2, 11_877, Programs);
    let servicing = d.dir(windows, "servicing");
    d.rest(servicing, 1.7, 6_032, Archives);
    let syswow = d.dir(windows, "SysWOW64");
    d.rest(syswow, 1.6, 8_911, Programs);
    let installer = d.dir(windows, "Installer");
    d.rest(installer, 1.4, 652, Programs);
    d.rest(windows, 2.3, 30_418, Other);

    let program_data = d.dir(0, "ProgramData");
    let bluestacks = d.dir(program_data, "BlueStacks_nxt");
    d.file(bluestacks, "Data.vhdx", 7.6, Archives);
    d.rest(bluestacks, 0.6, 410, Programs);
    let pd_microsoft = d.dir(program_data, "Microsoft");
    d.rest(pd_microsoft, 3.8, 14_032, Other);
    d.rest(pd_microsoft, 1.5, 2_206, Programs);
    let pd_nvidia = d.dir(program_data, "NVIDIA");
    d.rest(pd_nvidia, 4.6, 702, Archives);
    let cache = d.dir(program_data, "Package Cache");
    d.rest(cache, 2.4, 1_310, Programs);
    d.rest(program_data, 1.5, 9_814, Other);

    let x86 = d.dir(0, "Program Files (x86)");
    let x86_microsoft = d.dir(x86, "Microsoft");
    d.rest(x86_microsoft, 3.9, 7_120, Programs);
    let studio = d.dir(x86, "Microsoft Visual Studio");
    d.rest(studio, 2.0, 21_455, Code);
    d.rest(studio, 1.3, 3_310, Programs);
    let steam = d.dir(x86, "Steam");
    d.rest(steam, 4.1, 3_806, Other);
    d.rest(steam, 1.1, 402, Programs);
    d.rest(x86, 2.1, 6_118, Programs);

    let program_files = d.dir(0, "Program Files");
    for (name, gb, items) in [("Microsoft Office", 3.1, 4_870), ("NVIDIA Corporation", 2.8, 2_119), ("Docker", 2.6, 1_655), ("WindowsApps", 2.4, 12_007)] {
        let at = d.dir(program_files, name);
        d.rest(at, gb, items, Programs);
    }
    d.rest(program_files, 1.1, 3_410, Programs);

    let bin = d.dir(0, RECYCLE_NAME);
    let sid = d.dir(bin, "S-1-5-21-1001");
    d.rest(sid, 4.0, 12, Video);
    d.rest(sid, 3.0, 58, Archives);
    d.rest(sid, 2.0, 1_402, Other);
    let onedrive_temp = d.dir(0, "OneDriveTemp");
    d.rest(onedrive_temp, 2.1, 37, Archives);
    d.file(0, "pagefile.sys", 6.0, Other);
    let recovery = d.dir(0, "Recovery");
    d.rest(recovery, 0.9, 12, Programs);
    let intel = d.dir(0, "Intel");
    d.rest(intel, 0.2, 140, Programs);
    // The root's own small files make the read part 223.4 GB.
    let read = gib(223.4);
    let so_far = d.total();
    if read > so_far {
        d.rest(0, (read - so_far) as f64 / GB as f64, 21, Other);
    }
    d.roll(0);
    let scanned = d.tree.nodes[0].size;
    let mut tree = d.tree;
    tree.nodes[0].files = 1_336_317;
    tree.nodes[0].dirs = 315_942;
    let drive = Drive { letter: 'C', total: gib(234.0), free: gib(3.2) };
    add_hidden(&mut tree, drive.used(), scanned);
    let stats = Stats { bytes: scanned, files: 1_336_317, dirs: 315_942, unreadable: 326, cloud_bytes: gib(1.6), cloud_files: 2_214, elapsed: Duration::from_millis(13_300) };
    let largest = vec![
        BigFile { path: r"C:\Users\max\Videos\2026-10-31 Studio Session.mp4".into(), size: gib(12.4), cat: Video },
        BigFile { path: r"C:\Users\max\AppData\Local\Docker\ext4.vhdx".into(), size: gib(8.1), cat: Archives },
        BigFile { path: r"C:\ProgramData\BlueStacks_nxt\Data.vhdx".into(), size: gib(7.6), cat: Archives },
    ];
    (tree, stats, drive, largest)
}

/// The demo drive as the panel sees it: finished, or 73 % of the way through (`scanning`).
pub(crate) fn demo_snapshot(scanning: bool) -> Snapshot {
    let (tree, stats, drive, _) = demo_drive();
    let mut snap = Snapshot {
        phase: Phase::Done,
        demo: true,
        label: "C:\\".to_string(),
        is_drive: true,
        drive: Some(drive),
        recycle: Some(gib(9.0)),
        expected: Some(drive.used()),
        live: Live::default(),
        elapsed: stats.elapsed,
        stats,
        tree: None,
        view: Vec::new(),
        graft: None,
        version: 1,
        error: None,
    };
    if !scanning {
        snap.tree = Some(Arc::new(tree));
        return snap;
    }
    // Read in name order up to Users, which is being read; Windows is still to come.
    let read = gib(168.8);
    let mut top: Vec<u32> = tree.nodes[0].children.iter().copied().filter(|&c| tree.nodes[c as usize].kind != Kind::Hidden).collect();
    top.sort_by_key(|&c| tree.name(c).to_lowercase());
    let mut tiles = Vec::new();
    let mut cats = [0u64; 8];
    let mut before = 0u64;
    for &c in top.iter().filter(|&&c| tree.name(c).to_lowercase().as_str() < "users") {
        let n = &tree.nodes[c as usize];
        before += n.size;
        for (sum, add) in cats.iter_mut().zip(tree.cats(c)) {
            *sum += add;
        }
        tiles.push(LiveTile { name: tree.name(c).to_string(), bytes: n.size, cat: n.cat, done: true, items: 0 });
    }
    let root = &tree.nodes[0];
    if root.rest_bytes > 0 {
        before += root.rest_bytes;
        cats[Category::Other.index()] += root.rest_bytes;
        tiles.push(LiveTile { name: smaller_items(root.rest_items), bytes: root.rest_bytes, cat: Category::Other, done: true, items: root.rest_items });
    }
    // Users so far: its share of what it holds, by type.
    let partial = read.saturating_sub(before);
    if let Some(users) = tree.find(Path::new("C:\\Users")) {
        let whole = tree.nodes[users as usize].size.max(1);
        for (sum, add) in cats.iter_mut().zip(tree.cats(users)) {
            *sum += (add as f64 * partial as f64 / whole as f64) as u64;
        }
        tiles.push(LiveTile { name: "Users".to_string(), bytes: partial, cat: tree.nodes[users as usize].cat, done: false, items: 0 });
    }
    tiles.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    snap.phase = Phase::Scanning;
    snap.elapsed = Duration::from_secs(16);
    snap.live = Live {
        stats: Stats { bytes: read, files: 1_005_950, dirs: 212_227, unreadable: 241, cloud_bytes: gib(1.2), cloud_files: 1_630, elapsed: Duration::from_secs(16) },
        current: r"C:\Users\max\AppData\Local\Packages\Microsoft.WindowsStore_8wekyb3d8bbwe\LocalCache\Local\Microsoft\Windows\INetCache\IE\Low".to_string(),
        tiles,
        cats,
    };
    snap.stats = Stats::default();
    snap
}

/// Opens the demo drive (finished, or mid-scan) in place of a real one.
pub(crate) fn open_demo(scanning: bool) {
    SCAN_GEN.fetch_add(1, Ordering::AcqRel);
    let (_, _, _, largest) = demo_drive();
    let d = demo_snapshot(scanning);
    let mut s = state();
    let version = s.version + 1;
    *s = State {
        phase: d.phase,
        demo: true,
        root: PathBuf::from("C:\\"),
        label: d.label,
        is_drive: true,
        drive: d.drive,
        recycle: d.recycle,
        expected: d.expected,
        live: d.live,
        started: None,
        demo_elapsed: Some(d.elapsed),
        stats: d.stats,
        tree: d.tree,
        largest,
        view: Vec::new(),
        graft: None,
        version,
        finished: (!scanning).then(Instant::now),
        error: None,
    };
    OPEN.store(true, Ordering::Release);
}

// -------------------------------------------------------------------------------- tests ----

/// Tests that open, drive or close the one disk map take turns.
#[cfg(test)]
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sizes_and_counts_read_like_the_reference() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_500), "1.5 KB");
        assert_eq!(format_size(gib(1.6)), "1.6 GB");
        assert_eq!(format_size(754 * MB), "754 MB");
        assert_eq!(format_size(130 * MB), "130 MB");
        assert_eq!(format_size(gib(84.9)), "84.9 GB");
        assert_eq!(format_size(127 * GB), "127 GB");
        assert_eq!(format_size(9 * GB), "9.0 GB");
        assert_eq!(format_size(gib(62.0)), "62.0 GB");
        assert_eq!(format_size(1000 * MB), "0.98 GB");
        assert_eq!(format_size(2 * 1024 * GB), "2.0 TB");
        assert_eq!(spoken_size(9 * GB), "9 GB");
        assert_eq!(spoken_size(gib(3.2)), "3.2 GB");
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_336_317), "1,336,317");
        assert_eq!(group(315_942), "315,942");
        assert_eq!(smaller_items(83_907), "83,907 smaller items");
        assert_eq!(smaller_items(1), "1 smaller item");
    }

    #[test]
    fn file_types_follow_the_extension() {
        for (name, cat) in [
            ("setup.exe", Category::Programs),
            ("KERNEL32.DLL", Category::Programs),
            ("clip.MKV", Category::Video),
            ("2026-10-31 Studio Session.mp4", Category::Video),
            ("backup.7z", Category::Archives),
            ("ubuntu.iso", Category::Archives),
            ("main.rs", Category::Code),
            ("app.ts", Category::Code),
            ("Cargo.lock", Category::Code),
            ("jarvis.pdb", Category::Code),
            ("photo.JPEG", Category::Images),
            ("design.psd", Category::Images),
            ("report.pdf", Category::Documents),
            ("notes.md", Category::Documents),
            ("song.flac", Category::Audio),
            ("pagefile.sys", Category::Programs),
            ("README", Category::Other),
            (".gitignore", Category::Other),
            ("archive.", Category::Other),
            ("weird.ünï", Category::Other),
            ("data.verylongextension", Category::Other),
        ] {
            assert_eq!(category_of(name), cat, "{name}");
        }
        assert!(is_dev_dir("node_modules") && is_dev_dir(".GIT") && !is_dev_dir("Documents"));
        let mut cats = [0u64; 8];
        assert_eq!(dominant(&cats), Category::Other);
        cats[Category::Video.index()] = 5;
        cats[Category::Code.index()] = 9;
        assert_eq!(dominant(&cats), Category::Code);
    }

    #[test]
    fn a_path_names_a_drive_a_known_folder_or_itself() {
        let known = |name: &str| Some(PathBuf::from(format!("C:\\Users\\max\\{name}")));
        let home = PathBuf::from("C:\\Users\\max");
        let parse = |raw: &str| parse_target(raw, &known, Some(&home), 'C');
        if cfg!(windows) {
            assert_eq!(parse(""), Ok(PathBuf::from("C:\\")));
        }
        for raw in ["D", "d:", "D:\\", "d:/", "D drive", "my d drive", "drive D", "D: drive", "  \"D:\\\"  "] {
            assert_eq!(parse(raw), Ok(PathBuf::from("D:\\")), "{raw}");
        }
        for (raw, folder) in [("Downloads", "Downloads"), ("downloads", "Downloads"), ("my downloads folder", "Downloads"), ("Documents", "Documents"), ("the desktop", "Desktop"), ("Videos", "Videos"), ("pictures", "Pictures"), ("Music", "Music"), ("AppData", "AppData"), ("temp", "Temp")] {
            assert_eq!(parse(raw), Ok(PathBuf::from(format!("C:\\Users\\max\\{folder}"))), "{raw}");
        }
        assert_eq!(parse("home"), Ok(home.clone()));
        if cfg!(windows) {
            assert_eq!(parse("C:\\Users\\max\\Videos\\Captures"), Ok(PathBuf::from("C:\\Users\\max\\Videos\\Captures")));
            assert_eq!(parse("D:/Games/Steam"), Ok(PathBuf::from("D:\\Games\\Steam")));
            assert_eq!(parse("C:\\Users\\max\\..\\Public\\.\\Videos"), Ok(PathBuf::from("C:\\Users\\Public\\Videos")));
            assert_eq!(parse("~\\Downloads\\Setups"), Ok(PathBuf::from("C:\\Users\\max\\Downloads\\Setups")));
        }
        let none = |_: &str| None;
        assert!(parse_target("Downloads", &none, None, 'C').is_err());
        // What the panel and JAI call a root.
        assert!(is_drive_root(Path::new("D:\\")) && is_drive_root(Path::new("d:")) && !is_drive_root(Path::new("D:\\Games")));
        assert_eq!(drive_letter(Path::new("e:\\x")), Some('E'));
        assert_eq!(drive_letter(Path::new("relative")), None);
        assert_eq!(label_of(Path::new("D:\\")), "D:\\");
        assert_eq!(root_name(Path::new("C:\\Users\\max\\Downloads")), "Downloads");
        assert_eq!(spoken_target(Path::new("Z:\\")), "your Z drive");
        assert_eq!(spoken_target(Path::new("C:\\Users\\max\\Downloads")), "your Downloads folder");
        assert_eq!(spoken_target(Path::new("D:\\Games")), "the Games folder");
        assert_eq!(speakable("C:\\"), "the C drive");
        assert_eq!(speakable("Users"), "Users");
        assert!(same_path(Path::new("C:\\Users\\"), Path::new("c:\\users")));
    }

    #[test]
    fn the_verdict_names_what_fills_the_drive_in_two_sentences() {
        let full = Summary {
            drive: Some('C'),
            total: 234 * GB,
            free: gib(3.2),
            folder: None,
            scanned: gib(223.4),
            top: Some(("Users".into(), 127 * GB)),
            biggest: Some((gib(12.4), Category::Video)),
            recycle: 9 * GB,
            temp: 0,
        };
        assert_eq!(advice(&full), "Your C drive is nearly full, Sir: 231 of 234 GB. Users takes 127 GB, there's a single video over 10 GB, and emptying the Recycle Bin would free 9 GB.");
        let roomy = Summary { drive: Some('D'), total: 1_863 * GB, free: 1_240 * GB, top: Some(("Games".into(), 512 * GB)), ..Summary::default() };
        assert_eq!(advice(&roomy), "Your D drive has 1.2 TB free of 1.8 TB, Sir. Games takes 512 GB.");
        let folder = Summary { folder: Some("your Downloads folder".into()), scanned: gib(23.4), top: Some(("Setups".into(), gib(6.2))), biggest: Some((gib(5.9), Category::Archives)), temp: 2 * GB, ..Summary::default() };
        assert_eq!(advice(&folder), "Your Downloads folder holds 23.4 GB, Sir. Setups takes 6.2 GB, there's a single archive over 5 GB, and clearing the temp files would free 2 GB.");
        assert_eq!(advice(&Summary { folder: Some("the Games folder".into()), scanned: 0, ..Summary::default() }), "The Games folder holds 0 B, Sir.");
        for s in [&full, &roomy, &folder] {
            let line = advice(s);
            assert!(line.matches(". ").count() <= 1 && line.ends_with('.'), "two sentences at most: {line}");
        }
    }

    /// A folder of files and folders made for the test (deleted again, also when it fails).
    struct TempTree(PathBuf);

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(root: &Path, rel: &str, bytes: usize) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a folder");
        std::fs::write(path, vec![7u8; bytes]).expect("a file");
    }

    #[test]
    fn a_small_folder_is_read_whole_with_its_types_and_bounds() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = TempTree(std::env::temp_dir().join(format!("jarvis_disk_map_test_{}_{nanos}", std::process::id())));
        let root = &dir.0;
        write(root, "big.mp4", 4000);
        write(root, "notes.txt", 50);
        write(root, "a/one.zip", 3000);
        write(root, "a/two.zip", 2500);
        write(root, "a/three.zip", 2000);
        write(root, "a/deep/code.rs", 1200);
        write(root, "a/deep/deeper/song.mp3", 1100);
        write(root, "node_modules/pkg.js", 10);
        write(root, "node_modules/readme.md", 20);
        write(root, "node_modules/image.png", 30);
        std::fs::create_dir_all(root.join("empty")).expect("an empty folder");
        // A link to a folder is never followed (only where this account may make one).
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(root.join("a"), root.join("link_to_a"));
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(root.join("a"), root.join("link_to_a"));

        let opts = ScanOptions { keep_depth: 2, node_budget: 1_000, keep_files: 2, keep_min: 1_000, top_files: 3, publish_every: Duration::ZERO };
        let mut published = Vec::new();
        let scanned = scan(root, "test", &opts, &|| false, &mut |live| published.push(live)).expect("a finished scan");
        let (tree, stats) = (&scanned.tree, &scanned.stats);
        assert_eq!(stats.bytes, 13_910);
        assert_eq!(stats.files, 10);
        assert_eq!(stats.dirs, 5, "a, deep, deeper, node_modules, empty (the link is no folder of its own)");
        assert_eq!(stats.unreadable, 0);
        assert_eq!(stats.cloud_bytes, 0);
        let rootn = &tree.nodes[0];
        assert_eq!((rootn.size, rootn.files, rootn.dirs), (13_910, 10, 5));
        let cats = tree.cats(0);
        assert_eq!(cats[Category::Video.index()], 4000);
        assert_eq!(cats[Category::Documents.index()], 50);
        assert_eq!(cats[Category::Archives.index()], 7500);
        assert_eq!(cats[Category::Code.index()], 1260, "everything in node_modules is developer's");
        assert_eq!(cats[Category::Audio.index()], 1100);
        assert_eq!(rootn.cat, Category::Archives);
        // Largest first: a, big.mp4, node_modules, empty; notes.txt is a smaller item.
        let names: Vec<&str> = rootn.children.iter().map(|&c| tree.name(c)).collect();
        assert_eq!(names[..3], ["a", "big.mp4", "node_modules"]);
        assert_eq!((rootn.rest_bytes, rootn.rest_items), (50, 1));
        let a = tree.find(&root.join("a")).expect("a is a node");
        let an = &tree.nodes[a as usize];
        assert_eq!(an.size, 9_800);
        let a_names: Vec<&str> = an.children.iter().map(|&c| tree.name(c)).collect();
        assert_eq!(a_names, ["one.zip", "two.zip", "deep"], "two files of their own, the third is a smaller item");
        assert_eq!((an.rest_bytes, an.rest_items), (2_000, 1));
        let deep = tree.find(&root.join("a").join("deep")).expect("deep is a node");
        let dn = &tree.nodes[deep as usize];
        assert!(dn.truncated, "deeper lies past the depth kept");
        assert_eq!((dn.size, dn.rest_bytes, dn.rest_items), (2_300, 1_100, 1));
        assert!(tree.find(&root.join("a").join("deep").join("deeper")).is_none());
        assert_eq!(tree.path(deep), root.join("a").join("deep"));
        assert_eq!(tree.chain(deep), vec![a, deep]);
        assert_eq!(scanned.largest.len(), 3);
        assert!(scanned.largest[0].path.ends_with("big.mp4") && scanned.largest[0].size == 4000);
        // The live map grew to the whole folder.
        let last = published.last().expect("progress was published");
        assert!(published.len() > 3);
        assert!(last.stats.bytes <= 13_910 && last.tiles.iter().any(|t| t.name == "a"));
        assert!(published.windows(2).all(|w| w[0].stats.bytes <= w[1].stats.bytes), "the counters only grow");
        // A cancelled scan gives nothing.
        assert!(scan(root, "test", &opts, &|| true, &mut |_| {}).is_none());
        // Read again, folder `deep` takes the place of what was known of it.
        let mut grown = scanned.tree.clone();
        write(root, "a/deep/deeper/extra.wav", 900);
        let sub = scan(&root.join("a").join("deep"), "deep", &ScanOptions { publish_every: Duration::from_secs(60), ..opts.clone() }, &|| false, &mut |_| {}).expect("the folder read again");
        graft(&mut grown, deep, sub.tree);
        let gd = &grown.nodes[deep as usize];
        assert_eq!(gd.size, 3_200);
        assert!(!gd.truncated, "deeper is a node now");
        let deeper = grown.find(&root.join("a").join("deep").join("deeper")).expect("deeper after the second reading");
        assert_eq!(grown.nodes[deeper as usize].size, 2_000);
        assert_eq!(grown.nodes[deeper as usize].parent, deep);
        assert_eq!(grown.name(deeper), "deeper");
        assert_eq!(grown.nodes[a as usize].size, 10_700);
        assert_eq!(grown.nodes[0].size, 14_810);
        assert_eq!(grown.nodes[0].files, 11);
    }

    #[test]
    fn the_demo_drive_matches_the_reference() {
        let done = demo_snapshot(false);
        let tree = done.tree.as_deref().expect("the demo tree");
        let drive = done.drive.expect("the demo drive");
        assert_eq!((format_size(drive.used()), format_size(drive.total), format_size(drive.free)), ("231 GB".into(), "234 GB".into(), "3.2 GB".into()));
        assert!(drive.used_share() >= NEARLY_FULL);
        assert_eq!(format_size(done.stats.bytes), "223 GB");
        let top: Vec<(&str, String)> = tree.nodes[0].children.iter().take(7).map(|&c| (tree.name(c), format_size(tree.nodes[c as usize].size))).collect();
        assert_eq!(top[0], ("Users", "127 GB".to_string()));
        assert_eq!(top[1], ("Windows", "28.8 GB".to_string()));
        assert_eq!(top[2], ("ProgramData", "22.0 GB".to_string()));
        assert!(top.contains(&(HIDDEN_NAME, "7.4 GB".to_string())), "{top:?}");
        assert!(top.contains(&(RECYCLE_NAME, "9.0 GB".to_string())), "{top:?}");
        let mut s = State { tree: done.tree.clone(), is_drive: true, drive: done.drive, recycle: done.recycle, stats: done.stats.clone(), root: PathBuf::from("C:\\"), ..State::default() };
        s.largest = demo_drive().3;
        assert_eq!(advice(&summarize(&s)), "Your C drive is nearly full, Sir: 231 of 234 GB. Users takes 127 GB, there's a single video over 10 GB, and emptying the Recycle Bin would free 9 GB.");
        let scanning = demo_snapshot(true);
        assert_eq!(scanning.phase, Phase::Scanning);
        let to_go = scanning.expected.unwrap_or(0) - scanning.live.stats.bytes;
        assert_eq!(format_size(to_go), "62.0 GB");
        assert_eq!(format_size(scanning.live.stats.bytes), "169 GB");
        assert!(scanning.live.tiles.iter().any(|t| t.name == "Users" && !t.done));
        assert!(!scanning.live.tiles.iter().any(|t| t.name == "Windows"), "Windows is still to come");
    }

    #[test]
    fn in_a_test_run_the_map_opens_the_demo_and_reads_no_disk() {
        let _lock = test_lock();
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
        let out = rt.block_on(run(&json!({"view": "disk_map", "action": "show", "path": "D:\\"}))).expect("the map opens");
        assert!(out.ends_with("Spoken: I'll check your drive, Sir. A live map of it is on your screen."), "{out}");
        assert!(is_open());
        let s = snapshot();
        assert!(s.demo && s.phase == Phase::Done && s.tree.is_some());
        // Into Users and back out, by click and by voice.
        let users = s.tree.as_ref().and_then(|t| t.find(Path::new("C:\\Users"))).expect("Users");
        drill(users);
        assert_eq!(snapshot().view, vec![users]);
        let out = rt.block_on(run(&json!({"view": "disk_map", "action": "back"}))).expect("back");
        assert!(out.ends_with("Spoken: Back to the C drive, Sir."), "{out}");
        assert!(snapshot().view.is_empty());
        assert!(!back(), "the root has no way up");
        let out = rt.block_on(run(&json!({"view": "disk_map", "action": "rescan"}))).expect("rescan");
        assert!(out.contains("Spoken:"), "{out}");
        close();
        assert!(!is_open());
        assert_eq!(snapshot().phase, Phase::Idle);
        assert!(rt.block_on(run(&json!({"action": "back"}))).expect("back").contains("isn't open"));
    }
}
