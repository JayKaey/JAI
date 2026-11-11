//! Sir's files: an ultra-fast file finder, a content grep and a surgical text/code editor.
//!
//! * A background index (walkdir) of Sir's folders — Desktop, Documents, Downloads, Pictures,
//!   Videos, Music, OneDrive, project folders, Minecraft/CurseForge/Modrinth instances and every
//!   other fixed drive — answers `find_files` in milliseconds. Junk (node_modules, .git, target,
//!   AppData, caches, $Recycle.Bin, virtualenvs …) is never walked. Paths outside the index are
//!   searched live with ripgrep's parallel, .gitignore-aware walker (`ignore`).
//! * `search_inside_files`: parallel grep (binary files skipped) with line numbers and context.
//! * `read_text_file`, `create_or_overwrite_file`, `edit_file_surgical`, `undo_last_file_edit`:
//!   every write first copies the old version to `.jai_backups/` (next to JAI.exe), so
//!   nothing is ever lost and "undo that edit" always works. Snippets match across `\r\n` / `\n`
//!   and trailing-whitespace or indentation differences.

use crate::apps::{expand_env, home_dir, normalize};
use crate::tools::{bool_arg, str_arg};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use strsim::jaro_winkler;
use tracing::{info, warn};

const MAX_INDEXED: usize = 250_000;
const MAX_DEPTH: usize = 14;
const REINDEX_EVERY: Duration = Duration::from_secs(20 * 60);
const LIVE_BUDGET: Duration = Duration::from_millis(2500);
const GREP_BUDGET: Duration = Duration::from_secs(5);
const MAX_RESULTS: usize = 15;
const MAX_GREP_MATCHES: usize = 60;
const MAX_GREP_FILES: usize = 30;
const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;
const READ_LIMIT_LINES: usize = 400;
const READ_LIMIT_CHARS: usize = 16_000;
const DIFF_LINES: usize = 60;
pub const BACKUP_DIR: &str = ".jai_backups";

/// Folder names that are never walked (lower-case). Hidden (`.x`) and `$x` folders are skipped too.
const JUNK_DIRS: &[&str] = &[
    "node_modules", "target", "appdata", "system volume information", "__pycache__", "venv", "site-packages", "cache", "caches",
    "temp", "tmp", "windows", "program files", "program files (x86)", "programdata", "recovery", "perflogs", "msocache",
    "config.msi", "windowsapps", "build", "dist", "obj", "bower_components", "steamapps", "shadercache", "libraries",
];

// ------------------------------------------------------------------------- the index ----

struct Entry {
    path: PathBuf,
    /// Lower-case file name.
    name: Box<str>,
    is_dir: bool,
    size: u64,
    modified: SystemTime,
}

#[derive(Default)]
struct Index {
    entries: Vec<Entry>,
    roots: Vec<PathBuf>,
    built: Option<Instant>,
}

static INDEX: LazyLock<RwLock<Index>> = LazyLock::new(|| RwLock::new(Index::default()));
static BUILDING: AtomicBool = AtomicBool::new(false);

fn same_path_key(p: &Path) -> String {
    let s = p.display().to_string().replace('/', "\\");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// What the index covers: Sir's own folders, launcher instances and the other fixed drives.
pub fn roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        for d in [
            "Desktop", "Documents", "Downloads", "Pictures", "Videos", "Music", "Projects", "Coding", "Code", "dev", "source", "repos",
            "workspace", "git", "GitHub",
        ] {
            roots.push(home.join(d));
        }
        #[cfg(not(windows))]
        for d in [".minecraft", ".local/share/PrismLauncher/instances", ".local/share/ModrinthApp/profiles"] {
            roots.push(home.join(d));
        }
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        for d in [".minecraft", "PrismLauncher\\instances", "ModrinthApp\\profiles", "com.modrinth.theseus\\profiles", "ATLauncher\\instances", ".technic\\modpacks"] {
            roots.push(appdata.join(d));
        }
    }
    if let Some(od) = std::env::var_os("OneDrive").map(PathBuf::from) {
        roots.push(od);
    }
    #[cfg(windows)]
    roots.extend(fixed_drives());
    #[cfg(not(windows))]
    for base in ["/mnt".to_string(), format!("/media/{}", std::env::var("USER").unwrap_or_default())] {
        if let Ok(list) = std::fs::read_dir(&base) {
            roots.extend(list.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    roots.retain(|r| r.is_dir());
    // One copy of each (Windows paths are case-insensitive), and none inside another root.
    let mut seen = std::collections::HashSet::new();
    roots.retain(|r| seen.insert(same_path_key(r)));
    let all = roots.clone();
    roots.into_iter().filter(|r| !all.iter().any(|other| other != r && is_under(r, other))).collect()
}

/// Fixed drives other than the system drive (`D:\`, `E:\` …); the system drive is covered by
/// Sir's own folders.
#[cfg(windows)]
fn fixed_drives() -> Vec<PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
    // SAFETY: plain query of the drive bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = (b'A' + i) as char;
            if format!("{letter}:") == system {
                return None;
            }
            let root = format!("{letter}:\\");
            let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `wide` is a NUL-terminated string that outlives the call.
            let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
            (kind == DRIVE_FIXED).then(|| PathBuf::from(root))
        })
        .collect()
}

fn is_junk_dir(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('$') || JUNK_DIRS.contains(&name.to_lowercase().as_str())
}

fn is_junk_file(name: &str) -> bool {
    name.starts_with("~$") || name.eq_ignore_ascii_case("desktop.ini") || name.eq_ignore_ascii_case("thumbs.db")
}

/// Builds the index on a background thread and refreshes it every 20 minutes.
pub fn start() {
    let spawned = std::thread::Builder::new().name("jarvis-fileindex".into()).spawn(|| {
        crate::platform::lower_thread_priority();
        loop {
            rebuild();
            std::thread::sleep(REINDEX_EVERY);
        }
    });
    if let Err(e) = spawned {
        warn!("[FILES] Could not start the file indexer: {e}");
    }
}

/// Walks every root again (blocking; seconds for a large drive, so it runs in the background).
pub fn rebuild() {
    if BUILDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = Instant::now();
    let roots = roots();
    let mut entries: Vec<Entry> = Vec::with_capacity(64 * 1024);
    'roots: for root in &roots {
        let walker = walkdir::WalkDir::new(root)
            .max_depth(MAX_DEPTH)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !(e.file_type().is_dir() && is_junk_dir(&e.file_name().to_string_lossy())));
        for e in walker.flatten() {
            if e.depth() == 0 {
                continue;
            }
            let name = e.file_name().to_string_lossy();
            let is_dir = e.file_type().is_dir();
            if !is_dir && is_junk_file(&name) {
                continue;
            }
            if entries.len() >= MAX_INDEXED {
                warn!("[FILES] Index capped at {MAX_INDEXED} entries; the rest is searched live.");
                break 'roots;
            }
            let meta = e.metadata().ok();
            entries.push(Entry {
                name: name.to_lowercase().into_boxed_str(),
                is_dir,
                size: meta.as_ref().filter(|_| !is_dir).map(|m| m.len()).unwrap_or(0),
                modified: meta.and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH),
                path: e.into_path(),
            });
        }
    }
    let count = entries.len();
    *INDEX.write().unwrap_or_else(|e| e.into_inner()) = Index { entries, roots: roots.clone(), built: Some(Instant::now()) };
    BUILDING.store(false, Ordering::Release);
    info!("[FILES] Indexed {count} files and folders in {} locations in {} ms", roots.len(), started.elapsed().as_millis());
}

// ------------------------------------------------------------------------- matching ----

/// Parsed `find_files` request.
#[derive(Debug, Default, PartialEq)]
struct Query {
    words: Vec<String>,
    exts: Vec<String>,
    dirs_only: bool,
}

/// "my tax pdf" → words ["tax"], exts [pdf]; "kinetic horizon modpack folder" → words
/// ["kinetic", "horizon"], folders only ("modpack" only counts when nothing else names it).
fn parse_query(query: &str, extension: Option<&str>) -> Query {
    let mut q = Query::default();
    let mut soft: Vec<String> = Vec::new();
    for w in normalize(query).split_whitespace() {
        let exts: &[&str] = match w {
            "pdf" | "pdfs" => &["pdf"],
            "word" | "doc" | "docx" | "document" | "documents" | "dokument" => &["docx", "doc", "odt", "pdf", "txt", "md"],
            "excel" | "spreadsheet" | "spreadsheets" | "xlsx" | "tabelle" | "csv" => &["xlsx", "xls", "csv", "ods"],
            "powerpoint" | "presentation" | "slides" | "pptx" | "präsentation" => &["pptx", "ppt", "odp", "key"],
            "photo" | "photos" | "picture" | "pictures" | "image" | "images" | "bild" | "bilder" | "foto" | "fotos" => {
                &["jpg", "jpeg", "png", "heic", "webp", "gif", "bmp"]
            }
            // Screenshots are named after what they are ("Screenshot 2026-10-21 …").
            "screenshot" | "screenshots" | "bildschirmfoto" => {
                q.words.push("screenshot".to_string());
                &["png", "jpg", "jpeg", "webp", "bmp"]
            }
            "video" | "videos" | "movie" | "film" | "clip" => &["mp4", "mkv", "mov", "avi", "webm"],
            "song" | "music" | "mp3" | "audio" | "track" => &["mp3", "flac", "wav", "m4a", "ogg"],
            "zip" | "archive" | "archiv" => &["zip", "7z", "rar", "tar", "gz"],
            "text" | "txt" | "notes" | "note" | "notiz" => &["txt", "md"],
            "code" | "script" | "source" => &["rs", "py", "js", "ts", "ps1", "sh", "cpp", "c", "h", "cs", "java", "go", "lua"],
            "config" if !query.contains('.') => &["toml", "json", "yaml", "yml", "ini", "cfg", "conf", "properties"],
            "folder" | "folders" | "ordner" | "directory" | "verzeichnis" => {
                q.dirs_only = true;
                &[]
            }
            "modpack" | "modpacks" | "instance" | "repo" | "repository" | "project" | "projekt" => {
                q.dirs_only = true;
                soft.push(w.trim_end_matches('s').to_string());
                &[]
            }
            "file" | "files" | "datei" | "dateien" | "my" | "the" | "a" | "an" | "meine" | "mein" | "meinen" | "die" | "das" | "der"
            | "from" | "of" | "in" | "on" | "last" | "week" | "latest" | "newest" | "recent" | "neueste" | "called" | "named" => &[],
            other => {
                q.words.push(other.to_string());
                continue;
            }
        };
        q.exts.extend(exts.iter().map(|e| e.to_string()));
    }
    if q.words.is_empty() {
        q.words = soft;
    }
    if let Some(ext) = extension {
        q.exts = ext.split([',', ' ', ';']).map(|e| e.trim().trim_start_matches("*.").trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
        q.dirs_only = false;
    }
    q
}

/// How well a lower-case file name matches the words (0..1, ≥ 0.82 counts).
fn score(name: &str, words: &[String]) -> f64 {
    // Only a kind ("my pdfs", "recent photos"): every file of that kind, newest first.
    if words.is_empty() {
        return 0.9;
    }
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let plain: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
    if words.join(" ") == stem || words.join("") == plain {
        return 1.05;
    }
    let tokens: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let mut total = 0.0;
    for w in words {
        let w_plain: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
        let best = if name.contains(w.as_str()) || (!w_plain.is_empty() && plain.contains(&w_plain)) {
            1.0
        } else {
            // Typos keep their first letter; comparing only those keeps 250k names under 50 ms.
            let first = w.chars().next();
            tokens.iter().filter(|t| t.chars().next() == first).map(|t| jaro_winkler(t, w)).fold(0.0, f64::max)
        };
        total += best;
    }
    total / words.len() as f64
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: SystemTime,
    score: f64,
}

fn wanted(q: &Query, name: &str, is_dir: bool) -> Option<f64> {
    if q.dirs_only && !is_dir {
        return None;
    }
    if !q.exts.is_empty() {
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if is_dir || !q.exts.iter().any(|e| e == ext) {
            return None;
        }
    }
    let s = score(name, &q.words);
    (s >= 0.82).then_some(s)
}

/// Where `find_files` should look.
#[derive(Debug, Clone, PartialEq)]
enum Scope {
    Everywhere,
    /// Under this folder.
    Root(PathBuf),
    /// Paths containing this text ("in my modpacks folder").
    Containing(String),
}

/// "Downloads", "D:", "D drive", "C:\Users\…\Projects", "modpacks" → a scope.
fn scope_from_hint(hint: Option<&str>) -> Scope {
    let Some(raw) = hint.map(str::trim).filter(|h| !h.is_empty()) else { return Scope::Everywhere };
    let lower = raw.to_lowercase();
    let lower = lower.trim_end_matches(" folder").trim_end_matches(" ordner").trim_end_matches(" directory").trim();
    let lower = lower.trim_start_matches("my ").trim_start_matches("meine ").trim_start_matches("mein ").trim_start_matches("the ").trim();
    // "D drive", "drive D", "Laufwerk D", "D:".
    let letter = lower
        .strip_suffix(" drive")
        .or_else(|| lower.strip_prefix("drive "))
        .or_else(|| lower.strip_prefix("laufwerk "))
        .or_else(|| lower.strip_suffix(':'))
        .filter(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_alphabetic()));
    if let Some(l) = letter {
        if cfg!(windows) {
            return Scope::Root(PathBuf::from(format!("{}:\\", l.to_uppercase())));
        }
    }
    let expanded = PathBuf::from(expand_env(raw.trim_matches('"')));
    if expanded.is_absolute() && expanded.is_dir() {
        return Scope::Root(expanded);
    }
    if let Some(home) = home_dir() {
        let folder = match lower {
            "downloads" | "download" => Some("Downloads"),
            "documents" | "dokumente" | "docs" => Some("Documents"),
            "desktop" | "schreibtisch" => Some("Desktop"),
            "pictures" | "bilder" | "photos" | "fotos" => Some("Pictures"),
            "videos" | "video" => Some("Videos"),
            "music" | "musik" => Some("Music"),
            "home" | "user folder" | "benutzerordner" => Some(""),
            _ => None,
        };
        if let Some(f) = folder {
            return Scope::Root(if f.is_empty() { home } else { home.join(f) });
        }
        let under_home = home.join(raw);
        if under_home.is_dir() {
            return Scope::Root(under_home);
        }
    }
    Scope::Containing(lower.to_string())
}

/// `path` is `root` or inside it (whole path components: `D:\Code2` is not under `D:\Code`).
fn is_under(path: &Path, root: &Path) -> bool {
    let (p, r) = (same_path_key(path), same_path_key(root));
    let r = r.trim_end_matches('\\');
    p.starts_with(r) && (p.len() == r.len() || p[r.len()..].starts_with('\\'))
}

fn in_scope(scope: &Scope, path: &Path) -> bool {
    match scope {
        Scope::Everywhere => true,
        Scope::Root(root) => is_under(path, root),
        Scope::Containing(text) => path.to_string_lossy().to_lowercase().contains(text.as_str()),
    }
}

/// Scores the index in parallel; `None` when the index cannot answer (not built yet, or the
/// scope is a folder it does not cover).
fn search_index(q: &Query, scope: &Scope) -> Option<Vec<Hit>> {
    let index = INDEX.read().unwrap_or_else(|e| e.into_inner());
    index.built?;
    if let Scope::Root(root) = scope {
        if !index.roots.iter().any(|ir| is_under(root, ir)) {
            return None;
        }
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let chunk = index.entries.len().div_ceil(threads).max(1);
    let hits: Vec<Hit> = std::thread::scope(|s| {
        let handles: Vec<_> = index
            .entries
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .filter_map(|e| {
                            let score = wanted(q, &e.name, e.is_dir)?;
                            in_scope(scope, &e.path)
                                .then(|| Hit { path: e.path.clone(), is_dir: e.is_dir, size: e.size, modified: e.modified, score })
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    Some(hits)
}

/// Parallel live walk (ripgrep's walker) for folders outside the index or before it is built.
fn search_live(q: &Query, roots: &[PathBuf], scope: &Scope) -> Vec<Hit> {
    let Some((first, rest)) = roots.split_first() else { return Vec::new() };
    let mut builder = ignore::WalkBuilder::new(first);
    for r in rest {
        builder.add(r);
    }
    builder
        .hidden(true)
        .git_ignore(true)
        .ignore(true)
        .parents(false)
        .max_depth(Some(MAX_DEPTH))
        .threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8))
        .filter_entry(|e| e.depth() == 0 || !(e.file_type().is_some_and(|t| t.is_dir()) && is_junk_dir(&e.file_name().to_string_lossy())));
    let hits = Mutex::new(Vec::new());
    let deadline = Instant::now() + LIVE_BUDGET;
    builder.build_parallel().run(|| {
        let hits = &hits;
        Box::new(move |entry| {
            if Instant::now() > deadline {
                return ignore::WalkState::Quit;
            }
            let Ok(e) = entry else { return ignore::WalkState::Continue };
            if e.depth() == 0 {
                return ignore::WalkState::Continue;
            }
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            let name = e.file_name().to_string_lossy().to_lowercase();
            if let Some(score) = wanted(q, &name, is_dir).filter(|_| in_scope(scope, e.path())) {
                let meta = e.metadata().ok();
                hits.lock().unwrap_or_else(|p| p.into_inner()).push(Hit {
                    path: e.path().to_path_buf(),
                    is_dir,
                    size: meta.as_ref().filter(|_| !is_dir).map(|m| m.len()).unwrap_or(0),
                    modified: meta.and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH),
                    score,
                });
            }
            ignore::WalkState::Continue
        })
    });
    hits.into_inner().unwrap_or_else(|p| p.into_inner())
}

/// The best matches for a request, most relevant and most recent first.
pub fn find(query: &str, extension: Option<&str>, hint: Option<&str>, limit: usize) -> Vec<Hit> {
    let q = parse_query(query, extension);
    if q.words.is_empty() && q.exts.is_empty() {
        return Vec::new();
    }
    let scope = scope_from_hint(hint);
    let mut hits = match search_index(&q, &scope) {
        Some(h) => h,
        None => {
            let roots = match &scope {
                Scope::Root(r) => vec![r.clone()],
                _ => roots(),
            };
            search_live(&q, &roots, &scope)
        }
    };
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(b.modified.cmp(&a.modified)));
    hits.truncate(limit);
    hits
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.0} KB", bytes as f64 / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.2} GB", bytes as f64 / 1_073_741_824.0),
    }
}

fn age(modified: SystemTime) -> String {
    let secs = modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=3599 => "within the last hour".to_string(),
        3600..=86_399 => format!("{} hours ago", secs / 3600),
        86_400..=1_209_599 => format!("{} days ago", secs / 86_400),
        _ => chrono::DateTime::<chrono::Local>::from(modified).format("%-d %b %Y").to_string(),
    }
}

fn describe(hit: &Hit) -> String {
    if hit.is_dir {
        format!("[folder] {} (modified {})", hit.path.display(), age(hit.modified))
    } else {
        format!("{} ({}, modified {})", hit.path.display(), human_size(hit.size), age(hit.modified))
    }
}

/// Opens a file or folder with its default app, in VS Code, or selected in Explorer.
pub async fn open_with(path: &Path, with: Option<&str>) -> Result<String, String> {
    let shown = path.display().to_string();
    match with.map(normalize).as_deref() {
        Some("vscode" | "vs code" | "code" | "visual studio code" | "editor") => {
            crate::apps::open("vscode", std::slice::from_ref(&shown)).await?;
            Ok(format!("Opened {shown} in VS Code."))
        }
        Some("explorer" | "file explorer" | "folder" | "reveal" | "show in folder" | "finder" | "file manager") => {
            #[cfg(windows)]
            {
                let p = shown.clone();
                crate::win32::call(move || crate::win32::shell_execute("explorer.exe", Some(&format!("/select,\"{p}\"")), None)).await??;
            }
            #[cfg(not(windows))]
            open_path(path.parent().unwrap_or(path)).await?;
            Ok(format!("Showed {shown} in the file manager."))
        }
        _ => {
            open_path(path).await?;
            Ok(format!("Opened {shown}."))
        }
    }
}

async fn open_path(path: &Path) -> Result<(), String> {
    let p = path.display().to_string();
    #[cfg(windows)]
    {
        crate::win32::call(move || crate::win32::shell_execute(&p, None, None)).await?
    }
    #[cfg(not(windows))]
    {
        tokio::task::spawn_blocking(move || std::process::Command::new("xdg-open").arg(&p).spawn().map(|_| ()).map_err(|e| e.to_string()))
            .await
            .map_err(|e| e.to_string())?
    }
}

/// What a file tool did, for the focus tracker and the HUD preview.
pub struct FileOutcome {
    pub text: String,
    /// The file the conversation is now about.
    pub path: Option<PathBuf>,
}

/// The `find_files` tool.
pub async fn find_tool(args: &Value) -> Result<FileOutcome, String> {
    let query = str_arg(args, &["query", "name", "file", "filename"]).ok_or("find_files needs 'query'.")?.to_string();
    let extension = str_arg(args, &["file_extension", "extension", "ext"]).map(String::from);
    let hint = str_arg(args, &["directory_hint", "directory", "folder", "location", "in"]).map(String::from);
    let open = bool_arg(args, "open_immediately").or_else(|| bool_arg(args, "open")).unwrap_or(false);
    let started = Instant::now();
    let (q, e, h) = (query.clone(), extension.clone(), hint.clone());
    let hits = tokio::task::spawn_blocking(move || find(&q, e.as_deref(), h.as_deref(), MAX_RESULTS)).await.map_err(|e| e.to_string())?;
    let ms = started.elapsed().as_millis();
    if hits.is_empty() {
        let indexing = BUILDING.load(Ordering::Acquire) && INDEX.read().unwrap_or_else(|e| e.into_inner()).built.is_none();
        return Err(format!(
            "No file or folder matching '{query}'{}{}.",
            hint.map(|h| format!(" in {h}")).unwrap_or_default(),
            if indexing { " (the file index is still being built; the search covered what it could)" } else { "" }
        ));
    }
    let best = hits[0].path.clone();
    if open {
        let opened = open_with(&best, str_arg(args, &["open_with", "with", "app"])).await?;
        let others = if hits.len() > 1 { format!(" {} other match(es), e.g. {}.", hits.len() - 1, hits[1].path.display()) } else { String::new() };
        return Ok(FileOutcome { text: format!("{opened}{others}"), path: Some(best) });
    }
    Ok(FileOutcome {
        text: format!("{} match(es) in {ms} ms:\n{}", hits.len(), hits.iter().map(describe).collect::<Vec<_>>().join("\n")),
        path: Some(best),
    })
}

// ----------------------------------------------------------------------------- grep ----

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// The `search_inside_files` tool. `default_dir`: where to look when Sir names no folder (the
/// folder of the file the conversation is about).
pub async fn grep_tool(args: &Value, default_dir: Option<PathBuf>) -> Result<String, String> {
    let pattern = str_arg(args, &["pattern_or_regex", "pattern", "regex", "text", "query"]).ok_or("search_inside_files needs 'pattern_or_regex'.")?.to_string();
    let is_regex = bool_arg(args, "is_regex").or_else(|| bool_arg(args, "regex")).unwrap_or(false);
    let case_sensitive = bool_arg(args, "case_sensitive").unwrap_or(false);
    let context = args.get("context_lines").and_then(Value::as_u64).unwrap_or(1).min(3) as usize;
    let extensions: Vec<String> = str_arg(args, &["file_extension", "extension", "ext"])
        .map(|e| e.split([',', ' ', ';']).map(|x| x.trim().trim_start_matches("*.").trim_start_matches('.').to_lowercase()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    let root = match str_arg(args, &["directory_or_file", "directory", "path", "file", "folder", "in"]) {
        Some(raw) => resolve_existing(raw, default_dir.as_deref()).await?,
        None => default_dir.ok_or("search_inside_files needs 'directory_or_file' (a folder or file to search).")?,
    };
    let source = if is_regex { pattern.clone() } else { regex::escape(&pattern) };
    let re = regex::RegexBuilder::new(&source).case_insensitive(!case_sensitive).size_limit(1 << 22).build().map_err(|e| format!("Invalid pattern: {e}"))?;
    let shown_root = root.display().to_string();
    let started = Instant::now();
    let (files, total, timed_out) = tokio::task::spawn_blocking(move || grep(&root, &re, &extensions, context)).await.map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err(format!("Nothing in {shown_root} contains '{pattern}'{}.", if timed_out { " (the search ran out of time; narrow the folder)" } else { "" }));
    }
    let mut out = format!(
        "{total} match(es) in {} file(s) for '{pattern}' under {shown_root} ({} ms{}):\n",
        files.len(),
        started.elapsed().as_millis(),
        if timed_out || total >= MAX_GREP_MATCHES { ", list capped" } else { "" }
    );
    for (path, lines) in files {
        out.push_str(&format!("{}\n", path.display()));
        for (n, text, hit) in lines {
            out.push_str(&format!("{}{n:>5}: {}\n", if hit { ">" } else { " " }, text));
        }
    }
    Ok(out)
}

type GrepFile = (PathBuf, Vec<(usize, String, bool)>);

fn grep(root: &Path, re: &regex::Regex, extensions: &[String], context: usize) -> (Vec<GrepFile>, usize, bool) {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .parents(true)
        .max_depth(Some(MAX_DEPTH + 4))
        .threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8))
        .filter_entry(|e| e.depth() == 0 || !(e.file_type().is_some_and(|t| t.is_dir()) && is_junk_dir(&e.file_name().to_string_lossy())));
    let results: Mutex<(Vec<GrepFile>, usize)> = Mutex::new((Vec::new(), 0));
    let deadline = Instant::now() + GREP_BUDGET;
    let timed_out = AtomicBool::new(false);
    builder.build_parallel().run(|| {
        let (results, timed_out) = (&results, &timed_out);
        Box::new(move |entry| {
            if Instant::now() > deadline {
                timed_out.store(true, Ordering::Release);
                return ignore::WalkState::Quit;
            }
            let Ok(e) = entry else { return ignore::WalkState::Continue };
            if !e.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = e.path();
            if !extensions.is_empty() {
                let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
                if !extensions.contains(&ext) {
                    return ignore::WalkState::Continue;
                }
            }
            if e.metadata().map(|m| m.len() > MAX_TEXT_BYTES).unwrap_or(true) {
                return ignore::WalkState::Continue;
            }
            let Ok(bytes) = std::fs::read(path) else { return ignore::WalkState::Continue };
            if looks_binary(&bytes) {
                return ignore::WalkState::Continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let hit_lines: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| re.is_match(l)).map(|(i, _)| i).collect();
            if hit_lines.is_empty() {
                return ignore::WalkState::Continue;
            }
            let mut shown: Vec<(usize, String, bool)> = Vec::new();
            for &i in hit_lines.iter().take(8) {
                let from = i.saturating_sub(context);
                for (j, text) in lines.iter().enumerate().skip(from).take(i + context + 1 - from) {
                    if shown.last().is_none_or(|(n, _, _)| *n < j + 1) {
                        let line: String = text.trim_end().chars().take(200).collect();
                        shown.push((j + 1, line, j == i));
                    } else if j == i {
                        if let Some(last) = shown.last_mut() {
                            last.2 = true;
                        }
                    }
                }
            }
            let mut r = results.lock().unwrap_or_else(|p| p.into_inner());
            r.1 += hit_lines.len();
            r.0.push((path.to_path_buf(), shown));
            if r.0.len() >= MAX_GREP_FILES || r.1 >= MAX_GREP_MATCHES {
                return ignore::WalkState::Quit;
            }
            ignore::WalkState::Continue
        })
    });
    let (mut files, total) = results.into_inner().unwrap_or_else(|p| p.into_inner());
    files.sort_by_key(|f| f.0.clone());
    (files, total, timed_out.load(Ordering::Acquire))
}

// ------------------------------------------------------------------- path resolution ----

/// An existing file or folder: absolute paths, `~`/`%VARS%`, paths relative to the focus folder
/// or the home folder ("Desktop/notes.txt"), or just a name ("my config.toml") looked up in the
/// file index (most recently modified match first).
pub async fn resolve_existing(raw: &str, focus_dir: Option<&Path>) -> Result<PathBuf, String> {
    let cleaned = raw.trim().trim_matches('"').trim_start_matches("my ").trim_start_matches("the ").trim();
    let expanded = PathBuf::from(expand_env(cleaned));
    if expanded.is_absolute() {
        return if expanded.exists() { Ok(expanded) } else { Err(format!("{} does not exist.", expanded.display())) };
    }
    for base in focus_dir.map(Path::to_path_buf).into_iter().chain(home_dir()) {
        let candidate = base.join(&expanded);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let name = expanded.file_name().map(|n| n.to_string_lossy().to_lowercase()).ok_or_else(|| format!("'{raw}' is not a file name."))?;
    let suffix = same_path_key(&expanded);
    let looked_up = tokio::task::spawn_blocking(move || {
        let matches = |p: &Path| {
            let key = same_path_key(p);
            key.ends_with(&suffix) && (key.len() == suffix.len() || key[..key.len() - suffix.len()].ends_with('\\'))
        };
        let from_index = {
            let index = INDEX.read().unwrap_or_else(|e| e.into_inner());
            index.built.map(|_| {
                // The most recently modified file of that name.
                index.entries.iter().filter(|e| *e.name == name && matches(&e.path)).max_by_key(|e| e.modified).map(|e| e.path.clone())
            })
        };
        match from_index {
            Some(found) => found,
            // Index not built yet: a quick live search by exact name.
            None => find(&name, None, None, 5).into_iter().map(|h| h.path).find(|p| matches(p)),
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    looked_up.ok_or_else(|| format!("I could not find a file called '{raw}' (searched the file index of Sir's folders and drives)."))
}

/// Where a new file goes: absolute paths as given; "Desktop/x.txt" under the home folder; a bare
/// name next to the file the conversation is about, otherwise in Documents.
pub fn resolve_new(raw: &str, focus_dir: Option<&Path>) -> Result<PathBuf, String> {
    let expanded = PathBuf::from(expand_env(raw.trim().trim_matches('"')));
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let home = home_dir().ok_or("I cannot find Sir's home folder.")?;
    let first = expanded.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).unwrap_or_default();
    let known = ["desktop", "documents", "downloads", "pictures", "music", "videos", "onedrive", "projects", "code", "coding"];
    if known.contains(&first.as_str()) || home.join(&expanded).parent().is_some_and(|p| p.is_dir() && p != home) {
        return Ok(home.join(expanded));
    }
    Ok(focus_dir.map(Path::to_path_buf).unwrap_or_else(|| home.join("Documents")).join(expanded))
}

/// Writing here could break Windows, Linux or JAI himself.
fn check_writable(path: &Path) -> Result<(), String> {
    let key = same_path_key(path);
    let protected: &[&str] = if cfg!(windows) {
        &["c:\\windows", "c:\\program files", "c:\\programdata", "c:\\$recycle.bin", "c:\\system volume information"]
    } else {
        &["/etc", "/usr", "/bin", "/sbin", "/boot", "/sys", "/proc", "/lib", "/var/lib", "/dev"]
    };
    if protected.iter().any(|p| key.starts_with(p)) {
        return Err(format!("{} is a system location; I won't write there.", path.display()));
    }
    if key.split('\\').any(|c| c == BACKUP_DIR) {
        return Err("That is inside my backup folder; restore with undo_last_file_edit instead.".into());
    }
    if let Ok(root) = crate::self_improve::project_root() {
        let src = same_path_key(&root.join("src"));
        if key.starts_with(&src) || key == same_path_key(&root.join("Cargo.toml")) {
            return Err("That is my own source code: use patch_own_source, which compile-checks the change and rolls it back if it breaks.".into());
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------- text I/O ----

struct TextFile {
    /// Content with `\n` line endings.
    text: String,
    crlf: bool,
    bom: bool,
}

fn load_text(path: &Path, lossy: bool) -> Result<TextFile, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder, not a file.", path.display()));
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(format!("{} is {} — too large to handle as text.", path.display(), human_size(meta.len())));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if looks_binary(&bytes) {
        return Err(format!("{} is a binary file, not text.", path.display()));
    }
    let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let body = if bom { &bytes[3..] } else { &bytes[..] };
    let raw = match std::str::from_utf8(body) {
        Ok(s) => s.to_string(),
        Err(_) if lossy => String::from_utf8_lossy(body).into_owned(),
        Err(_) => return Err(format!("{} is not UTF-8 text, so I won't edit it (that could corrupt it).", path.display())),
    };
    Ok(TextFile { crlf: raw.contains("\r\n"), text: raw.replace("\r\n", "\n"), bom })
}

fn save_text(path: &Path, text: &str, crlf: bool, bom: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let body = if crlf { text.replace('\n', "\r\n") } else { text.to_string() };
    let mut bytes = Vec::with_capacity(body.len() + 3);
    if bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(body.as_bytes());
    std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

// -------------------------------------------------------------------------- backups ----

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupEntry {
    original: String,
    /// File name inside `.jai_backups/`; `None` when the file did not exist (it was created).
    backup: Option<String>,
    op: String,
    at: String,
}

static BACKUP_LOCK: Mutex<()> = Mutex::new(());

fn backup_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("jarvis_test_backups_{}", std::process::id()));
    }
    std::env::current_dir().unwrap_or_default().join(BACKUP_DIR)
}

fn backup_name(path: &Path, tag: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    same_path_key(path).hash(&mut h);
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    format!("{}_{:08x}_{tag}_{name}.bak", chrono::Local::now().format("%Y%m%d_%H%M%S_%3f"), h.finish() as u32)
}

/// Copies the current version (if any) into `.jai_backups/` and records it for undo.
fn record_backup(path: &Path, op: &str) -> Result<Option<PathBuf>, String> {
    let _guard = BACKUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = backup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let backup = if path.is_file() {
        let name = backup_name(path, op);
        std::fs::copy(path, dir.join(&name)).map_err(|e| format!("backing up {}: {e}", path.display()))?;
        Some(name)
    } else {
        None
    };
    let entry = BackupEntry { original: path.display().to_string(), backup: backup.clone(), op: op.into(), at: chrono::Local::now().to_rfc3339() };
    let mut index = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("index.jsonl")).map_err(|e| e.to_string())?;
    writeln!(index, "{}", serde_json::to_string(&entry).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(backup.map(|b| dir.join(b)))
}

/// Removes and returns the newest undo entry for `path`.
fn take_last_backup(path: &Path) -> Result<Option<BackupEntry>, String> {
    let _guard = BACKUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let file = backup_dir().join("index.jsonl");
    let Ok(raw) = std::fs::read_to_string(&file) else { return Ok(None) };
    let key = same_path_key(path);
    let mut entries: Vec<BackupEntry> = raw.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let Some(pos) = entries.iter().rposition(|e| same_path_key(Path::new(&e.original)) == key) else { return Ok(None) };
    let entry = entries.remove(pos);
    let rest: String = entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
    std::fs::write(&file, rest).map_err(|e| e.to_string())?;
    Ok(Some(entry))
}

// ----------------------------------------------------------------------------- diffs ----

/// One hunk covering everything between the common first and last lines, with two lines of
/// context, capped at `DIFF_LINES` lines.
fn region_diff(old: &str, new: &str) -> (String, usize, usize) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (removed, added) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    if removed.is_empty() && added.is_empty() {
        return ("(no changes)".into(), 0, 0);
    }
    let ctx = 2;
    let start = prefix.saturating_sub(ctx);
    let mut lines = vec![format!("@@ -{},{} +{},{} @@", start + 1, removed.len() + (prefix - start) + suffix.min(ctx), start + 1, added.len() + (prefix - start) + suffix.min(ctx))];
    lines.extend(a[start..prefix].iter().map(|l| format!(" {l}")));
    lines.extend(removed.iter().map(|l| format!("-{l}")));
    lines.extend(added.iter().map(|l| format!("+{l}")));
    lines.extend(a[a.len() - suffix..].iter().take(ctx).map(|l| format!(" {l}")));
    if lines.len() > DIFF_LINES {
        let more = lines.len() - DIFF_LINES;
        lines.truncate(DIFF_LINES);
        lines.push(format!("… ({more} more diff lines)"));
    }
    (lines.join("\n"), removed.len(), added.len())
}

/// The result of a write, for the chat, the focus tracker and the HUD's diff card.
pub struct EditOutcome {
    pub path: PathBuf,
    pub summary: String,
    pub diff: String,
}

impl EditOutcome {
    fn new(path: PathBuf, what: &str, old: &str, new: &str, note: &str) -> Self {
        let (diff, removed, added) = region_diff(old, new);
        let summary = format!("{what} {} (-{removed} +{added} lines{note}). Backup in {BACKUP_DIR}; undo_last_file_edit restores it.\n{diff}", path.display());
        Self { path, summary, diff }
    }
}

// --------------------------------------------------------------------------- tools ----

/// `read_text_file`: numbered lines of a file or a line range.
pub async fn read_tool(args: &Value, focus_dir: Option<&Path>) -> Result<FileOutcome, String> {
    let raw = str_arg(args, &["file_path", "path", "file", "name"]).ok_or("read_text_file needs 'file_path'.")?;
    let path = resolve_existing(raw, focus_dir).await?;
    let file = load_text(&path, true)?;
    let lines: Vec<&str> = file.text.lines().collect();
    let total = lines.len();
    let start = args.get("start_line").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let end = args.get("end_line").and_then(Value::as_u64).map(|e| e as usize).unwrap_or(start + READ_LIMIT_LINES - 1).min(total).min(start + READ_LIMIT_LINES - 1);
    if start > total.max(1) {
        return Err(format!("{} has only {total} lines.", path.display()));
    }
    let mut out = format!("{} (lines {start}-{end} of {total}):\n", path.display());
    for (i, line) in lines.iter().enumerate().take(end).skip(start - 1) {
        out.push_str(&format!("{:>5} | {line}\n", i + 1));
        if out.len() > READ_LIMIT_CHARS {
            out.push_str("… (truncated; read on with start_line)\n");
            break;
        }
    }
    Ok(FileOutcome { text: out, path: Some(path) })
}

/// `create_or_overwrite_file`.
pub async fn write_tool(args: &Value, focus_dir: Option<&Path>) -> Result<EditOutcome, String> {
    let raw = str_arg(args, &["file_path", "path", "file", "name"]).ok_or("create_or_overwrite_file needs 'file_path'.")?;
    let content = args.get("content").or_else(|| args.get("text")).and_then(Value::as_str).ok_or("create_or_overwrite_file needs 'content'.")?;
    // Never a same-named file found elsewhere: a bare name means the folder just discussed.
    let expanded = PathBuf::from(expand_env(raw.trim().trim_matches('"')));
    let path = match focus_dir.map(|d| d.join(&expanded)).filter(|p| !expanded.is_absolute() && p.is_file()) {
        Some(p) => p,
        None => resolve_new(raw, focus_dir)?,
    };
    check_writable(&path)?;
    let old = if path.is_file() { Some(load_text(&path, false)?) } else { None };
    record_backup(&path, if old.is_some() { "overwrite" } else { "create" })?;
    let content = content.replace("\r\n", "\n");
    let (crlf, bom) = old.as_ref().map(|o| (o.crlf, o.bom)).unwrap_or((cfg!(windows) && is_windows_script(&path), false));
    save_text(&path, &content, crlf, bom)?;
    info!("[FILES] {} {}", if old.is_some() { "Overwrote" } else { "Created" }, path.display());
    let old_text = old.map(|o| o.text).unwrap_or_default();
    Ok(EditOutcome::new(path, if old_text.is_empty() { "Created" } else { "Overwrote" }, &old_text, &content, ""))
}

fn is_windows_script(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref(), Some("bat" | "cmd" | "ps1"))
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum MatchKind {
    Exact,
    /// Same lines apart from trailing whitespace.
    TrailingWhitespace,
    /// Same lines apart from indentation.
    Indentation,
}

/// Byte ranges of `target` in `text` (both with `\n` endings), trying exact, then
/// trailing-whitespace-insensitive, then indentation-insensitive line matching.
fn locate(text: &str, target: &str) -> (Vec<(usize, usize)>, MatchKind) {
    let exact: Vec<(usize, usize)> = text.match_indices(target).map(|(i, m)| (i, i + m.len())).collect();
    if !exact.is_empty() {
        return (exact, MatchKind::Exact);
    }
    let want: Vec<&str> = {
        let all: Vec<&str> = target.lines().collect();
        let first = all.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
        let last = all.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(all.len());
        all[first..last].to_vec()
    };
    if want.is_empty() {
        return (Vec::new(), MatchKind::Exact);
    }
    // (start byte, end byte without the newline) of every line.
    let mut spans = Vec::new();
    let mut pos = 0;
    for line in text.split('\n') {
        spans.push((pos, pos + line.len()));
        pos += line.len() + 1;
    }
    for (kind, norm) in [(MatchKind::TrailingWhitespace, str::trim_end as fn(&str) -> &str), (MatchKind::Indentation, str::trim as fn(&str) -> &str)] {
        let found: Vec<(usize, usize)> = (0..spans.len().saturating_sub(want.len() - 1))
            .filter(|&i| want.iter().enumerate().all(|(k, w)| norm(&text[spans[i + k].0..spans[i + k].1]) == norm(w)))
            .map(|i| (spans[i].0, spans[i + want.len() - 1].1))
            .collect();
        if !found.is_empty() {
            return (found, kind);
        }
    }
    (Vec::new(), MatchKind::Exact)
}

fn indent_of(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

/// Re-indents `replacement` from the snippet's indentation to the file's.
fn reindent(replacement: &str, snippet_indent: &str, file_indent: &str) -> String {
    replacement
        .split('\n')
        .map(|l| match l.strip_prefix(snippet_indent) {
            Some(rest) if !l.trim().is_empty() => format!("{file_indent}{rest}"),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].matches('\n').count() + 1
}

/// `edit_file_surgical`: snippet search-and-replace or a line-range patch.
pub async fn edit_tool(args: &Value, focus_dir: Option<&Path>) -> Result<EditOutcome, String> {
    let raw = str_arg(args, &["file_path", "path", "file", "name"]).ok_or("edit_file_surgical needs 'file_path'.")?;
    let path = resolve_existing(raw, focus_dir).await?;
    check_writable(&path)?;
    let file = load_text(&path, false)?;
    let replacement = args
        .get("replacement_snippet")
        .or_else(|| args.get("replacement"))
        .or_else(|| args.get("new_text"))
        .or_else(|| args.get("replace"))
        .and_then(Value::as_str)
        .map(|s| s.replace("\r\n", "\n"))
        .ok_or("edit_file_surgical needs 'replacement_snippet' (empty to delete).")?;
    let target = args.get("target_snippet").or_else(|| args.get("target")).or_else(|| args.get("find")).and_then(Value::as_str).map(|s| s.replace("\r\n", "\n"));
    let replace_all = bool_arg(args, "replace_all").unwrap_or(false);
    let (new_text, note) = match target.filter(|t| !t.trim().is_empty()) {
        Some(target) => {
            let (ranges, kind) = locate(&file.text, &target);
            match ranges.len() {
                0 => {
                    return Err(format!(
                        "The target snippet is not in {} (also tried ignoring line endings, trailing spaces and indentation). Read the lines with read_text_file and copy them exactly.",
                        path.display()
                    ))
                }
                n if n > 1 && !replace_all => {
                    let at: Vec<String> = ranges.iter().take(6).map(|(s, _)| line_of(&file.text, *s).to_string()).collect();
                    return Err(format!("The snippet occurs {n} times in {} (lines {}); include more surrounding lines, or set replace_all.", path.display(), at.join(", ")));
                }
                _ => {}
            }
            let mut out = String::with_capacity(file.text.len() + replacement.len());
            let mut last = 0;
            for &(s, e) in &ranges {
                out.push_str(&file.text[last..s]);
                let fitted = if kind == MatchKind::Indentation {
                    let first_target = target.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                    reindent(&replacement, indent_of(first_target), indent_of(&file.text[s..e]))
                } else {
                    replacement.clone()
                };
                out.push_str(&fitted);
                last = e;
            }
            out.push_str(&file.text[last..]);
            let note = match (kind, ranges.len()) {
                (MatchKind::Exact, 1) => String::new(),
                (MatchKind::Exact, n) => format!(", {n} occurrences"),
                (MatchKind::TrailingWhitespace, _) => ", matched ignoring trailing whitespace".into(),
                (MatchKind::Indentation, _) => ", matched ignoring indentation (re-indented)".into(),
            };
            (out, note)
        }
        None => {
            let (start, end) = match (args.get("start_line").and_then(Value::as_u64), args.get("end_line").and_then(Value::as_u64)) {
                (Some(s), Some(e)) => (s as usize, e as usize),
                (Some(s), None) => (s as usize, s as usize),
                _ => return Err("edit_file_surgical needs 'target_snippet', or 'start_line'/'end_line' for a line-range patch.".into()),
            };
            let lines: Vec<&str> = file.text.split('\n').collect();
            let count = if file.text.ends_with('\n') { lines.len() - 1 } else { lines.len() };
            if start == 0 || start > end || end > count {
                return Err(format!("Lines {start}-{end} are outside {} (it has {count} lines).", path.display()));
            }
            let mut out: Vec<String> = lines[..start - 1].iter().map(|s| s.to_string()).collect();
            if !replacement.is_empty() {
                out.extend(replacement.trim_end_matches('\n').split('\n').map(String::from));
            }
            out.extend(lines[end..].iter().map(|s| s.to_string()));
            (out.join("\n"), format!(", lines {start}-{end}"))
        }
    };
    if new_text == file.text {
        return Err("That change would not alter the file.".into());
    }
    record_backup(&path, "edit")?;
    save_text(&path, &new_text, file.crlf, file.bom)?;
    info!("[FILES] Edited {}{note}", path.display());
    Ok(EditOutcome::new(path, "Edited", &file.text, &new_text, &note))
}

/// `undo_last_file_edit`: puts the newest backup of a file back (a file JAI created is moved
/// to the backups). The version being replaced is kept in `.jai_backups/` too.
pub async fn undo_tool(args: &Value, focus_file: Option<PathBuf>) -> Result<EditOutcome, String> {
    let path = match str_arg(args, &["file_path", "path", "file", "name"]) {
        Some(raw) => match resolve_existing(raw, focus_file.as_deref().and_then(Path::parent)).await {
            Ok(p) => p,
            // A created file that is gone again still has its undo entry.
            Err(_) => PathBuf::from(expand_env(raw)),
        },
        None => focus_file.ok_or("Which file? I haven't edited one in this conversation.")?,
    };
    let entry = take_last_backup(&path)?.ok_or_else(|| format!("There is no backup of {} to restore.", path.display()))?;
    let current = if path.is_file() { load_text(&path, true).map(|f| f.text).unwrap_or_default() } else { String::new() };
    if path.is_file() {
        let dir = backup_dir();
        std::fs::copy(&path, dir.join(backup_name(&path, "before_undo"))).map_err(|e| format!("saving the current version: {e}"))?;
    }
    match &entry.backup {
        Some(name) => {
            let source = backup_dir().join(name);
            if !source.is_file() {
                return Err(format!("The backup {name} is missing from {BACKUP_DIR}."));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&source, &path).map_err(|e| format!("restoring {}: {e}", path.display()))?;
        }
        None if path.is_file() => std::fs::remove_file(&path).map_err(|e| format!("removing {}: {e}", path.display()))?,
        None => {}
    }
    let restored = if path.is_file() { load_text(&path, true).map(|f| f.text).unwrap_or_default() } else { String::new() };
    info!("[FILES] Undid the {} of {}", entry.op, path.display());
    let what = match (entry.backup.is_some(), entry.op.as_str()) {
        (false, _) => "Removed the newly created",
        (true, "overwrite") => "Restored the overwritten",
        _ => "Undid the last edit of",
    };
    Ok(EditOutcome::new(path, what, &current, &restored, &format!(", from {}", entry.at.get(..19).unwrap_or(&entry.at))))
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "find_files",
            "description": "Find files or folders on Sir's PC in milliseconds (indexed: his user folders, OneDrive, projects, Minecraft instances, other drives). Fuzzy name words plus kind ('resume pdf', 'holiday photos', 'config.toml'). Returns the 15 best; open_immediately opens the best (default app, VS Code or Explorer via open_with).",
            "parameters": {"type": "object", "properties": {
                "query": {"type": "string"},
                "file_extension": {"type": "string", "description": "e.g. pdf, rs, toml (comma-separated)"},
                "directory_hint": {"type": "string", "description": "Downloads, D:, a folder path or a folder name to search in"},
                "open_immediately": {"type": "boolean"},
                "open_with": {"type": "string", "enum": ["default", "vscode", "explorer"]}
            }, "required": ["query"]}
        }}),
        json!({"type": "function", "function": {
            "name": "search_inside_files",
            "description": "Search the text inside files (parallel, skips binaries and .gitignored/junk folders): returns paths, line numbers and context lines.",
            "parameters": {"type": "object", "properties": {
                "pattern_or_regex": {"type": "string"},
                "directory_or_file": {"type": "string", "description": "Folder or file to search (defaults to the folder of the file just discussed)"},
                "file_extension": {"type": "string", "description": "e.g. rs or py,js"},
                "is_regex": {"type": "boolean"},
                "case_sensitive": {"type": "boolean"},
                "context_lines": {"type": "integer"}
            }, "required": ["pattern_or_regex"]}
        }}),
        json!({"type": "function", "function": {
            "name": "read_text_file",
            "description": "Read a text/code/config file (or lines start_line-end_line) with line numbers. A bare name like 'config.toml' is found automatically.",
            "parameters": {"type": "object", "properties": {
                "file_path": {"type": "string"}, "start_line": {"type": "integer"}, "end_line": {"type": "integer"}
            }, "required": ["file_path"]}
        }}),
        json!({"type": "function", "function": {
            "name": "create_or_overwrite_file",
            "description": "Create a text/code file (parent folders are created) or overwrite one completely; the old version is backed up first. For small changes use edit_file_surgical.",
            "parameters": {"type": "object", "properties": {
                "file_path": {"type": "string", "description": "Absolute path, 'Desktop/notes.txt', or a name next to the file just discussed"},
                "content": {"type": "string"},
                "preview": {"type": "string", "enum": ["panel", "window", "none"], "description": "Show the change as a diff card (default panel)"}
            }, "required": ["file_path", "content"]}
        }}),
        json!({"type": "function", "function": {
            "name": "edit_file_surgical",
            "description": "Change part of a text/code file without rewriting it: replace target_snippet (copied from read_text_file; line endings, trailing spaces and indentation are tolerated) with replacement_snippet, or replace lines start_line-end_line. Backed up first; shows a diff.",
            "parameters": {"type": "object", "properties": {
                "file_path": {"type": "string"},
                "target_snippet": {"type": "string"},
                "replacement_snippet": {"type": "string", "description": "New text (empty string deletes the snippet)"},
                "replace_all": {"type": "boolean"},
                "start_line": {"type": "integer"}, "end_line": {"type": "integer"},
                "preview": {"type": "string", "enum": ["panel", "window", "none"]}
            }, "required": ["file_path", "replacement_snippet"]}
        }}),
        json!({"type": "function", "function": {
            "name": "undo_last_file_edit",
            "description": "Undo JAI's last change to a file (restores the backup; a newly created file is removed). Without file_path: the file just edited.",
            "parameters": {"type": "object", "properties": {"file_path": {"type": "string"}}, "required": []}
        }}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis_file_ops_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn queries_split_into_words_kinds_and_folders() {
        let q = parse_query("my tax return pdf", None);
        assert_eq!((q.words.clone(), q.exts.clone(), q.dirs_only), (vec!["tax".to_string(), "return".to_string()], vec!["pdf".to_string()], false));
        let q = parse_query("Kinetic Horizon modpack folder", None);
        assert_eq!((q.words.clone(), q.dirs_only), (vec!["kinetic".to_string(), "horizon".to_string()], true));
        let q = parse_query("resume", Some(".pdf, docx"));
        assert_eq!(q.exts, vec!["pdf".to_string(), "docx".to_string()]);
        assert!(score("tax_return_2025.pdf", &["tax".into(), "return".into()]) > 0.99);
        assert!(score("kinetic horizon", &["kinetic".into(), "horizon".into()]) > 1.0, "exact names rank first");
        assert!(score("kinetic horizon 1.20.1", &["kinetc".into(), "horizon".into()]) > 0.9, "typos still match");
        assert!(score("invoice.pdf", &["tax".into()]) < 0.82);
    }

    #[test]
    fn hints_become_scopes() {
        if cfg!(windows) {
            assert_eq!(scope_from_hint(Some("D drive")), Scope::Root(PathBuf::from("D:\\")));
            assert_eq!(scope_from_hint(Some("d:")), Scope::Root(PathBuf::from("D:\\")));
        }
        assert_eq!(scope_from_hint(None), Scope::Everywhere);
        assert_eq!(scope_from_hint(Some("my modpacks folder")), Scope::Containing("modpacks".into()));
        assert!(is_under(Path::new("D:\\Code\\x"), Path::new("d:\\code")) == cfg!(windows));
        assert!(!is_under(Path::new("D:\\Code2\\x"), Path::new("D:\\Code")));
        let home = home_dir().unwrap();
        if home.join("Downloads").is_dir() {
            assert_eq!(scope_from_hint(Some("Downloads")), Scope::Root(home.join("Downloads")));
        }
    }

    #[test]
    fn live_search_and_grep_find_files_and_lines() {
        let dir = temp_dir("search");
        std::fs::create_dir_all(dir.join("Kinetic Horizon").join("mods")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules").join("budget.txt"), "junk").unwrap();
        std::fs::write(dir.join("budget_2026.xlsx"), "x").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 42]).unwrap();
        let scope = Scope::Root(dir.clone());
        let hits = search_live(&parse_query("budget", None), std::slice::from_ref(&dir), &scope);
        assert_eq!(hits.len(), 1, "node_modules is skipped: {hits:?}");
        let folders = search_live(&parse_query("kinetic horizon modpack", None), std::slice::from_ref(&dir), &scope);
        assert!(folders.iter().all(|h| h.is_dir) && folders.len() == 1, "{folders:?}");
        let re = regex::RegexBuilder::new("answer").case_insensitive(true).build().unwrap();
        let (files, total, _) = grep(&dir, &re, &[], 1);
        assert_eq!((files.len(), total), (1, 2), "binary files are skipped");
        assert!(files[0].1.iter().any(|(n, _, hit)| *n == 2 && *hit));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn snippets_match_across_line_endings_whitespace_and_indentation() {
        let text = "fn a() {\n    let x = 1;   \n    let y = 2;\n}\n";
        assert_eq!(locate(text, "let y = 2;").1, MatchKind::Exact);
        let (r, kind) = locate(text, "let x = 1;\nlet y = 2;");
        assert_eq!((r.len(), kind), (1, MatchKind::Indentation));
        let (r, kind) = locate(text, "    let x = 1;\n    let y = 2;");
        assert_eq!((r.len(), kind), (1, MatchKind::TrailingWhitespace));
        assert_eq!(&text[r[0].0..r[0].1], "    let x = 1;   \n    let y = 2;");
        assert!(locate(text, "let z").0.is_empty());
        assert_eq!(reindent("let x = 10;\nlet y = 20;", "", "    "), "    let x = 10;\n    let y = 20;");
        let (diff, removed, added) = region_diff("a\nb\nc\nd\n", "a\nb\nC\nd\n");
        assert_eq!((removed, added), (1, 1));
        assert!(diff.contains("-c") && diff.contains("+C") && diff.contains(" b"), "{diff}");
    }

    #[test]
    fn edit_create_and_undo_round_trip_with_backups() {
        let dir = temp_dir("edit");
        let file = dir.join("settings.toml");
        std::fs::write(&file, "[server]\r\nport = 8080\r\nhost = \"localhost\"\r\n").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let path = file.display().to_string();
        let out = rt
            .block_on(edit_tool(&json!({"file_path": path, "target_snippet": "port = 8080\n", "replacement_snippet": "port = 9090\n"}), None))
            .unwrap();
        assert!(out.diff.contains("+port = 9090"), "{}", out.diff);
        let edited = std::fs::read_to_string(&file).unwrap();
        assert_eq!(edited, "[server]\r\nport = 9090\r\nhost = \"localhost\"\r\n", "CRLF endings are kept");
        rt.block_on(undo_tool(&json!({"file_path": path}), None)).unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("port = 8080"));
        assert!(rt.block_on(undo_tool(&json!({"file_path": path}), None)).is_err(), "nothing left to undo");

        let created = dir.join("sub").join("notes.md");
        rt.block_on(write_tool(&json!({"file_path": created.display().to_string(), "content": "# Notes\n"}), None)).unwrap();
        assert!(created.is_file());
        rt.block_on(undo_tool(&json!({"file_path": created.display().to_string()}), None)).unwrap();
        assert!(!created.exists(), "undoing a creation removes the file");
        let lines = rt.block_on(read_tool(&json!({"file_path": path, "start_line": 2, "end_line": 2}), None)).unwrap();
        assert!(lines.text.contains("    2 | port = 8080"), "{}", lines.text);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn system_and_own_source_paths_are_protected() {
        if cfg!(windows) {
            assert!(check_writable(Path::new("C:\\Windows\\System32\\drivers\\etc\\hosts")).is_err());
        } else {
            assert!(check_writable(Path::new("/etc/hosts")).is_err());
        }
        if let Ok(root) = crate::self_improve::project_root() {
            assert!(check_writable(&root.join("src").join("main.rs")).unwrap_err().contains("patch_own_source"));
        }
        assert!(check_writable(&std::env::temp_dir().join("x.txt")).is_ok());
    }
}
