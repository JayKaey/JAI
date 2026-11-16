//! Installed-application index, built in the background at start-up so "open <anything
//! installed>" resolves instantly and fuzzily (`strsim`), not just for the built-in aliases.
//!
//! Windows: Start-Menu shortcuts (`%ProgramData%` and `%AppData%`), registry App Paths (HKLM and
//! HKCU) and the program folders (`%LocalAppData%\Programs`, `Program Files`, `Program Files
//! (x86)`). Linux: `.desktop` entries (system, Flatpak, Snap, user) and executables on `$PATH`.

use crate::apps::normalize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};
use strsim::{jaro_winkler, normalized_levenshtein};
use tracing::{debug, info};

/// Rebuilt on a miss when older than this (new installs show up without a restart).
const MAX_AGE: Duration = Duration::from_secs(300);
const MIN_SCORE: f64 = 0.88;
const NEAREST_SCORE: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// `.lnk` / `.url` shortcut.
    Shortcut,
    /// Registry App Paths executable.
    AppPath,
    /// Executable found in a program folder.
    Program,
    /// Linux `.desktop` entry.
    #[cfg_attr(windows, allow(dead_code))]
    Desktop,
    /// Executable on `$PATH` (Linux).
    PathBin,
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    /// Display name ("Visual Studio Code", "obs64").
    pub name: String,
    /// What gets launched: shortcut, executable or `.desktop` file.
    pub target: PathBuf,
    pub kind: EntryKind,
    /// `normalize`d name, plus the folder name for program-folder executables.
    keys: Vec<String>,
}

#[derive(Default)]
struct Index {
    entries: Vec<AppEntry>,
    built: Option<Instant>,
}

static INDEX: LazyLock<RwLock<Index>> = LazyLock::new(|| RwLock::new(Index::default()));

/// Builds the index on a background thread.
pub fn start() {
    let spawned = std::thread::Builder::new().name("jai-appindex".into()).spawn(|| {
        crate::platform::lower_thread_priority();
        rebuild();
    });
    if let Err(e) = spawned {
        tracing::warn!("[APPS] Could not start the application indexer: {e}");
    }
}

/// Scans every source again (blocking, typically 50–300 ms).
pub fn rebuild() {
    let started = Instant::now();
    let mut entries = scan();
    // Same target reachable twice (shortcut + App Path): keep the first, i.e. the shortcut.
    let mut seen = HashSet::new();
    entries.retain(|e| seen.insert((e.keys[0].clone(), e.kind == EntryKind::PathBin)));
    let count = entries.len();
    *INDEX.write().unwrap_or_else(|e| e.into_inner()) = Index { entries, built: Some(Instant::now()) };
    info!("[APPS] Indexed {count} installed applications in {} ms", started.elapsed().as_millis());
    // His voice learns how to say their titles, and his games'.
    crate::pronunciation_lexicon::learn_titles(names().into_iter().chain(crate::kit_games::library().into_iter().map(|g| g.name)));
}

fn score(query: &str, key: &str) -> f64 {
    if key == query {
        return 1.0;
    }
    let compact = |s: &str| s.replace([' ', '-', '.'], "");
    if compact(key) == compact(query) {
        return 0.99;
    }
    // "obs" → "obs studio", "code" → "visual studio code" (whole words only).
    if key.split_whitespace().any(|w| w == query) || key.starts_with(&format!("{query} ")) {
        return 0.93;
    }
    jaro_winkler(query, key).max(normalized_levenshtein(query, key))
}

fn best(entries: &[AppEntry], query: &str, threshold: f64) -> Option<(f64, AppEntry)> {
    let mut best: Option<(f64, &AppEntry)> = None;
    for e in entries {
        let s = e.keys.iter().map(|k| score(query, k)).fold(0.0, f64::max);
        // Prefer shortcuts and App Paths over raw executables at equal scores.
        let s = s - if matches!(e.kind, EntryKind::Program | EntryKind::PathBin) { 0.005 } else { 0.0 };
        if s >= threshold && best.is_none_or(|(b, _)| s > b) {
            best = Some((s, e));
        }
    }
    best.map(|(s, e)| (s, e.clone()))
}

/// Best installed match for `name` (rebuilding a stale index once on a miss).
pub fn lookup(name: &str) -> Option<AppEntry> {
    let query = normalize(name);
    if query.len() < 2 {
        return None;
    }
    let (hit, stale) = {
        let index = INDEX.read().unwrap_or_else(|e| e.into_inner());
        (best(&index.entries, &query, MIN_SCORE), index.built.is_none_or(|t| t.elapsed() > MAX_AGE))
    };
    if hit.is_none() && stale {
        rebuild();
        let index = INDEX.read().unwrap_or_else(|e| e.into_inner());
        return best(&index.entries, &query, MIN_SCORE).map(|(_, e)| e);
    }
    if let Some((s, e)) = &hit {
        debug!("[APPS] '{query}' → {} ({:?}, score {s:.2})", e.name, e.kind);
    }
    hit.map(|(_, e)| e)
}

/// The suggestion when no app has that name: the closest shortcut name, if it is close ("spotfy").
pub fn nearest(name: &str) -> Option<AppEntry> {
    closest(&INDEX.read().unwrap_or_else(|e| e.into_inner()).entries, &normalize(name))
}

fn closest(entries: &[AppEntry], query: &str) -> Option<AppEntry> {
    let named: Vec<AppEntry> = entries.iter().filter(|e| !matches!(e.kind, EntryKind::Program | EntryKind::PathBin)).cloned().collect();
    (query.len() >= 3).then(|| best(&named, query, NEAREST_SCORE)).flatten().map(|(_, e)| e)
}

/// What the installed applications are called (their shortcuts and registered programs: the
/// names Sir would say).
pub fn names() -> Vec<String> {
    INDEX.read().unwrap_or_else(|e| e.into_inner()).entries.iter().filter(|e| !matches!(e.kind, EntryKind::Program | EntryKind::PathBin)).map(|e| e.name.clone()).collect()
}

/// In-memory check used by the intent parser: an installed app with (nearly) this exact name.
pub fn is_known(name: &str) -> bool {
    let query = normalize(name);
    if query.len() < 3 {
        return false;
    }
    let index = INDEX.read().unwrap_or_else(|e| e.into_inner());
    best(&index.entries, &query, 0.97).is_some()
}

fn entry(name: &str, target: PathBuf, kind: EntryKind, extra_key: Option<&str>) -> Option<AppEntry> {
    let key = normalize(name);
    if key.len() < 2 {
        return None;
    }
    let mut keys = vec![key];
    if let Some(extra) = extra_key.map(normalize).filter(|k| k.len() >= 2) {
        if !keys.contains(&extra) {
            keys.push(extra);
        }
    }
    Some(AppEntry { name: name.to_string(), target, kind, keys })
}

/// Names that are never what Sir means by "open X".
fn is_noise(name: &str) -> bool {
    let l = name.to_lowercase();
    [
        "uninstall", "deinstall", "readme", "help", "website", "documentation", "release notes", "license", "updater", "update",
        "crashpad", "crash handler", "crashreporter", "setup", "installer", "helper", "elevate", "notification_helper", "squirrel",
        "unins0", "vc_redist", "dotnet", "report", "bugreport", "telemetry",
    ]
    .iter()
    .any(|w| l.contains(w))
}

fn walk(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, visit);
            }
        } else {
            visit(&path);
        }
    }
}

/// The three sources are scanned in parallel (the program folders are the slow part).
#[cfg(windows)]
fn scan() -> Vec<AppEntry> {
    std::thread::scope(|scope| {
        let jobs = [scope.spawn(scan_shortcuts), scope.spawn(scan_app_paths), scope.spawn(scan_program_folders)];
        jobs.into_iter().flat_map(|job| job.join().unwrap_or_default()).collect()
    })
}

#[cfg(windows)]
fn env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

/// 1. Start-Menu shortcuts (what Windows search shows).
#[cfg(windows)]
fn scan_shortcuts() -> Vec<AppEntry> {
    let mut out = Vec::new();
    for base in ["ProgramData", "APPDATA"].iter().filter_map(|k| env(k)) {
        walk(&base.join("Microsoft\\Windows\\Start Menu\\Programs"), 4, &mut |path| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if ext != "lnk" && ext != "url" {
                return;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !is_noise(stem) {
                out.extend(entry(stem, path.to_path_buf(), EntryKind::Shortcut, None));
            }
        });
    }
    out
}

/// 2. Registry App Paths ("chrome.exe" → full path).
#[cfg(windows)]
fn scan_app_paths() -> Vec<AppEntry> {
    let mut out = Vec::new();
    for (name, path) in registry_app_paths() {
        let stem = name.trim_end_matches(".exe").trim_end_matches(".EXE");
        if !is_noise(stem) {
            out.extend(entry(stem, path, EntryKind::AppPath, None));
        }
    }
    out
}

/// 3. Program folders: executables in an app's folder (or its bin/), named after the exe and
///    the folder ("obs-studio\bin\64bit\obs64.exe" answers to "obs64" and "obs studio").
#[cfg(windows)]
fn scan_program_folders() -> Vec<AppEntry> {
    let mut out = Vec::new();
    let roots: Vec<PathBuf> = [env("LOCALAPPDATA").map(|p| p.join("Programs")), env("ProgramFiles"), env("ProgramFiles(x86)"), env("LOCALAPPDATA")]
        .into_iter()
        .flatten()
        .collect();
    for root in roots {
        let Ok(apps) = std::fs::read_dir(&root) else { continue };
        for app in apps.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            let folder = app.file_name().to_string_lossy().to_string();
            if is_noise(&folder) || folder.starts_with('.') || matches!(folder.as_str(), "Microsoft" | "Packages" | "Temp" | "Common Files" | "WindowsApps") {
                continue;
            }
            let mut found = 0;
            walk(&app.path(), 2, &mut |path| {
                if found >= 6 || !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
                    return;
                }
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if is_noise(stem) {
                    return;
                }
                found += 1;
                out.extend(entry(stem, path.to_path_buf(), EntryKind::Program, Some(&folder)));
            });
        }
    }
    out
}

#[cfg(windows)]
fn registry_app_paths() -> Vec<(String, PathBuf)> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
    };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let base = wide("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths");
    let mut out = Vec::new();
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        // SAFETY: registry handles are opened and closed here; buffers are sized and outlive the calls.
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(base.as_ptr()), 0, KEY_READ, &mut key) != ERROR_SUCCESS {
                continue;
            }
            for i in 0..2000u32 {
                let mut name = [0u16; 260];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(key, i, windows::core::PWSTR(name.as_mut_ptr()), &mut len, None, windows::core::PWSTR::null(), None, None)
                    != ERROR_SUCCESS
                {
                    break;
                }
                let sub_name = String::from_utf16_lossy(&name[..len as usize]);
                let mut sub = HKEY::default();
                let sub_w = wide(&sub_name);
                if RegOpenKeyExW(key, PCWSTR(sub_w.as_ptr()), 0, KEY_READ, &mut sub) != ERROR_SUCCESS {
                    continue;
                }
                let mut buf = [0u16; 1024];
                let mut size = (buf.len() * 2) as u32;
                let ok = RegQueryValueExW(sub, PCWSTR::null(), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut size)) == ERROR_SUCCESS;
                let _ = RegCloseKey(sub);
                if !ok {
                    continue;
                }
                let chars = (size as usize / 2).min(buf.len());
                let raw = String::from_utf16_lossy(&buf[..chars]);
                let path = raw.trim_matches(char::from(0)).trim().trim_matches('"').to_string();
                let path = PathBuf::from(expand_env(&path));
                if path.is_file() {
                    out.push((sub_name, path));
                }
            }
            let _ = RegCloseKey(key);
        }
    }
    out
}

#[cfg(windows)]
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let var = &after[..end];
                out.push_str(&std::env::var(var).unwrap_or_else(|_| format!("%{var}%")));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(not(windows))]
fn scan() -> Vec<AppEntry> {
    let mut out = Vec::new();
    let mut roots = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/snapd/desktop/applications"),
    ];
    if let Some(home) = crate::apps::home_dir() {
        roots.push(home.join(".local/share/applications"));
        roots.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    for root in roots {
        walk(&root, 2, &mut |path| {
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                return;
            }
            let Ok(text) = std::fs::read_to_string(path) else { return };
            if text.lines().any(|l| l.trim() == "NoDisplay=true") {
                return;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let name = text.lines().find_map(|l| l.strip_prefix("Name=")).unwrap_or(&stem).to_string();
            if !is_noise(&name) {
                let short = stem.rsplit('.').next().unwrap_or(&stem).to_string();
                out.extend(entry(&name, path.to_path_buf(), EntryKind::Desktop, Some(&short)));
            }
        });
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let Ok(files) = std::fs::read_dir(&dir) else { continue };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if name.contains('.') || name.len() < 3 {
                    continue;
                }
                use std::os::unix::fs::PermissionsExt;
                if f.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
                    out.extend(entry(&name, f.path(), EntryKind::PathBin, None));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, kind: EntryKind, folder: Option<&str>) -> AppEntry {
        entry(name, PathBuf::from(format!("C:/x/{name}")), kind, folder).unwrap()
    }

    #[test]
    fn fuzzy_lookup_prefers_close_names() {
        let entries = vec![
            e("Visual Studio Code", EntryKind::Shortcut, None),
            e("obs64", EntryKind::Program, Some("obs-studio")),
            e("Blender 4.2", EntryKind::Shortcut, None),
            e("GIMP 2.10", EntryKind::Shortcut, None),
        ];
        let find = |q: &str| best(&entries, &normalize(q), MIN_SCORE).map(|(_, e)| e.name);
        assert_eq!(find("blender").as_deref(), Some("Blender 4.2"));
        assert_eq!(find("blendr").as_deref(), Some("Blender 4.2"), "mumbled");
        assert_eq!(find("obs studio").as_deref(), Some("obs64"), "folder name");
        assert_eq!(find("gimp").as_deref(), Some("GIMP 2.10"));
        assert_eq!(find("code").as_deref(), Some("Visual Studio Code"));
        assert_eq!(find("photoshop"), None);
        assert!(is_noise("Uninstall Blender"));
        // The retry when nothing has that name: the closest shortcut, if it is close.
        let near = |q: &str| closest(&entries, &normalize(q)).map(|e| e.name);
        assert_eq!((find("gimpy"), near("gimpy").as_deref()), (None, Some("GIMP 2.10")));
        assert_eq!(near("photoshop"), None);
        assert_eq!(near("obs studios"), None, "only names Sir would say, not raw executables");
    }

    #[test]
    fn a_nearest_name_is_only_suggested() {
        // "open teams" with no Teams installed launched Steam (0.867): now it names Steam and starts nothing.
        let entries = vec![e("Steam", EntryKind::Shortcut, None), e("Spotify", EntryKind::Shortcut, None)];
        let near = |q: &str| closest(&entries, &normalize(q)).map(|e| e.name);
        assert_eq!(near("teams").as_deref(), Some("Steam"));
        assert_eq!(near("spotfy").as_deref(), Some("Spotify"));
        let reply = crate::apps::not_found("Teams", near("teams").as_deref());
        assert!(reply.starts_with("No Teams here, Sir. The closest is Steam."), "{reply}");
        assert!(!reply.to_lowercase().contains("opened"), "nothing was started: {reply}");
    }
}
