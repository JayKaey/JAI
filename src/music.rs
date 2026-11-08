//! Music: "play my playlist JaysRock", "play Back in Black in JaysRock", "pause the music",
//! "what's playing?".
//!
//! * Names are matched fuzzily and phonetically against Sir's library, so Whisper's "Jays Rock",
//!   "Jay's Rock", "jayrock" or "chase rock" all find the playlist "Jays Rock".
//! * The library is what Sir already has: the Spotify library Sonora (his Spotify client) keeps in
//!   `%LOCALAPPDATA%\sonora\cache.sqlite`, aliases Sir taught Jarvis (`music_library` table in
//!   `jarvis_memory.db`), and local playlists / audio files from the file index.
//! * Sonora receives local paths and provider links directly through its supported single-
//!   instance command-line handoff. Spotify uses its own URI handler. The selected player's
//!   Windows media session verifies playback; opening a search never counts as playing.

use crate::tools::str_arg;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use strsim::jaro_winkler;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Playlist,
    Track,
    Album,
    Artist,
    LocalPlaylist,
    LocalFile,
}

impl ItemKind {
    fn label(self) -> &'static str {
        match self {
            Self::Playlist | Self::LocalPlaylist => "playlist",
            Self::Track | Self::LocalFile => "song",
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryItem {
    pub kind: ItemKind,
    pub name: String,
    /// `spotify:playlist:…`, `spotify:track:…` or a file path.
    pub uri: String,
    pub artist: String,
    pub source: &'static str,
}

/// What the active media session is playing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NowPlaying {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
}

impl NowPlaying {
    pub fn describe(&self) -> String {
        let who = if self.artist.is_empty() { String::new() } else { format!(" by {}", self.artist) };
        format!("{}{who} ({}, {})", self.title, player_name(&self.app), if self.playing { "playing" } else { "paused" })
    }
}

fn player_name(app: &str) -> String {
    let a = app.to_lowercase();
    if a.contains("spotify") {
        "Spotify".into()
    } else if a.contains("sonora") {
        "Sonora".into()
    } else if a.contains("chrome") {
        "Chrome".into()
    } else if a.contains("edge") {
        "Edge".into()
    } else if a.contains("firefox") {
        "Firefox".into()
    } else {
        app.trim_end_matches(".exe").to_string()
    }
}

// ------------------------------------------------------------------------ name matching ----

/// Lower-case letters and digits only ("Jay's Rock!" → "jaysrock").
fn compact(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// A rough sound-alike key, so misheard names still match: "chase rock", "jays rock" and
/// "jaysrock" all become "jsrk".
fn phonetic(s: &str) -> String {
    let mut t = compact(s);
    for (from, to) in [("sch", "s"), ("ch", "j"), ("sh", "s"), ("ph", "f"), ("ck", "k"), ("qu", "kw"), ("c", "k"), ("z", "s"), ("x", "ks"), ("y", "i"), ("w", "v")] {
        t = t.replace(from, to);
    }
    let mut out = String::new();
    for (i, ch) in t.chars().enumerate() {
        let vowel = matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'ä' | 'ö' | 'ü');
        if (i == 0 || !vowel) && !out.ends_with(ch) {
            out.push(ch);
        }
    }
    out
}

/// How well a spoken name matches a library name (0-1).
pub fn name_score(query: &str, name: &str) -> f64 {
    let (q, n) = (compact(query), compact(name));
    if q.is_empty() || n.is_empty() {
        return 0.0;
    }
    if q == n {
        return 1.0;
    }
    let mut best = jaro_winkler(&q, &n);
    let (pq, pn) = (phonetic(query), phonetic(name));
    if pq.len() >= 3 && pn.len() >= 3 {
        best = best.max(if pq == pn { 0.95 } else { jaro_winkler(&pq, &pn) - 0.03 });
    }
    // "rock" inside "jays rock" is not enough, but "jays rock" inside "jays rock playlist" is.
    if q.len() >= 5 && (n.contains(&q) || q.contains(&n)) {
        best = best.max(0.9);
    }
    best
}

/// The best match above the bar, preferring `prefer` kinds on ties.
fn best_match<'a>(items: &'a [LibraryItem], query: &str, kinds: &[ItemKind], artist: Option<&str>) -> Option<(&'a LibraryItem, f64)> {
    items
        .iter()
        .filter(|i| kinds.contains(&i.kind))
        .map(|i| {
            let mut s = name_score(query, &i.name);
            if let Some(a) = artist.filter(|a| !a.is_empty()) {
                // The artist confirms (or contradicts) a song of that title.
                s += if name_score(a, &i.artist) >= 0.85 || compact(&i.artist).contains(&compact(a)) { 0.05 } else { -0.05 };
            }
            (i, s)
        })
        .filter(|(_, s)| *s >= 0.86)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

// ----------------------------------------------------------------------------- library ----

/// Where Sonora keeps its library cache.
fn sonora_binary() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from(r"D:\Program Files\Sonora\Sonora.exe")];
    for variable in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.extend([root.join("Sonora/Sonora.exe"), root.join("Programs/Sonora/Sonora.exe")]);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|p| p.join(if cfg!(windows) { "sonora.exe" } else { "sonora" })));
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Player { Sonora, Spotify, Local }

impl Player {
    fn id(self) -> &'static str { match self { Self::Sonora => "sonora", Self::Spotify => "spotify", Self::Local => "local" } }
}

fn choose_player(preference: &str, sonora_running: bool, spotify_running: bool, sonora_installed: bool) -> Player {
    match preference {
        "spotify" => Player::Spotify,
        "sonora" => Player::Sonora,
        "local" | "file" => Player::Local,
        _ if sonora_running => Player::Sonora,
        _ if spotify_running => Player::Spotify,
        _ if sonora_installed => Player::Sonora,
        _ => Player::Spotify,
    }
}

async fn detect_player(preference: &str) -> Player {
    let (mut sonora, mut spotify) = tokio::task::spawn_blocking(|| {
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::All);
        let mut sonora = false;
        let mut spotify = false;
        for process in system.processes().values() {
            let name = process.name().to_string_lossy().to_lowercase();
            sonora |= matches!(name.as_str(), "sonora" | "sonora.exe");
            spotify |= matches!(name.as_str(), "spotify" | "spotify.exe");
        }
        (sonora, spotify)
    }).await.unwrap_or_default();
    #[cfg(windows)]
    if let Ok(windows) = crate::win32::call(crate::win32::app_windows).await {
        sonora |= windows.iter().any(|w| w.process.eq_ignore_ascii_case("sonora") || w.title.eq_ignore_ascii_case("Sonora"));
        spotify |= windows.iter().any(|w| w.process.eq_ignore_ascii_case("spotify") || w.title.eq_ignore_ascii_case("Spotify"));
    }
    choose_player(preference, sonora, spotify, sonora_binary().is_some())
}

fn artists_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from).or_else(|| x["name"].as_str().map(String::from)))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(o) => o.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// Where a saved Spotify song sits on its album (track URI → album URI, disc, track number):
/// Sonora starts an album handed to it, so a song can be reached by skipping to its number.
type AlbumPosition = (String, u32, u32);

static TRACK_ALBUMS: std::sync::LazyLock<std::sync::RwLock<HashMap<String, AlbumPosition>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

fn album_position(track_uri: &str) -> Option<AlbumPosition> {
    TRACK_ALBUMS.read().ok()?.get(track_uri).cloned()
}

/// Playlists, saved songs, albums and artists from Sonora's Spotify library snapshot.
fn read_sonora(path: &Path) -> Vec<LibraryItem> {
    let uri = format!("file:{}?mode=ro", path.display().to_string().replace('\\', "/"));
    let conn = match Connection::open_with_flags(&uri, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI) {
        Ok(c) => c,
        Err(e) => {
            warn!("[MUSIC] Sonora's library is not readable: {e}");
            return Vec::new();
        }
    };
    let _ = conn.busy_timeout(Duration::from_millis(500));
    let mut items = Vec::new();
    for (key, kind) in [("spotify/playlists", ItemKind::Playlist), ("spotify/songs", ItemKind::Track), ("spotify/albums", ItemKind::Album), ("spotify/artists", ItemKind::Artist)] {
        let raw: Option<String> = conn.query_row("SELECT value FROM snapshots WHERE key = ?1", params![key], |r| r.get(0)).ok();
        let Some(v) = raw.and_then(|r| serde_json::from_str::<Value>(&r).ok()) else { continue };
        for row in v["rows"].as_array().into_iter().flatten() {
            let (Some(id), Some(name)) = (row["id"].as_str(), row["name"].as_str()) else { continue };
            if name.trim().is_empty() || name == "Unknown" {
                continue;
            }
            let prefix = match kind {
                ItemKind::Playlist => "playlist",
                ItemKind::Track => "track",
                ItemKind::Album => "album",
                _ => "artist",
            };
            let artist = match kind {
                ItemKind::Playlist => row["owner"].as_str().unwrap_or("").to_string(),
                _ => artists_text(&row["artists"]),
            };
            if kind == ItemKind::Track {
                if let (Some(album), Some(number)) = (row["album_id"].as_str().filter(|a| !a.is_empty()), row["track_number"].as_u64()) {
                    let disc = row["disc_number"].as_u64().unwrap_or(1) as u32;
                    if let Ok(mut albums) = TRACK_ALBUMS.write() {
                        albums.insert(format!("spotify:track:{id}"), (format!("spotify:album:{album}"), disc, number as u32));
                    }
                }
            }
            items.push(LibraryItem { kind, name: name.trim().to_string(), uri: format!("spotify:{prefix}:{id}"), artist, source: "Sonora library" });
        }
    }
    items
}

struct Cache {
    stamp: Option<SystemTime>,
    items: Vec<LibraryItem>,
    members: HashMap<String, Vec<LibraryItem>>,
    /// The files and folders the last scan read, each with its size and time just after it.
    sources: Vec<(PathBuf, Option<FileStamp>)>,
}

/// A file's (or folder's) size and last write.
type FileStamp = (u64, SystemTime);

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Within this of a scan the library is used as read, without a look at its files.
const LIBRARY_FRESH: Duration = Duration::from_secs(30);
/// At the latest after this it is read again (a song added deep inside a folder changes none of
/// the files a scan read).
const LIBRARY_RESCAN: Duration = Duration::from_secs(10 * 60);

/// Whether the library read `age` ago is still Sonora's. Between half a minute and ten minutes it
/// is, while none of the files and folders it was read from changed: a dozen stats instead of a
/// scan of Sonora's cache (70 MB here, 60 ms a time and twice that beside a game, 2026-11-08,
/// for every music command more than half a minute after the last one).
fn library_current(age: Option<Duration>, sources: &[(PathBuf, Option<FileStamp>)]) -> bool {
    match age {
        None => false,
        Some(age) if age <= LIBRARY_FRESH => true,
        Some(age) if age > LIBRARY_RESCAN => false,
        Some(_) => sources.iter().all(|(path, then)| file_stamp(path) == *then),
    }
}

/// Bounded, read-only roots; the walker never follows links out into unrelated user folders.
fn library_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(r"D:\Program Files\Sonora")];
    for variable in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) { roots.push(PathBuf::from(root).join("Sonora")); }
    }
    if let Some(home) = crate::apps::home_dir() {
        roots.extend([home.join("Music"), home.join("OneDrive/Music"), home.join(".local/share/sonora"), home.join(".cache/sonora")]);
    }
    roots.retain(|p| p.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

fn local_item(path: &Path, name: Option<&str>) -> Option<LibraryItem> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let kind = match ext.as_str() {
        "m3u" | "m3u8" | "pls" => ItemKind::LocalPlaylist,
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "opus" | "aac" => ItemKind::LocalFile,
        _ => return None,
    };
    Some(LibraryItem { kind, name: name.map(str::to_string).unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
        uri: path.to_string_lossy().into_owned(), artist: String::new(), source: "local library" })
}

fn playlist_tracks(path: &Path) -> Vec<LibraryItem> {
    let Ok(meta) = std::fs::metadata(path) else { return Vec::new() };
    if meta.len() > 4 * 1024 * 1024 { return Vec::new(); }
    let Ok(raw) = std::fs::read(path) else { return Vec::new() };
    let text = String::from_utf8_lossy(&raw);
    let mut title = None;
    let mut tracks = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if let Some((_, name)) = line.strip_prefix("#EXTINF:").and_then(|s| s.split_once(',')) {
            title = Some(name.trim().to_string()); continue;
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') { continue; }
        let line = if line.starts_with("File") { line.split_once('=').map(|(_, path)| path).unwrap_or(line) } else { line };
        if line.contains("://") { continue; }
        let candidate = PathBuf::from(line);
        let candidate = if candidate.is_absolute() { candidate } else { path.parent().unwrap_or(Path::new(".")).join(candidate) };
        if candidate.is_file() {
            if let Some(mut item) = local_item(&candidate, title.as_deref()) {
                if let Some((artist, name)) = item.name.split_once(" - ") { item.artist = artist.to_string(); item.name = name.to_string(); }
                tracks.push(item);
            }
        }
        title = None;
    }
    tracks
}

fn json_library(value: &Value, base: &Path, items: &mut Vec<LibraryItem>, members: &mut HashMap<String, Vec<LibraryItem>>, depth: usize) {
    if depth > 16 || items.len() > 50_000 { return; }
    match value {
        Value::String(raw) => {
            let path = PathBuf::from(raw);
            let path = if path.is_absolute() { path } else { base.join(path) };
            if path.is_file() { if let Some(item) = local_item(&path, None) { items.push(item); } }
        }
        Value::Array(rows) => for row in rows.iter().take(50_000) { json_library(row, base, items, members, depth + 1); },
        Value::Object(row) => {
            let name = row.get("name").or_else(|| row.get("title")).and_then(Value::as_str);
            let raw = row.get("uri").or_else(|| row.get("path")).or_else(|| row.get("file")).or_else(|| row.get("url")).and_then(Value::as_str);
            let inferred_uri = row.get("id").and_then(Value::as_str).filter(|id| id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric())).map(|id| format!("spotify:track:{id}"));
            if let (Some(name), Some(raw)) = (name, raw.or(inferred_uri.as_deref())) {
                let item = if let Some(uri) = spotify_uri(raw) {
                    let kind = if uri.contains(":playlist:") { ItemKind::Playlist } else if uri.contains(":album:") { ItemKind::Album } else { ItemKind::Track };
                    Some(LibraryItem { kind, name: name.into(), uri, artist: artists_text(&value["artists"]), source: "Sonora JSON" })
                } else {
                    let path = PathBuf::from(raw);
                    let path = if path.is_absolute() { path } else { base.join(path) };
                    path.is_file().then(|| local_item(&path, Some(name))).flatten()
                };
                if let Some(item) = item { items.push(item); }
            }
            if let Some(tracks) = row.get("tracks").or_else(|| row.get("songs")) {
                let mut children = Vec::new();
                json_library(tracks, base, &mut children, members, depth + 1);
                if let Some(name) = name { members.entry(compact(name)).or_default().extend(children.iter().cloned()); }
                items.extend(children);
            }
            for (key, child) in row {
                if !matches!(key.as_str(), "tracks" | "songs" | "token" | "password" | "credentials" | "access_token" | "refresh_token")
                    && (child.is_array() || child.is_object()) { json_library(child, base, items, members, depth + 1); }
            }
        }
        _ => {}
    }
}

fn read_playlist_cache(path: &Path, items: &mut Vec<LibraryItem>, members: &mut HashMap<String, Vec<LibraryItem>>) {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return };
    let _ = conn.busy_timeout(Duration::from_millis(300));
    if let Ok(mut stmt) = conn.prepare("SELECT key,value FROM snapshots WHERE key LIKE '%playlist%' LIMIT 5000") {
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for (key, raw) in rows.flatten().filter(|(_, raw)| raw.len() <= 8 * 1024 * 1024) {
                if key.ends_with("/playlists") { continue; }
                let Ok(value) = serde_json::from_str::<Value>(&raw) else { continue };
                let mut children = Vec::new();
                json_library(&value, path.parent().unwrap_or(Path::new(".")), &mut children, members, 0);
                if let Some(playlist) = items.iter().find(|i| i.kind == ItemKind::Playlist && i.uri.rsplit(':').next().is_some_and(|id| key.contains(id))) {
                    members.insert(compact(&playlist.name), children.clone());
                }
                items.extend(children);
            }
        }
    }
    // Sonora's local state database stores ordered playlist memberships as actual local IDs.
    // Only IDs resolving to an existing supported audio file are included.
    if let Ok(mut stmt) = conn.prepare("SELECT p.name,t.track_id FROM playlists p JOIN playlist_tracks t ON t.playlist_id=p.id ORDER BY p.id,t.position LIMIT 50000") {
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for (name, id) in rows.flatten() {
                let decoded = urlencoding::decode(id.trim_start_matches("file://")).map(|s| s.into_owned()).unwrap_or(id);
                let candidate = PathBuf::from(decoded.trim_start_matches("local:"));
                if candidate.is_file() {
                    if let Some(item) = local_item(&candidate, None) {
                        members.entry(compact(&name)).or_default().push(item.clone());
                        items.push(item);
                        let uri = format!("sonora-local:{}:{name}", path.display());
                        if !items.iter().any(|i| i.uri == uri) { items.push(LibraryItem { kind: ItemKind::LocalPlaylist, name, uri, artist: String::new(), source: "Sonora local playlists" }); }
                    }
                }
            }
        }
    };
    // Playlists Sonora downloaded keep their whole ordered track list in `downloaded_sets`
    // (state.sqlite): "Jays Rock" with all 125 songs, in playlist order.
    if let Ok(mut stmt) = conn.prepare("SELECT value FROM downloaded_sets WHERE kind='playlist' LIMIT 500") {
        let rows = stmt.query_map([], |r| r.get::<_, String>(0));
        if let Ok(rows) = rows {
            for raw in rows.flatten().filter(|raw| raw.len() <= 16 * 1024 * 1024) {
                if let Some((name, tracks)) = downloaded_playlist(&raw) {
                    members.insert(compact(&name), tracks.clone());
                    items.extend(tracks);
                }
            }
        }
    };
}

/// One `downloaded_sets` row → the playlist's name and its tracks in order.
pub fn downloaded_playlist(raw: &str) -> Option<(String, Vec<LibraryItem>)> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let head = v["head"].as_object()?.values().next()?;
    let name = head["name"].as_str()?.trim().to_string();
    let tracks: Vec<LibraryItem> = v["tracks"]
        .as_array()?
        .iter()
        .filter_map(|t| {
            let (id, title) = (t["id"].as_str()?, t["name"].as_str()?);
            let uri = format!("spotify:track:{id}");
            if let (Some(album), Some(number)) = (t["album_id"].as_str().filter(|a| !a.is_empty()), t["track_number"].as_u64()) {
                if let Ok(mut albums) = TRACK_ALBUMS.write() {
                    albums.insert(uri.clone(), (format!("spotify:album:{album}"), t["disc_number"].as_u64().unwrap_or(1) as u32, number as u32));
                }
            }
            Some(LibraryItem { kind: ItemKind::Track, name: title.trim().to_string(), uri, artist: artists_text(&t["artists"]), source: "Sonora downloaded playlist" })
        })
        .collect();
    (!name.is_empty() && !tracks.is_empty()).then_some((name, tracks))
}

/// The best entry for `song` inside a playlist's ordered members: (index, track).
pub fn playlist_position<'a>(members: &'a [LibraryItem], song: &str, artist: Option<&str>) -> Option<(usize, &'a LibraryItem)> {
    members
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m.kind, ItemKind::Track | ItemKind::LocalFile))
        .map(|(i, m)| {
            let by = artist.filter(|a| !a.is_empty() && !m.artist.is_empty()).map_or(0.0, |a| name_score(a, &m.artist) * 0.05);
            (i, m, name_score(song, &m.name) + by)
        })
        .filter(|(_, _, score)| *score >= 0.8)
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(i, m, _)| (i, m))
}

/// The library: its items, each playlist's members, and the files and folders it was read from
/// (`library_current` looks at them before the next scan).
fn scan_library() -> (Vec<LibraryItem>, HashMap<String, Vec<LibraryItem>>, Vec<PathBuf>) {
    let mut items = Vec::new();
    let mut members = HashMap::new();
    let mut seen = HashSet::new();
    let mut sources: Vec<PathBuf> = Vec::new();
    for root in library_roots() {
        sources.push(root.clone());
        for entry in walkdir::WalkDir::new(root).follow_links(false).max_depth(12).into_iter().filter_entry(|e| {
            !matches!(e.file_name().to_str().unwrap_or(""), "node_modules" | ".git" | "GPUCache" | "Code Cache")
        }).filter_map(Result::ok).filter(|e| e.file_type().is_file()).take(50_000) {
            let path = entry.path();
            if !seen.insert(path.to_path_buf()) { continue; }
            let ext = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();
            if let Some(item) = local_item(path, None) {
                if item.kind == ItemKind::LocalPlaylist {
                    sources.push(path.to_path_buf());
                    let tracks = playlist_tracks(path);
                    members.insert(compact(&item.name), tracks.clone());
                    items.extend(tracks);
                }
                items.push(item);
            } else if matches!(ext.as_str(), "sqlite" | "sqlite3" | "db") {
                // (A database in WAL mode takes its writes in the "-wal" file first.)
                sources.push(path.to_path_buf());
                sources.push(PathBuf::from(format!("{}-wal", path.display())));
                items.extend(read_sonora(path));
                read_playlist_cache(path, &mut items, &mut members);
            } else if ext == "json" && entry.metadata().is_ok_and(|m| m.len() <= 8 * 1024 * 1024) {
                sources.push(path.to_path_buf());
                if let Some(value) = std::fs::read(path).ok().and_then(|s| serde_json::from_slice::<Value>(&s).ok()) {
                    json_library(&value, path.parent().unwrap_or(Path::new(".")), &mut items, &mut members, 0);
                    if value.get("tracks").or_else(|| value.get("songs")).is_some() {
                        if let Some(name) = value["name"].as_str().or_else(|| value["title"].as_str()) {
                            if members.get(&compact(name)).is_some_and(|tracks| !tracks.is_empty()) {
                                items.push(LibraryItem { kind: ItemKind::LocalPlaylist, name: name.into(), uri: path.to_string_lossy().into_owned(), artist: String::new(), source: "JSON playlist" });
                            }
                        }
                    }
                }
            }
        }
    }
    let mut unique = HashSet::new();
    items.retain(|i| unique.insert((i.uri.clone(), i.name.clone())));
    // And the folders they lie in: a playlist saved next to them changes its folder.
    let folders: Vec<PathBuf> = sources.iter().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
    sources.extend(folders);
    sources.sort();
    sources.dedup();
    (items, members, sources)
}

// ------------------------------------------------------------------------- track index ----
// Every individual song Sir owns, so "play Highway to Hell" finds the exact file or Spotify
// track: audio files on his drives (with their tags), the members of every local, JSON and Sonora
// playlist, and his saved Spotify songs. Kept in memory and in SQLite (`sonora_tracks`).

/// One song of the track index.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedTrack {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// A file path or a `spotify:track:` URI.
    pub path: String,
    /// The playlist it belongs to ("" for a loose file).
    pub playlist: String,
    pub source: String,
}

static TRACKS: std::sync::RwLock<Vec<IndexedTrack>> = std::sync::RwLock::new(Vec::new());
static INDEXER: std::sync::Once = std::sync::Once::new();
/// Audio files smaller than this are sound effects or ringtones, not songs.
const MIN_SONG_BYTES: u64 = 600 * 1024;
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "m4a", "ogg", "opus", "aac"];

/// The tracks indexed so far.
pub fn indexed_tracks() -> Vec<IndexedTrack> {
    TRACKS.read().map(|t| t.clone()).unwrap_or_default()
}

/// Folders that never hold Sir's music (system, games, developer trees, caches).
fn skip_dir(name: &str) -> bool {
    let n = name.to_lowercase();
    (n.starts_with('.') && n != ".music")
        || n.starts_with('$')
        || matches!(
            n.as_str(),
            "windows" | "system volume information" | "program files" | "program files (x86)" | "programdata" | "steamlibrary" | "steamapps" | "steam"
                | "epic games" | "xboxgames" | "riot games" | "battle.net" | "games" | "node_modules" | "target" | "appdata" | "site-packages"
                | "venv" | "__pycache__" | "models" | "runtime" | "cache" | "gpucache" | "code cache" | "windowsapps" | "msocache" | "recovery"
                | "temp" | "tmp" | "minecraft" | "resourcepacks" | "shaderpacks" | "mods" | "assets" | "sounds" | "sfx" | "jarvis_replay"
        )
}

/// Where Sir's songs may live: his Music/Downloads folders, Sonora's folders, and the data
/// drive D:\ (walked with the folders above skipped).
fn audio_roots() -> Vec<PathBuf> {
    let mut roots = library_roots();
    if let Some(home) = crate::apps::home_dir() {
        roots.extend([home.join("Music"), home.join("Downloads"), home.join("OneDrive/Music"), home.join("Desktop")]);
    }
    #[cfg(windows)]
    roots.push(PathBuf::from(r"D:\"));
    roots.retain(|p| p.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

/// Audio files under `roots` with their tags (bounded in files visited and time).
fn scan_audio_files(roots: &[PathBuf]) -> Vec<IndexedTrack> {
    let started = std::time::Instant::now();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut visited = 0usize;
    for root in roots {
        let walker = walkdir::WalkDir::new(root).follow_links(false).max_depth(10).into_iter().filter_entry(|e| {
            e.depth() == 0 || !e.file_type().is_dir() || !skip_dir(&e.file_name().to_string_lossy())
        });
        for entry in walker.filter_map(Result::ok) {
            visited += 1;
            if visited > 400_000 || out.len() >= 60_000 || started.elapsed() > Duration::from_secs(240) {
                warn!("[MUSIC] Track index scan stopped early ({visited} entries, {} songs, {:?})", out.len(), started.elapsed());
                return out;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !AUDIO_EXTENSIONS.contains(&ext.as_str()) || entry.metadata().map(|m| m.len()).unwrap_or(0) < MIN_SONG_BYTES {
                continue;
            }
            if !seen.insert(path.to_path_buf()) {
                continue;
            }
            let tags = crate::audio_tags::read(path);
            out.push(IndexedTrack {
                title: tags.title,
                artist: tags.artist,
                album: tags.album,
                path: path.to_string_lossy().into_owned(),
                playlist: String::new(),
                source: "local file".into(),
            });
        }
    }
    out
}

/// The whole index: local files, playlist members (local and Sonora) and saved Spotify songs.
fn build_track_index() -> Vec<IndexedTrack> {
    let (items, members, _) = scan_library();
    let mut tracks = scan_audio_files(&audio_roots());
    let tagged: HashMap<String, (String, String, String)> =
        tracks.iter().map(|t| (t.path.clone(), (t.title.clone(), t.artist.clone(), t.album.clone()))).collect();
    let playlists: HashMap<String, String> = items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Playlist | ItemKind::LocalPlaylist))
        .map(|i| (compact(&i.name), i.name.clone()))
        .collect();
    for (key, list) in &members {
        let playlist = playlists.get(key).cloned().unwrap_or_else(|| key.clone());
        for item in list.iter().filter(|i| matches!(i.kind, ItemKind::Track | ItemKind::LocalFile)) {
            let (title, artist, album) = tagged.get(&item.uri).cloned().unwrap_or_else(|| (item.name.clone(), item.artist.clone(), String::new()));
            tracks.push(IndexedTrack { title, artist, album, path: item.uri.clone(), playlist: playlist.clone(), source: item.source.to_string() });
        }
    }
    for item in items.iter().filter(|i| i.kind == ItemKind::Track) {
        tracks.push(IndexedTrack {
            title: item.name.clone(),
            artist: item.artist.clone(),
            album: String::new(),
            path: item.uri.clone(),
            playlist: "Liked Songs".into(),
            source: item.source.to_string(),
        });
    }
    let mut unique = HashSet::new();
    tracks.retain(|t| !t.title.trim().is_empty() && unique.insert((t.path.clone(), t.playlist.clone())));
    tracks
}

fn store_tracks(conn: &mut Connection, tracks: &[IndexedTrack]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM sonora_tracks", [])?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO sonora_tracks (path, playlist, title, artist, album, source, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        )?;
        for t in tracks {
            insert.execute(params![t.path, t.playlist, t.title, t.artist, t.album, t.source])?;
        }
    }
    tx.commit()
}

fn load_tracks(conn: &Connection) -> Vec<IndexedTrack> {
    let Ok(mut stmt) = conn.prepare("SELECT title, artist, album, path, playlist, source FROM sonora_tracks") else { return Vec::new() };
    stmt.query_map([], |r| {
        Ok(IndexedTrack { title: r.get(0)?, artist: r.get(1)?, album: r.get(2)?, path: r.get(3)?, playlist: r.get(4)?, source: r.get(5)? })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// Loads the stored index at once and refreshes it in the background (shortly after start-up,
/// then every 30 minutes), off every critical path.
fn spawn_track_indexer(db_path: PathBuf) {
    INDEXER.call_once(move || {
        if let Ok(conn) = Connection::open(&db_path) {
            let stored = load_tracks(&conn);
            if !stored.is_empty() {
                info!("[MUSIC] Track index: {} song(s) from the last scan", stored.len());
                if let Ok(mut t) = TRACKS.write() {
                    *t = stored;
                }
            }
        }
        let spawned = std::thread::Builder::new().name("jarvis-track-index".into()).spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            loop {
                let started = std::time::Instant::now();
                let tracks = build_track_index();
                let local = tracks.iter().filter(|t| !t.path.starts_with("spotify:")).count();
                info!(
                    "[MUSIC] Track index: {} song(s) ({local} local files / playlist entries, {} Spotify) in {:.1} s",
                    tracks.len(),
                    tracks.len() - local,
                    started.elapsed().as_secs_f32()
                );
                match Connection::open(&db_path) {
                    Ok(mut conn) => {
                        let _ = conn.busy_timeout(Duration::from_secs(5));
                        if let Err(e) = store_tracks(&mut conn, &tracks) {
                            warn!("[MUSIC] The track index could not be stored: {e}");
                        }
                    }
                    Err(e) => warn!("[MUSIC] The track index database is unavailable: {e}"),
                }
                if let Ok(mut t) = TRACKS.write() {
                    *t = tracks;
                }
                std::thread::sleep(Duration::from_secs(30 * 60));
            }
        });
        if let Err(e) = spawned {
            warn!("[MUSIC] The track indexer could not start: {e}");
        }
    });
}

/// Indexed songs as library items (local files only: Spotify songs are already in the library).
fn index_items() -> Vec<LibraryItem> {
    TRACKS
        .read()
        .map(|tracks| {
            tracks
                .iter()
                .filter(|t| !t.path.starts_with("spotify:") && !t.path.starts_with("sonora-local:"))
                .map(|t| LibraryItem { kind: ItemKind::LocalFile, name: t.title.clone(), uri: t.path.clone(), artist: t.artist.clone(), source: "track index" })
                .collect()
        })
        .unwrap_or_default()
}

/// The library as last read, for the instant command parser ("play JaysRock" without saying
/// "playlist"): it must answer in microseconds and has no engine at hand.
static SNAPSHOT: std::sync::RwLock<Vec<LibraryItem>> = std::sync::RwLock::new(Vec::new());

/// Loads the library snapshot (start-up, off the critical path).
pub fn warm_up() {
    if SNAPSHOT.read().map(|s| s.is_empty()).unwrap_or(false) {
        let (items, ..) = scan_library();
        if let Ok(mut s) = SNAPSHOT.write() {
            *s = items;
        }
    }
    // The media session manager is asked for once, up front: "stop" then acts in milliseconds.
    #[cfg(windows)]
    smtc::warm();
}

/// A fixed library for the parser's tests.
#[cfg(test)]
pub fn set_snapshot(items: Vec<LibraryItem>) {
    if let Ok(mut s) = SNAPSHOT.write() {
        *s = items;
    }
}

/// What a spoken name is in Sir's library, when it clearly is something (a stricter bar than
/// the music tool's, since a false hit would hijack "play Minecraft").
pub fn quick_lookup(name: &str) -> Option<ItemKind> {
    let snapshot = SNAPSHOT.read().ok()?;
    let kinds = [ItemKind::Playlist, ItemKind::Track, ItemKind::Artist, ItemKind::Album, ItemKind::LocalPlaylist, ItemKind::LocalFile];
    snapshot
        .iter()
        .filter(|i| kinds.contains(&i.kind))
        .map(|i| (i.kind, name_score(name, &i.name)))
        .filter(|(_, s)| *s >= 0.92)
        // Playlists win ties: "play JaysRock" is the playlist.
        .max_by(|a, b| a.1.total_cmp(&b.1).then((b.0 as u8).cmp(&(a.0 as u8))))
        .map(|(k, _)| k)
}

struct Inner {
    conn: Option<Mutex<Connection>>,
    cache: Mutex<Cache>,
    /// The playlist (or song) Jarvis started last (protocols capture it).
    last_played: Mutex<Option<LibraryItem>>,
}

#[derive(Clone)]
pub struct MusicEngine {
    inner: Arc<Inner>,
}

impl MusicEngine {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS music_library (
                 alias TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 uri TEXT NOT NULL,
                 kind TEXT NOT NULL DEFAULT 'playlist',
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE IF NOT EXISTS music_preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sonora_tracks (
                 path TEXT NOT NULL,
                 playlist TEXT NOT NULL DEFAULT '',
                 title TEXT NOT NULL,
                 artist TEXT NOT NULL DEFAULT '',
                 album TEXT NOT NULL DEFAULT '',
                 source TEXT NOT NULL DEFAULT '',
                 indexed_at TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (path, playlist)
             );",
        )?;
        spawn_track_indexer(db_path.as_ref().to_path_buf());
        Ok(Self::with(Some(conn)))
    }

    pub fn in_memory() -> Self {
        Self::with(None)
    }

    fn with(conn: Option<Connection>) -> Self {
        Self {
            inner: Arc::new(Inner {
                conn: conn.map(Mutex::new),
                cache: Mutex::new(Cache { stamp: None, items: Vec::new(), members: HashMap::new(), sources: Vec::new() }),
                last_played: Mutex::new(None),
            }),
        }
    }

    pub fn last_played(&self) -> Option<LibraryItem> {
        self.inner.last_played.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Saved aliases first (Sir's own names win), then Sonora's library (re-read when it changed).
    pub fn library(&self) -> Vec<LibraryItem> {
        let mut items = self.saved();
        let mut cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        let age = cache.stamp.map(|stamp| stamp.elapsed().unwrap_or_default());
        if !library_current(age, &cache.sources) {
            let (indexed, members, sources) = scan_library();
            cache.items = indexed;
            cache.members = members;
            // Stamped after the scan: whatever reading them touched belongs to the baseline.
            cache.sources = sources.into_iter().map(|path| {
                let stamp = file_stamp(&path);
                (path, stamp)
            }).collect();
            cache.stamp = Some(SystemTime::now());
            info!("[MUSIC] Library: {} item(s) from Sonora", cache.items.len());
            if let Ok(mut s) = SNAPSHOT.write() {
                *s = cache.items.clone();
            }
        }
        items.extend(cache.items.iter().cloned());
        drop(cache);
        // Individual songs on Sir's drives, with their tags (the background track index).
        let known: HashSet<String> = items.iter().map(|i| i.uri.clone()).collect();
        items.extend(index_items().into_iter().filter(|i| !known.contains(&i.uri)));
        items
    }

    fn saved(&self) -> Vec<LibraryItem> {
        let Some(conn) = &self.inner.conn else { return Vec::new() };
        let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut stmt) = conn.prepare("SELECT alias, name, uri, kind FROM music_library") else { return Vec::new() };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .map(|rows| {
                rows.flatten()
                    .map(|(alias, _name, uri, kind)| LibraryItem {
                        kind: if kind == "track" { ItemKind::Track } else if uri.contains(":album:") { ItemKind::Album } else { ItemKind::Playlist },
                        name: alias,
                        uri,
                        artist: String::new(),
                        source: "saved",
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn save_alias(&self, alias: &str, target: &str) -> Result<String, String> {
        let conn = self.inner.conn.as_ref().ok_or("Music aliases are not persisted in this session.")?;
        // A URI / link, or the name of something already in the library.
        let (name, uri, kind) = if let Some(uri) = spotify_uri(target) {
            (alias.to_string(), uri.clone(), if uri.contains(":track:") { "track" } else { "playlist" })
        } else {
            let items = self.library();
            let (item, _) = best_match(&items, target, &[ItemKind::Playlist, ItemKind::Album, ItemKind::Track], None).ok_or_else(|| format!("Nothing in the library matches '{target}'."))?;
            (item.name.clone(), item.uri.clone(), if item.kind == ItemKind::Track { "track" } else { "playlist" })
        };
        conn.lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(
                "INSERT INTO music_library (alias, name, uri, kind, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))
                 ON CONFLICT(alias) DO UPDATE SET name = excluded.name, uri = excluded.uri, kind = excluded.kind, updated_at = excluded.updated_at",
                params![alias.trim(), name, uri, kind],
            )
            .map_err(|e| e.to_string())?;
        Ok(format!("Saved '{alias}' → {name} ({uri})."))
    }

    // --------------------------------------------------------------------- the tool ----

    /// `control_music(action, song_name, playlist_name, artist_name, player_preference, level)`.
    pub async fn control(&self, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action", "command"]).unwrap_or("play").to_lowercase().replace([' ', '-'], "_");
        let song = str_arg(args, &["song_name", "song", "track", "title"]);
        let playlist = str_arg(args, &["playlist_name", "playlist"]);
        let artist = str_arg(args, &["artist_name", "artist"]);
        let album = str_arg(args, &["album_name", "album"]);
        let requested = str_arg(args, &["player_preference", "player", "app"]).unwrap_or("auto").to_lowercase();
        let preferred = self.inner.conn.as_ref().and_then(|conn| {
            let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
            conn.query_row("SELECT value FROM music_preferences WHERE key='player'", [], |r| r.get::<_, String>(0)).ok()
        });
        let preference = if requested == "auto" { preferred.as_deref().unwrap_or("auto") } else { &requested };
        let player = detect_player(preference).await.id().to_string();
        match action.as_str() {
            "set_preferred_player" => {
                if !matches!(requested.as_str(), "auto" | "sonora" | "spotify" | "local") { return Err("Choose auto, sonora, spotify or local.".into()); }
                let conn = self.inner.conn.as_ref().ok_or("Music preference storage is unavailable.")?;
                conn.lock().unwrap_or_else(|e| e.into_inner()).execute("INSERT INTO music_preferences(key,value) VALUES('player',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [&requested]).map_err(|e| e.to_string())?;
                Ok(format!("Preferred music player set to {requested}."))
            }
            "welcome_home" => {
                match self.play(Some("Highway to Hell"), Some("JaysRock"), Some("AC/DC"), None, &player).await {
                    Ok(result) => Ok(result),
                    Err(first) => {
                        warn!("[MUSIC] Welcome playback failed: {first}");
                        if let Ok(result) = self.play(Some("Highway to Hell"), None, Some("AC/DC"), None, &player).await { return Ok(result); }
                        if player != "spotify" {
                            if let Ok(result) = self.play(Some("Highway to Hell"), None, Some("AC/DC"), None, "spotify").await { return Ok(result); }
                        }
                        let url = "https://music.youtube.com/search?q=AC%2FDC%20Highway%20to%20Hell";
                        match open_uri(url).await {
                            Ok(()) => Err(format!("Playback could not be verified. I opened the AC/DC Highway to Hell search in YouTube Music; select Play there. {first}")),
                            Err(error) => Err(format!("Playback could not be started: {first}. The YouTube Music fallback also failed: {error}")),
                        }
                    }
                }
            }
            "pick_music" | "play_something" | "surprise_me" | "choose_music" => self.pick_music(&player).await,
            "play_from_playlist" | "play_track_from_playlist" | "play_in_playlist" => {
                let song = song.ok_or("play_from_playlist needs song_name.")?;
                let list = playlist.ok_or("play_from_playlist needs playlist_name.")?;
                self.play_track_directly_from_playlist(song, list, artist, &player).await
            }
            "play" | "start" | "play_playlist" | "play_song" | "play_track" | "shuffle" | "play_artist" | "play_album" => {
                if song.is_none() && playlist.is_none() && artist.is_none() && album.is_none() {
                    return session_control(SessionOp::Play, &player).await;
                }
                self.play(song, playlist, artist, album, &player).await
            }
            "pause" | "stop" => session_control(SessionOp::Pause, &player).await,
            "stop_all" | "stop_everything" | "pause_all" | "pause_everything" => Ok(stop_all_active_media().await.describe()),
            "resume" | "continue" | "unpause" => session_control(SessionOp::Play, &player).await,
            "toggle" | "play_pause" => session_control(SessionOp::Toggle, &player).await,
            "next" | "next_track" | "skip" | "next_song" => session_control(SessionOp::Next, &player).await,
            "previous" | "previous_track" | "prev_track" | "back" | "previous_song" => session_control(SessionOp::Previous, &player).await,
            // The player's own volume: its mixer channel (the master volume when it has none). A
            // level with a sign is a step: "-50" was once clamped to the level 0 and muted the PC.
            "set_volume" | "volume" => {
                let level = args.get("level").or_else(|| args.get("volume")).ok_or("set_volume needs 'level' (0-100, or a step such as \"+10\").")?;
                crate::audio_mixer::player_volume(level).await
            }
            "what_is_playing" | "now_playing" | "current" | "status" => match now_playing().await {
                Some(np) => Ok(format!("Now playing: {}.", np.describe())),
                None => Ok("Nothing is playing right now (no active media session).".into()),
            },
            "list_playlists" | "playlists" | "list" => {
                let names: Vec<String> = self.library().into_iter().filter(|i| matches!(i.kind, ItemKind::Playlist | ItemKind::LocalPlaylist)).map(|i| i.name).take(40).collect();
                if names.is_empty() {
                    Err("I can't see any playlists (Sonora's library cache is empty or missing).".into())
                } else {
                    let songs = indexed_tracks();
                    let local = songs.iter().filter(|t| !t.path.starts_with("spotify:")).count();
                    Ok(format!("Sir's playlists: {}. Track index: {} individual songs ({local} local files and playlist entries).", names.join(", "), songs.len()))
                }
            }
            "save_alias" | "remember_playlist" | "save_playlist" => {
                let alias = playlist.or(song).ok_or("save_alias needs 'playlist_name' (the alias).")?;
                let target = str_arg(args, &["uri", "link", "target"]).ok_or("save_alias needs 'uri' (a spotify: URI, open.spotify.com link or library name).")?;
                self.save_alias(alias, target)
            }
            other => Err(format!("Unknown music action '{other}' (play, pause, stop_all, resume, next_track, previous_track, set_volume, what_is_playing, list_playlists, save_alias).")),
        }
    }

    /// Resolves what Sir named and starts it.
    async fn play(&self, song: Option<&str>, playlist: Option<&str>, artist: Option<&str>, album: Option<&str>, player: &str) -> Result<String, String> {
        if let (Some(s), Some(p)) = (song, playlist) {
            // A song inside a named playlist: that playlist's own entry, never a search.
            if self.playlist_members(p).await.is_some_and(|(_, m)| !m.is_empty()) {
                return Box::pin(self.play_track_directly_from_playlist(s, p, artist, player)).await;
            }
        }
        let local_only = player.contains("local") || player.contains("file");
        let engine = self.clone();
        let items = tokio::task::spawn_blocking(move || engine.library()).await.map_err(|e| e.to_string())?;
        let items: Vec<_> = items.into_iter().filter(|item| !local_only || matches!(item.kind, ItemKind::LocalFile | ItemKind::LocalPlaylist)).collect();
        let context = playlist.and_then(|name| best_match(&items, name, &[ItemKind::Playlist, ItemKind::LocalPlaylist], None).map(|(i, _)| i.clone()));
        let members = context.as_ref().and_then(|item| self.inner.cache.lock().unwrap_or_else(|e| e.into_inner()).members.get(&compact(&item.name)).cloned()).unwrap_or_default();
        // A known playlist's members are authoritative; never label a global match as belonging
        // to that playlist. Missing remote membership is reported explicitly below.
        let wanted = if let Some(s) = song {
            best_match(if members.is_empty() { &items } else { &members }, s, &[ItemKind::Track, ItemKind::LocalFile], artist).map(|(i, _)| i.clone())
        } else if let Some(p) = playlist {
            // A name that is no playlist may still be an album, a saved song or an artist.
            best_match(&items, p, &[ItemKind::Playlist, ItemKind::LocalPlaylist], None)
                .or_else(|| best_match(&items, p, &[ItemKind::Album], None))
                .or_else(|| best_match(&items, p, &[ItemKind::Track, ItemKind::LocalFile], artist))
                .or_else(|| best_match(&items, p, &[ItemKind::Artist], None))
                .map(|(i, _)| i.clone())
        } else if let Some(a) = album {
            best_match(&items, a, &[ItemKind::Album], artist).map(|(i, _)| i.clone())
        } else {
            artist.and_then(|a| best_match(&items, a, &[ItemKind::Artist], None)).map(|(i, _)| i.clone())
        };
        if song.is_some() && !members.is_empty() && wanted.is_none() {
            return Err(format!("I couldn't find '{}' inside playlist '{}'.", song.unwrap_or_default(), playlist.unwrap_or_default()));
        }
        let item = match wanted {
            Some(i) => i,
            None => {
                let name = song.or(playlist).or(album).or(artist).unwrap_or("");
                match find_local(name, song.is_some()).await {
                    Some(local) => local,
                    // Not in the library: a Spotify search for it.
                    None if !local_only && !name.is_empty() => {
                        let query = [song, artist].into_iter().flatten().collect::<Vec<_>>().join(" ");
                        let query = if query.is_empty() { name.to_string() } else { query };
                        return search_and_play(&query, name, player).await;
                    }
                    None => return Err(format!("I couldn't find '{name}' in the library or the local music files.")),
                }
            }
        };
        let result = match (player, item.kind) {
            ("sonora", _) => play_sonora(&item, &members).await,
            (_, ItemKind::LocalFile | ItemKind::LocalPlaylist) => {
                crate::file_ops::open_with(Path::new(&item.uri), None).await.map(|_| format!("Opened the local {} '{}' in the default player; playback is not verified ({}).", item.kind.label(), item.name, item.uri))
            }
            _ => self.play_spotify(&item).await,
        }?;
        info!("[MUSIC] {result}");
        crate::dynamic_popups::now_playing(&item.name, &item.artist, context.as_ref().map(|c| c.name.as_str()), &player_name(player));
        if matches!(item.kind, ItemKind::Playlist | ItemKind::LocalPlaylist) {
            self.set_preference("last_playlist", &item.name);
        }
        *self.inner.last_played.lock().unwrap_or_else(|e| e.into_inner()) = Some(context.clone().filter(|_| item.kind == ItemKind::Track).unwrap_or_else(|| item.clone()));
        Ok(match context {
            Some(c) if matches!(item.kind, ItemKind::Track | ItemKind::LocalFile) && !members.is_empty() => format!("{result} From the indexed playlist '{}'.", c.name),
            Some(c) if matches!(item.kind, ItemKind::Track | ItemKind::LocalFile) => format!("{result} Playlist '{}' was found, but its track membership is not cached; I matched the song globally.", c.name),
            _ => result,
        })
    }

    fn preference(&self, key: &str) -> Option<String> {
        let conn = self.inner.conn.as_ref()?.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT value FROM music_preferences WHERE key=?1", [key], |r| r.get::<_, String>(0)).ok()
    }

    fn set_preference(&self, key: &str, value: &str) {
        if let Some(conn) = &self.inner.conn {
            let _ = conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
                "INSERT INTO music_preferences(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            );
        }
    }

    /// "Pick some good music": Jarvis decides — one of Sir's own playlists, the one he played
    /// last or whichever suits the time of day — and starts it.
    async fn pick_music(&self, player: &str) -> Result<String, String> {
        let engine = self.clone();
        let items = tokio::task::spawn_blocking(move || engine.library()).await.map_err(|e| e.to_string())?;
        let last = self.preference("last_playlist");
        let choice = choose_playlist(&items, last.as_deref(), chrono::Local::now())
            .ok_or("I can't see any of your playlists (Sonora's library cache is empty or missing).")?;
        info!("[MUSIC] Picking '{choice}' for Sir");
        let result = self.play(None, Some(&choice), None, None, player).await?;
        Ok(format!("I picked your playlist '{choice}'. {result}"))
    }

    /// The playlist Sir named and its ordered tracks (empty when only its name is known).
    async fn playlist_members(&self, playlist: &str) -> Option<(LibraryItem, Vec<LibraryItem>)> {
        let engine = self.clone();
        let items = tokio::task::spawn_blocking(move || engine.library()).await.ok()?;
        let list = best_match(&items, playlist, &[ItemKind::Playlist, ItemKind::LocalPlaylist], None).map(|(i, _)| i.clone())?;
        let members = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner()).members.get(&compact(&list.name)).cloned().unwrap_or_default();
        Some((list, members))
    }

    /// "Play Highway to Hell from my playlist JaysRock (without search)": the exact entry of
    /// that playlist. Sonora gets the track link through its hand-off, else the playlist itself
    /// skipped forward to the entry; a local playlist becomes a queue starting at the entry.
    /// Sonora's search bar is never touched.
    pub async fn play_track_directly_from_playlist(&self, song: &str, playlist: &str, artist: Option<&str>, player: &str) -> Result<String, String> {
        let (list, members) = self.playlist_members(playlist).await.ok_or_else(|| format!("I can't find a playlist called '{playlist}'."))?;
        if members.is_empty() {
            return Err(format!("I know the playlist '{}', but its track list isn't on this PC — download it in Sonora and I can play straight from it.", list.name));
        }
        let (index, track) = playlist_position(&members, song, artist).map(|(i, t)| (i, t.clone())).ok_or_else(|| format!("I couldn't find '{song}' inside playlist '{}'.", list.name))?;
        info!("[MUSIC] '{song}' is #{} of {} in '{}' → {}", index + 1, members.len(), list.name, track.uri);
        let result = if track.kind == ItemKind::LocalFile {
            // A queue from the entry on, in playlist order.
            let queue: Vec<String> = members[index..].iter().filter(|m| m.kind == ItemKind::LocalFile).take(1000).map(|m| m.uri.clone()).collect();
            if player == "sonora" {
                launch_sonora(queue).await?;
                match sonora_confirms(&track, &[], None, 10).await {
                    Some(now) => sonora_played(&track, "", &now),
                    None => format!("Handed '{}' and the rest of '{}' to Sonora; playback is not confirmed yet.", track.name, list.name),
                }
            } else {
                crate::file_ops::open_with(Path::new(&track.uri), None).await.map(|_| format!("Opened '{}' in the default player.", track.name))?
            }
        } else if player == "sonora" {
            play_sonora_in_playlist(&track, &list, index, &members).await?
        } else {
            self.play_spotify(&track).await?
        };
        *self.inner.last_played.lock().unwrap_or_else(|e| e.into_inner()) = Some(list.clone());
        self.set_preference("last_playlist", &list.name);
        crate::dynamic_popups::now_playing(&track.name, &track.artist, Some(&list.name), &player_name(player));
        Ok(format!("{result} Track {} of {} in your playlist '{}', played directly (no search).", index + 1, members.len(), list.name))
    }

    /// Opens a `spotify:` URI in the Spotify app and makes sure it actually plays.
    async fn play_spotify(&self, item: &LibraryItem) -> Result<String, String> {
        ensure_spotify_ready().await?;
        let before = session_of("spotify").await;
        let what = if item.artist.is_empty() || item.kind == ItemKind::Playlist {
            format!("the {} '{}'", item.kind.label(), item.name)
        } else {
            format!("'{}' by {}", item.name, item.artist)
        };
        // Tracks and albums start on their own with ":play"; playlists only navigate.
        let uri = match item.kind {
            ItemKind::Track | ItemKind::Album => format!("{}:play", item.uri),
            _ => item.uri.clone(),
        };
        open_uri(&uri).await?;
        // Playing *this*: the song itself, or (a list) a different song than before — resuming
        // whatever Spotify held last is not success.
        let started = |np: &Option<NowPlaying>| -> bool {
            let Some(np) = np else { return false };
            if !np.playing {
                return false;
            }
            match item.kind {
                ItemKind::Track => name_score(&item.name, &np.title) >= 0.8,
                _ => before.as_ref().is_none_or(|b| b.title != np.title),
            }
        };
        // Spotify may have to start first.
        let mut nudged = false;
        for step in 0..24 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = session_of("spotify").await;
            if started(&now) {
                let np = now.unwrap_or_default();
                return Ok(format!("Playing {what} on Spotify — now: {}.", np.describe()));
            }
            // Navigated but not playing: press play in Spotify once the page had time to load.
            if step == 7 && !nudged {
                nudged = true;
                nudge_spotify_play(item.kind == ItemKind::Playlist).await;
            }
        }
        let now = session_of("spotify").await;
        match now {
            Some(np) if started(&Some(np.clone())) => Ok(format!("Playing {what} on Spotify — now: {}.", np.describe())),
            _ => Err(format!("I opened {what} in Spotify, but it did not start playing; Spotify may need a click on Play.")),
        }
    }
}

/// The playlist Jarvis picks when Sir leaves the choice to him: his own playlists first (not
/// Spotify's mixes), the last one he played, a name that suits the hour, and his taste for rock;
/// the day of the year varies the pick among equals.
fn choose_playlist(items: &[LibraryItem], last: Option<&str>, now: chrono::DateTime<chrono::Local>) -> Option<String> {
    use chrono::{Datelike, Timelike};
    let lists: Vec<&LibraryItem> = items.iter().filter(|i| matches!(i.kind, ItemKind::Playlist | ItemKind::LocalPlaylist)).collect();
    let own: Vec<&LibraryItem> = lists.iter().copied().filter(|i| !i.artist.eq_ignore_ascii_case("spotify")).collect();
    let pool = if own.is_empty() { lists } else { own };
    let weekday = now.weekday().num_days_from_monday() < 5;
    let mood: &[&str] = match now.hour() {
        5..=11 => &["morning", "chill", "focus", "acoustic"],
        12..=17 if weekday => &["focus", "study", "lofi", "concentration"],
        12..=17 => &["rock", "hits", "mix", "party"],
        18..=23 => &["rock", "rap", "gaming", "party", "hits"],
        _ => &["chill", "sleep", "lofi", "calm"],
    };
    pool.into_iter()
        .map(|item| {
            let name = item.name.to_lowercase();
            let mut score = 0.0;
            if last.is_some_and(|l| name_score(l, &item.name) >= 0.95) {
                score += 2.5;
            }
            if mood.iter().any(|m| name.contains(m)) {
                score += 1.5;
            }
            if name.contains("rock") {
                score += 0.5;
            }
            score += f64::from((compact(&item.name).bytes().map(u32::from).sum::<u32>() + now.ordinal()) % 7) * 0.01;
            (item, score)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(item, _)| item.name.clone())
}

/// Launch arguments are passed as discrete OS arguments, never through a command shell.
/// Sonora's single-instance handler accepts provider links and local files (upstream
/// crates/sonora/src/main.rs, `follow` / `single::claim`).
async fn launch_sonora(arguments: Vec<String>) -> Result<(), String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.into());
    }
    let executable = sonora_binary().ok_or("Sonora.exe was not found in D:\\Program Files\\Sonora or the user installation paths.")?;
    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(executable);
        command.args(arguments).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
        let mut child = command.spawn().map_err(|e| format!("Could not launch Sonora: {e}"))?;
        // A new process may be the long-lived player or a short hand-off. Reap either safely.
        let _ = std::thread::Builder::new().name("sonora-child".into()).spawn(move || { let _ = child.wait(); });
        Ok(())
    }).await.map_err(|e| e.to_string())?
}

/// Waits up to `polls` × 350 ms for Sonora's media session to report `item`.
async fn sonora_confirms(item: &LibraryItem, members: &[LibraryItem], before: Option<&NowPlaying>, polls: usize) -> Option<NowPlaying> {
    for _ in 0..polls {
        tokio::time::sleep(Duration::from_millis(350)).await;
        if let Some(now) = session_of("sonora").await.filter(|now| playback_matches(item, members, before, now)) {
            return Some(now);
        }
    }
    None
}

fn sonora_played(item: &LibraryItem, how: &str, now: &NowPlaying) -> String {
    format!("Requested {} '{}' on Sonora{how}. Now playing: {}.", item.kind.label(), item.name, now.describe())
}

/// Plays `item` in Sonora and verifies it through Sonora's media session. A specific song is
/// tried three ways, each verified before the next:
/// 1. the exact file path or track link through Sonora's single-instance hand-off;
/// 2. its album (which Sonora starts playing), skipped forward to the song's track number;
/// 3. Sonora's own search: focus its window, open the search, type title and artist, start the
///    first result.
async fn play_sonora(item: &LibraryItem, members: &[LibraryItem]) -> Result<String, String> {
    let before = session_of("sonora").await;
    let arguments = if item.kind == ItemKind::LocalPlaylist && !members.is_empty() {
        members.iter().filter(|i| i.kind == ItemKind::LocalFile).take(1000).map(|i| i.uri.clone()).collect()
    } else { vec![item.uri.clone()] };
    launch_sonora(arguments).await?;
    let song = matches!(item.kind, ItemKind::Track);
    if !song {
        for attempt in 0..24 {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Some(now) = session_of("sonora").await.filter(|now| playback_matches(item, members, before.as_ref(), now)) {
                return Ok(sonora_played(item, "", &now));
            }
            if attempt == 7 && !matches!(item.kind, ItemKind::LocalFile | ItemKind::LocalPlaylist) {
                activate_player_selection("sonora", None, true).await?;
            }
        }
        return Err(format!("Sonora opened '{}' but did not report matching playback. Sign in to its music provider or select the matching result in Sonora.", item.name));
    }
    // 1. The track link itself.
    if let Some(now) = sonora_confirms(item, members, before.as_ref(), 7).await {
        return Ok(sonora_played(item, "", &now));
    }
    // 2. Its album, skipped to its number (Sonora plays albums handed to it).
    if let Some((album, 1, number @ 1..=30)) = album_position(&item.uri) {
        info!("[MUSIC] '{}' via its album, track {number}", item.name);
        let previous = session_of("sonora").await;
        launch_sonora(vec![album]).await?;
        let album_started = async {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(350)).await;
                if let Some(now) = session_of("sonora").await.filter(|n| n.playing && previous.as_ref().is_none_or(|p| p.title != n.title || !p.playing)) {
                    return Some(now);
                }
            }
            None
        };
        if let Some(now) = album_started.await {
            if playback_matches(item, members, None, &now) {
                return Ok(sonora_played(item, " (from its album)", &now));
            }
            for _ in 1..number {
                let _ = control_session(Some("sonora".into()), SessionOp::Next).await;
                tokio::time::sleep(Duration::from_millis(450)).await;
            }
            if let Some(now) = sonora_confirms(item, members, None, 8).await {
                return Ok(sonora_played(item, " (from its album)", &now));
            }
        }
    }
    // 3. Sonora's search, typed into its own window.
    let query = if item.artist.is_empty() { item.name.clone() } else { format!("{} {}", item.name, item.artist.split(',').next().unwrap_or("").trim()) };
    for first_result in [true, false] {
        activate_player_selection("sonora", Some(&query), first_result).await?;
        if let Some(now) = sonora_confirms(item, members, None, 10).await {
            return Ok(sonora_played(item, " (through its search)", &now));
        }
    }
    Err(format!("Sonora opened '{}' but did not report matching playback. Sign in to its music provider or select the matching result in Sonora.", item.name))
}

/// Skips Sonora takes at most to reach a playlist entry (each is verified afterwards).
const MAX_PLAYLIST_SKIPS: usize = 80;

/// A playlist entry in Sonora without its search: 1. the track link itself; 2. the playlist
/// link, then Next until the entry plays (from wherever the playlist started).
async fn play_sonora_in_playlist(track: &LibraryItem, list: &LibraryItem, index: usize, members: &[LibraryItem]) -> Result<String, String> {
    let before = session_of("sonora").await;
    launch_sonora(vec![track.uri.clone()]).await?;
    if let Some(now) = sonora_confirms(track, &[], before.as_ref(), 8).await {
        return Ok(sonora_played(track, "", &now));
    }
    if list.uri.starts_with("spotify:playlist:") {
        info!("[MUSIC] '{}' via its playlist '{}', entry {}", track.name, list.name, index + 1);
        let previous = session_of("sonora").await;
        launch_sonora(vec![list.uri.clone()]).await?;
        let mut started = None;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(350)).await;
            if let Some(now) = session_of("sonora").await.filter(|n| n.playing && previous.as_ref().is_none_or(|p| p.title != n.title || !p.playing)) {
                started = Some(now);
                break;
            }
        }
        if let Some(now) = started {
            if playback_matches(track, &[], None, &now) {
                return Ok(sonora_played(track, " (from its playlist)", &now));
            }
            let at = members.iter().position(|m| name_score(&m.name, &now.title) >= 0.85).unwrap_or(0);
            if index > at && index - at <= MAX_PLAYLIST_SKIPS {
                for _ in at..index {
                    let _ = control_session(Some("sonora".into()), SessionOp::Next).await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                if let Some(now) = sonora_confirms(track, &[], None, 10).await {
                    return Ok(sonora_played(track, " (from its playlist)", &now));
                }
            }
        }
    }
    Err(format!("Sonora didn't confirm '{}' from '{}', Sir. I did not fall back to its search, as you asked.", track.name, list.name))
}

fn playback_matches(item: &LibraryItem, members: &[LibraryItem], before: Option<&NowPlaying>, now: &NowPlaying) -> bool {
    if !now.playing || now.title.is_empty() { return false; }
    match item.kind {
        ItemKind::Track | ItemKind::LocalFile => name_score(&item.name, &now.title) >= 0.85
            && (item.artist.is_empty() || now.artist.is_empty() || name_score(&item.artist, &now.artist) >= 0.8),
        ItemKind::Album => name_score(&item.name, &now.album) >= 0.85,
        ItemKind::Artist => name_score(&item.name, &now.artist) >= 0.85,
        _ if !members.is_empty() => members.iter().any(|i| name_score(&i.name, &now.title) >= 0.85),
        _ => before.is_some_and(|previous| previous.title != now.title),
    }
}

async fn search_and_play(query: &str, expected_title: &str, player: &str) -> Result<String, String> {
    let confirmed = move || async move {
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Some(now) = session_of(player).await.filter(|s| s.playing && name_score(expected_title, &s.title) >= 0.85) {
                return Some(now);
            }
        }
        None
    };
    if player == "sonora" {
        launch_sonora(Vec::new()).await?;
        // The first result row, then (some result lists need it) Enter on the search itself.
        for first_result in [true, false] {
            activate_player_selection("sonora", Some(query), first_result).await?;
            if let Some(now) = confirmed().await {
                return Ok(format!("Playing '{}' on {} — {}.", now.title, player_name(player), now.describe()));
            }
        }
    } else {
        ensure_spotify_ready().await?;
        open_uri(&format!("spotify:search:{}", urlencoding::encode(query))).await?;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        activate_player_selection("spotify", None, false).await?;
        if let Some(now) = confirmed().await {
            return Ok(format!("Playing '{}' on {} — {}.", now.title, player_name(player), now.describe()));
        }
    }
    Err(format!("I searched {} for '{query}', but matching playback was not confirmed. The player may need sign-in or a selection from the search results.", player_name(player)))
}

/// Focuses the player's window and starts a selection there. With `search`: Sonora's global
/// search (Ctrl+Shift+F), the query typed as Unicode, then the first result (`first_result`:
/// Down + Enter) or the search's own default action (Enter, then Down + Enter). Every key is
/// sent only while the player is still the foreground window.
#[cfg(windows)]
async fn activate_player_selection(player: &str, search: Option<&str>, first_result: bool) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.into());
    }
    let player = player.to_string();
    let search = search.map(str::to_string);
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    let hwnd = loop {
        let name = player.clone();
        let window = crate::win32::call(move || crate::win32::app_windows().into_iter().find(|w| w.process.eq_ignore_ascii_case(&name) && !w.title.trim().is_empty())).await?;
        if let Some(window) = window { break window.hwnd; }
        if std::time::Instant::now() >= deadline { return Err(format!("{} has no controllable window.", player_name(&player))); }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    crate::win32::call(move || -> Result<(), String> {
        crate::win32::focus(hwnd);
        std::thread::sleep(Duration::from_millis(200));
        let focused = || if crate::win32::is_foreground(hwnd) { Ok(()) } else { Err("Music automation stopped because another window took focus.".to_string()) };
        focused()?;
        if let Some(query) = search {
            // Sonora upstream input::bindings: Ctrl+Shift+F opens global search; Down selects
            // the first result and Enter activates it. Unicode input preserves the clipboard.
            crate::win32::send_chord(&[VK_CONTROL, VK_SHIFT], VK_F)?;
            std::thread::sleep(Duration::from_millis(200));
            focused()?;
            crate::win32::send_chord(&[VK_CONTROL], VK_A)?;
            for unit in query.encode_utf16().take(240) {
                focused()?;
                let inputs = [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP].map(|flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, ..Default::default() } },
                });
                // SAFETY: initialized keyboard INPUT records remain valid for this call.
                if unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) } != inputs.len() as u32 { return Err("Windows blocked music search input.".into()); }
            }
            std::thread::sleep(Duration::from_millis(1500));
            focused()?;
            if !first_result {
                crate::win32::send_chord(&[], VK_RETURN)?;
                std::thread::sleep(Duration::from_millis(900));
                focused()?;
            }
            crate::win32::send_chord(&[], VK_DOWN)?;
        } else if player == "sonora" && first_result {
            crate::win32::send_chord(&[], VK_DOWN)?;
        }
        focused()?;
        crate::win32::send_chord(&[], VK_RETURN)
    }).await?
}

#[cfg(not(windows))]
async fn activate_player_selection(player: &str, _search: Option<&str>, _first_result: bool) -> Result<(), String> {
    Err(format!("Interactive search automation for {player} is available on Windows; select the requested music in the player."))
}

/// Spotify running with its main window up ("Spotify Premium" / "Spotify Free" or a song):
/// launched from its executable and waited for when it is not, since a link handed over while it
/// is still starting makes it quit.
#[cfg(windows)]
async fn ensure_spotify_ready() -> Result<(), String> {
    let ready = || crate::win32::call(|| crate::win32::app_windows().into_iter().any(|w| w.process == "spotify" && !w.title.trim().is_empty()));
    if ready().await? {
        return Ok(());
    }
    info!("[MUSIC] Starting Spotify");
    crate::apps::open("spotify", &[]).await.map_err(|e| format!("Spotify could not be started ({e}). Is the Spotify app installed?"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(700)).await;
        if ready().await? {
            // Its player is not ready the moment the window is.
            tokio::time::sleep(Duration::from_secs(3)).await;
            return Ok(());
        }
    }
    Err("Spotify did not finish starting in time.".into())
}

#[cfg(not(windows))]
async fn ensure_spotify_ready() -> Result<(), String> {
    Ok(())
}

/// `spotify:playlist:…` from a URI or an open.spotify.com link.
fn spotify_uri(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with("spotify:") {
        return Some(s.to_string());
    }
    let rest = s.split("open.spotify.com/").nth(1)?;
    let mut parts = rest.split(['/', '?']).filter(|p| !p.is_empty() && !p.starts_with("intl-"));
    let kind = parts.next()?;
    let id = parts.next()?;
    matches!(kind, "playlist" | "track" | "album" | "artist").then(|| format!("spotify:{kind}:{id}"))
}

/// Local playlists (.m3u/.m3u8) or audio files from the file index.
async fn find_local(name: &str, song: bool) -> Option<LibraryItem> {
    if name.trim().len() < 2 {
        return None;
    }
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        let exts: &[&str] = if song { &["mp3", "flac", "wav", "m4a", "ogg", "opus", "m3u", "m3u8"] } else { &["m3u", "m3u8", "pls", "mp3", "flac", "wav", "m4a"] };
        for ext in exts {
            let hits = crate::file_ops::find(&name, Some(ext), None, 3);
            if let Some(hit) = hits.into_iter().find(|h| !h.is_dir && name_score(&name, &h.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()) >= 0.8) {
                let kind = if matches!(*ext, "m3u" | "m3u8" | "pls") { ItemKind::LocalPlaylist } else { ItemKind::LocalFile };
                let stem = hit.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                return Some(LibraryItem { kind, name: stem, uri: hit.path.display().to_string(), artist: String::new(), source: "local files" });
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// What a test gets instead of touching Sir's players.
const HANDS_OFF: &str = "left alone: a test never touches Sir's players";

#[cfg(windows)]
async fn open_uri(uri: &str) -> Result<(), String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.into());
    }
    let uri = uri.to_string();
    crate::win32::call(move || crate::win32::shell_execute(&uri, None, None)).await?.map_err(|e| format!("The music link could not be opened ({e})."))
}

#[cfg(not(windows))]
async fn open_uri(uri: &str) -> Result<(), String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.into());
    }
    let uri = uri.to_string();
    tokio::task::spawn_blocking(move || std::process::Command::new("xdg-open").arg(&uri).spawn().map(|_| ()).map_err(|e| e.to_string())).await.map_err(|e| e.to_string())?
}

/// Spotify opened the page but is not playing it: focus it and press Enter on the page. (Never
/// Space or the play key: those resume whatever Spotify held before, not what Sir asked for.)
#[cfg(windows)]
async fn nudge_spotify_play(_playlist: bool) {
    if crate::platform::hands_off() {
        return;
    }
    let _ = crate::win32::call(move || {
        let Some(w) = crate::win32::app_windows().into_iter().find(|w| w.process == "spotify" && !w.title.trim().is_empty()) else { return };
        crate::win32::focus(w.hwnd);
        std::thread::sleep(Duration::from_millis(250));
        if crate::win32::is_foreground(w.hwnd) {
            let _ = crate::win32::send_chord(&[], windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN);
        }
    })
    .await;
}

#[cfg(not(windows))]
async fn nudge_spotify_play(_playlist: bool) {}

// ------------------------------------------------------------------ stop everything ----

/// What "stop" stopped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaStop {
    /// Players paused through their Windows media session (Sonora, Spotify, YouTube in a
    /// browser, VLC …).
    pub sessions: Vec<String>,
    /// Players that were making sound and got a direct pause command to their window.
    pub windows: Vec<String>,
}

impl MediaStop {
    pub fn describe(&self) -> String {
        let mut names: Vec<String> = Vec::new();
        for name in self.sessions.iter().chain(&self.windows) {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                names.push(name.clone());
            }
        }
        match names.len() {
            0 => "Nothing else was playing; Jarvis is quiet.".into(),
            1 => format!("Stopped {}.", names[0]),
            _ => format!("Stopped {} and {}.", names[..names.len() - 1].join(", "), names[names.len() - 1]),
        }
    }
}

/// How long one stop result is shared: the dispatcher's instant stop and the brain's silent
/// command for the same "stop" arrive milliseconds apart; a new "stop" a second later acts again.
const STOP_BURST: Duration = Duration::from_millis(700);

/// The last stop (when it finished) and what it stopped.
static LAST_STOP: tokio::sync::Mutex<Option<(std::time::Instant, MediaStop)>> = tokio::sync::Mutex::const_new(None);

/// "Stop" / "pause": everything that is playing stops at once, silently.
/// 1. Every Windows media session that is playing gets an explicit Pause — never play/pause, so
///    a paused player is never started again: Sonora, Spotify, any browser video, VLC, Apple
///    Music … (one cached session manager, all sessions paused in parallel).
/// 2. Media players that are still making sound (Sonora, Spotify, browsers, players without a
///    media session) get `WM_APPCOMMAND` / `APPCOMMAND_MEDIA_PAUSE`, a pause-only command, on
///    their windows.
///
/// Jarvis' own voice is stopped by the dispatcher (`TtsController::stop`).
pub async fn stop_all_active_media() -> MediaStop {
    if crate::platform::hands_off() {
        info!("[MUSIC] Stop everything: Sir's players left alone (a test never pauses them)");
        return MediaStop::default();
    }
    let mut last = LAST_STOP.lock().await;
    if let Some((_, report)) = last.as_ref().filter(|(at, _)| at.elapsed() < STOP_BURST) {
        return report.clone();
    }
    let started = std::time::Instant::now();
    let report = stop_everything().await;
    info!("[MUSIC] Stop everything in {} ms: {}", started.elapsed().as_millis(), report.describe());
    *last = Some((std::time::Instant::now(), report.clone()));
    report
}

/// Processes whose sound "stop" should end: music and video players and browsers (never games,
/// voice chats or system sounds).
fn is_media_player(process: &str) -> bool {
    let p = process.to_lowercase();
    let p = p.trim_end_matches(".exe");
    [
        "sonora", "spotify", "vlc", "wmplayer", "foobar2000", "aimp", "musicbee", "winamp", "itunes", "applemusic", "amazon music", "tidal", "deezer",
        "potplayermini64", "potplayermini", "potplayer", "mpc-hc64", "mpc-hc", "mpc-be64", "mpc-be", "mpv", "music.ui", "video.ui", "microsoft.media.player",
        "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "arc",
    ].contains(&p)
}

#[cfg(windows)]
async fn stop_everything() -> MediaStop {
    const APPCOMMAND_MEDIA_PAUSE: u32 = 47;
    let (paused, audible) = tokio::join!(tokio::task::spawn_blocking(smtc::pause_all), crate::audio_mixer::audible_apps());
    let sessions: Vec<String> = paused.unwrap_or_default().iter().map(|app| player_name(app)).collect();
    let own = std::process::id();
    let targets: Vec<(String, u32)> = audible.into_iter().filter(|(name, pid)| *pid != own && is_media_player(name)).collect();
    let windows = if targets.is_empty() {
        Vec::new()
    } else {
        crate::win32::call(move || {
            targets.into_iter().filter(|(_, pid)| crate::win32::post_app_command(*pid, APPCOMMAND_MEDIA_PAUSE) > 0).map(|(name, _)| player_name(&name)).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default()
    };
    MediaStop { sessions, windows }
}

#[cfg(not(windows))]
async fn stop_everything() -> MediaStop {
    let paused = tokio::task::spawn_blocking(|| std::process::Command::new("playerctl").args(["-a", "pause"]).status().map(|s| s.success()).unwrap_or(false)).await.unwrap_or(false);
    MediaStop { sessions: if paused { vec!["every player".into()] } else { Vec::new() }, windows: Vec::new() }
}

// ----------------------------------------------------------------- the media session ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOp {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}

/// Pause / resume / skip on the player Sir means (or the active one), media keys as fallback.
async fn session_control(op: SessionOp, player: &str) -> Result<String, String> {
    let hint = match player {
        p if p.contains("spotify") => Some("spotify".to_string()),
        p if p.contains("sonora") => Some("sonora".to_string()),
        _ => None,
    };
    match control_session(hint, op).await {
        Ok(Some(np)) => Ok(match op {
            SessionOp::Pause => format!("Paused {} on {}.", np.title, player_name(&np.app)),
            SessionOp::Play => format!("Resumed {} on {}.", np.title, player_name(&np.app)),
            SessionOp::Toggle => format!("Toggled playback of {} on {}.", np.title, player_name(&np.app)),
            SessionOp::Next => format!("Skipped to the next track on {}.", player_name(&np.app)),
            SessionOp::Previous => format!("Went back a track on {}.", player_name(&np.app)),
        }),
        Ok(None) => Err(format!("{} has no controllable media session. Start a track in that player first.", player_name(player))),
        Err(error) => Err(format!("{} media control failed: {error}", player_name(player))),
    }
}

/// What is playing now (the system's current media session).
pub async fn now_playing() -> Option<NowPlaying> {
    sessions().await.into_iter().max_by_key(|s| (s.playing, s.app.to_lowercase().contains("spotify") || s.app.to_lowercase().contains("sonora")))
}

/// The media session of one player.
async fn session_of(app: &str) -> Option<NowPlaying> {
    let app = app.to_lowercase();
    sessions().await.into_iter().find(|s| s.app.to_lowercase().contains(&app))
}

/// The system media sessions, served by one thread that owns the session manager: asking
/// Windows for the manager costs tens of milliseconds, so it is done once and kept (and asked
/// again only if it stops answering).
#[cfg(windows)]
mod smtc {
    use super::{NowPlaying, SessionOp};
    use std::sync::{mpsc, OnceLock};
    use std::time::Duration;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };

    type Job = Box<dyn FnOnce(&mut Option<Manager>) + Send>;

    fn worker() -> Option<&'static mpsc::Sender<Job>> {
        static TX: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();
        TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("jarvis-smtc".into())
                .spawn(move || {
                    // SAFETY: called once at the start of this thread, before any WinRT use.
                    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
                    let mut manager: Option<Manager> = None;
                    for job in rx {
                        if manager.is_none() {
                            manager = Manager::RequestAsync().and_then(|op| op.get()).ok();
                        }
                        job(&mut manager);
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
    }

    /// Runs `f` on the session thread and waits for its answer.
    fn run<R: Send + 'static>(f: impl FnOnce(&mut Option<Manager>) -> R + Send + 'static) -> Option<R> {
        let (tx, rx) = mpsc::channel();
        worker()?
            .send(Box::new(move |manager: &mut Option<Manager>| {
                let _ = tx.send(f(manager));
            }))
            .ok()?;
        rx.recv_timeout(Duration::from_secs(8)).ok()
    }

    /// The current sessions; a manager that fails is dropped and asked for again next time.
    fn list(manager: &mut Option<Manager>) -> Vec<Session> {
        let Some(m) = manager.as_ref() else { return Vec::new() };
        match m.GetSessions() {
            Ok(list) => list.into_iter().collect(),
            Err(_) => {
                *manager = None;
                Vec::new()
            }
        }
    }

    /// Asks for the manager now (in the background), so the first "stop" is instant.
    pub fn warm() {
        let _ = std::thread::Builder::new().name("jarvis-smtc-warm".into()).spawn(|| {
            let _ = run(|manager| manager.is_some());
        });
    }

    /// Pauses every session that is playing — all requests first, then the answers — and
    /// returns their app ids.
    pub fn pause_all() -> Vec<String> {
        run(|manager| {
            let playing: Vec<(String, Session)> = list(manager)
                .into_iter()
                .filter(|s| s.GetPlaybackInfo().ok().and_then(|i| i.PlaybackStatus().ok()) == Some(Status::Playing))
                .map(|s| (s.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default(), s))
                .collect();
            let requests: Vec<_> = playing.iter().map(|(app, s)| (app.clone(), s.TryPauseAsync())).collect();
            requests.into_iter().filter_map(|(app, op)| op.and_then(|o| o.get()).ok().filter(|done| *done).map(|_| app)).collect()
        })
        .unwrap_or_default()
    }

    fn describe(s: &Session) -> Option<NowPlaying> {
        let app = s.SourceAppUserModelId().ok()?.to_string();
        let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok();
        let playing = s.GetPlaybackInfo().ok().and_then(|i| i.PlaybackStatus().ok()) == Some(Status::Playing);
        Some(NowPlaying {
            app,
            title: props.as_ref().and_then(|p| p.Title().ok()).map(|t| t.to_string()).unwrap_or_default(),
            artist: props.as_ref().and_then(|p| p.Artist().ok()).map(|t| t.to_string()).unwrap_or_default(),
            album: props.as_ref().and_then(|p| p.AlbumTitle().ok()).map(|t| t.to_string()).unwrap_or_default(),
            playing,
        })
    }

    fn all(manager: &mut Option<Manager>) -> Vec<(Session, NowPlaying)> {
        list(manager).into_iter().filter_map(|s| describe(&s).map(|d| (s, d))).collect()
    }

    pub fn sessions() -> Vec<NowPlaying> {
        run(|manager| all(manager).into_iter().map(|(_, d)| d).collect()).unwrap_or_default()
    }

    /// Runs `op` on the session of `app` (or the playing / current one). None = no session.
    pub fn control(app: Option<String>, op: SessionOp) -> Option<NowPlaying> {
        run(move |manager| {
            let list = all(manager);
            let pick = match &app {
                Some(a) => list.iter().find(|(_, d)| d.app.to_lowercase().contains(a)),
                None => list.iter().find(|(_, d)| d.playing).or_else(|| list.first()),
            }?;
            let (session, info) = pick;
            let done = match op {
                SessionOp::Play => session.TryPlayAsync(),
                SessionOp::Pause => session.TryPauseAsync(),
                SessionOp::Toggle => session.TryTogglePlayPauseAsync(),
                SessionOp::Next => session.TrySkipNextAsync(),
                SessionOp::Previous => session.TrySkipPreviousAsync(),
            }
            .and_then(|op| op.get())
            .unwrap_or(false);
            done.then(|| info.clone())
        })
        .flatten()
    }
}

#[cfg(windows)]
async fn sessions() -> Vec<NowPlaying> {
    tokio::task::spawn_blocking(smtc::sessions).await.unwrap_or_default()
}

#[cfg(windows)]
async fn control_session(app: Option<String>, op: SessionOp) -> Result<Option<NowPlaying>, String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.into());
    }
    tokio::task::spawn_blocking(move || smtc::control(app, op)).await.map_err(|e| e.to_string())
}

#[cfg(not(windows))]
async fn sessions() -> Vec<NowPlaying> {
    tokio::task::spawn_blocking(|| {
        let out = std::process::Command::new("playerctl").args(["-a", "metadata", "--format", "{{playerName}}\t{{status}}\t{{title}}\t{{artist}}\t{{album}}"]).output().ok();
        out.map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    (f.len() >= 5).then(|| NowPlaying { app: f[0].into(), playing: f[1] == "Playing", title: f[2].into(), artist: f[3].into(), album: f[4].into() })
                })
                .collect()
        })
        .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

#[cfg(not(windows))]
async fn control_session(app: Option<String>, op: SessionOp) -> Result<Option<NowPlaying>, String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.into());
    }
    let cmd = match op {
        SessionOp::Play => "play",
        SessionOp::Pause => "pause",
        SessionOp::Toggle => "play-pause",
        SessionOp::Next => "next",
        SessionOp::Previous => "previous",
    };
    let mut args = Vec::new();
    if let Some(a) = &app {
        args.extend(["-p".to_string(), a.clone()]);
    }
    args.push(cmd.into());
    let ok = std::process::Command::new("playerctl").args(&args).status().map(|s| s.success()).unwrap_or(false);
    Ok(if ok { sessions().await.into_iter().next() } else { None })
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "control_music",
        "description": "Sir's music (Spotify / Sonora library, every indexed song on his drives). ALWAYS call it to play, pause or skip — never only say you did. play with playlist_name (\"play my playlist JaysRock\"), song_name for one specific song (\"play Highway to Hell\"; + playlist_name when he says \"in <playlist>\", + artist_name), artist_name or album_name; pick_music when he leaves the choice to you (\"pick some good music\"); stop_all for a bare \"stop\" / \"pause\" (pauses everything that plays, any app); pause (one player), resume, next_track, previous_track, set_volume (level 0-100: the MUSIC PLAYER's own volume only; the browser's, Discord's, any other app's and the PC's volume are control_audio_mixer), what_is_playing, list_playlists, save_alias (playlist_name = alias, uri).",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["play", "play_from_playlist", "pick_music", "pause", "stop_all", "resume", "next_track", "previous_track", "set_volume", "what_is_playing", "list_playlists", "save_alias", "set_preferred_player", "welcome_home"]},
            "song_name": {"type": "string"},
            "playlist_name": {"type": "string"},
            "artist_name": {"type": "string"},
            "album_name": {"type": "string"},
            "player_preference": {"type": "string", "enum": ["auto", "spotify", "sonora", "local"]},
            "level": {"type": "integer"},
            "uri": {"type": "string"}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `downloaded_sets` row as Sonora writes it (state.sqlite).
    const JAYS_ROCK: &str = r#"{"head":{"Playlist":{"id":"ExamplePlaylistId00000","name":"Jays Rock","owner":"JayKaey","track_count":3}},
        "tracks":[{"id":"08onVqQ8YicJ98Ycm1qoLf","name":"Urgent","artists":"Foreigner","album_id":"2Pw51hAGvWpTA3AYl2WVuu","track_number":3},
                  {"id":"2zYzyRzz6pRmhPzyfMEC8s","name":"Highway to Hell","artists":"AC/DC","album_id":"10v912xgTZbjAtYfyKWJCS","track_number":1},
                  {"id":"08mG3Y1vljYA6bvDt4Wqkj","name":"Back In Black","artists":"AC/DC"}]}"#;

    #[test]
    fn sonora_downloaded_playlists_keep_their_order() {
        let (name, tracks) = downloaded_playlist(JAYS_ROCK).unwrap();
        assert_eq!(name, "Jays Rock");
        assert_eq!(tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Urgent", "Highway to Hell", "Back In Black"]);
        assert_eq!(tracks[1].uri, "spotify:track:2zYzyRzz6pRmhPzyfMEC8s");
        assert_eq!(tracks[1].artist, "AC/DC");
        assert_eq!(album_position("spotify:track:2zYzyRzz6pRmhPzyfMEC8s"), Some(("spotify:album:10v912xgTZbjAtYfyKWJCS".into(), 1, 1)));
        assert!(downloaded_playlist(r#"{"head":{},"tracks":[]}"#).is_none());
    }

    #[test]
    fn the_library_is_read_again_only_when_its_files_change() {
        let dir = std::env::temp_dir().join(format!("jarvis-library-stamps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.sqlite");
        let wal = dir.join("state.sqlite-wal");
        std::fs::write(&db, b"one").unwrap();
        let sources = vec![(db.clone(), file_stamp(&db)), (wal.clone(), None)];
        assert!(!library_current(None, &sources), "never read: a scan");
        assert!(library_current(Some(Duration::from_secs(5)), &sources));
        assert!(library_current(Some(Duration::from_secs(5 * 60)), &sources), "nothing changed: no scan");
        assert!(!library_current(Some(Duration::from_secs(11 * 60)), &sources), "after ten minutes it is read again whatever happened");
        // Sonora writes: its database grows...
        std::fs::write(&db, b"one two").unwrap();
        assert!(!library_current(Some(Duration::from_secs(5 * 60)), &sources));
        assert!(library_current(Some(Duration::from_secs(10)), &sources), "within half a minute its files are not looked at");
        // ...or its write-ahead log appears.
        let sources = vec![(db.clone(), file_stamp(&db)), (wal.clone(), None)];
        assert!(library_current(Some(Duration::from_secs(60)), &sources));
        std::fs::write(&wal, b"w").unwrap();
        assert!(!library_current(Some(Duration::from_secs(60)), &sources));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn play_track_directly_from_playlist_finds_the_entry_without_search() {
        let (_, tracks) = downloaded_playlist(JAYS_ROCK).unwrap();
        // Whisper's spelling, a different case, the artist as a tie-breaker.
        let (index, track) = playlist_position(&tracks, "highway to hell", None).unwrap();
        assert_eq!((index, track.name.as_str()), (1, "Highway to Hell"));
        assert_eq!(playlist_position(&tracks, "Back in Black", Some("AC/DC")).map(|(i, _)| i), Some(2));
        assert!(playlist_position(&tracks, "Bohemian Rhapsody", None).is_none(), "only songs inside that playlist");
        // The state database is read straight from SQLite, in playlist order.
        let dir = std::env::temp_dir().join(format!("jarvis_sonora_state_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("state.sqlite");
        let _ = std::fs::remove_file(&db);
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch("CREATE TABLE downloaded_sets (provider TEXT, kind TEXT, set_id TEXT, value TEXT, added_at INTEGER);").unwrap();
            c.execute("INSERT INTO downloaded_sets VALUES ('spotify','playlist','ExamplePlaylistId00000',?1,0)", [JAYS_ROCK]).unwrap();
        }
        let (mut items, mut members) = (Vec::new(), HashMap::new());
        read_playlist_cache(&db, &mut items, &mut members);
        assert_eq!(members.get("jaysrock").map(Vec::len), Some(3));
        assert_eq!(members["jaysrock"][1].name, "Highway to Hell");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_preference_is_deterministic() {
        assert_eq!(choose_player("auto", true, true, true), Player::Sonora);
        assert_eq!(choose_player("auto", false, true, true), Player::Spotify);
        assert_eq!(choose_player("auto", false, false, true), Player::Sonora);
        assert_eq!(choose_player("auto", false, false, false), Player::Spotify);
        assert_eq!(choose_player("spotify", true, true, true), Player::Spotify);
        assert_eq!(choose_player("sonora", false, true, true), Player::Sonora);
    }

    #[test]
    fn mismatching_or_paused_media_never_confirms_playback() {
        let track = item(ItemKind::Track, "Highway to Hell", "AC/DC");
        let mut now = NowPlaying { app:"Sonora.exe".into(), title:"Back in Black".into(), artist:"AC/DC".into(), playing:true, ..Default::default() };
        assert!(!playback_matches(&track, &[], None, &now));
        now.title = "Highway to Hell".into();
        assert!(playback_matches(&track, &[], None, &now));
        now.playing = false;
        assert!(!playback_matches(&track, &[], None, &now));
    }

    #[test]
    fn local_playlist_tracks_and_json_membership_are_indexed() {
        let root = std::env::temp_dir().join(format!("jarvis-music-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Back in Black.mp3"), []).unwrap();
        let playlist = root.join("JaysRock.m3u8");
        std::fs::write(&playlist, "#EXTM3U\n#EXTINF:250,AC/DC - Back in Black\nBack in Black.mp3\n").unwrap();
        let tracks = playlist_tracks(&playlist);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].name, "Back in Black");
        assert_eq!(tracks[0].artist, "AC/DC");
        let mut items = Vec::new();
        let mut members = HashMap::new();
        json_library(&json!({"name":"Jay's Rock", "tracks":[{"name":"Back in Black", "path":"Back in Black.mp3"}]}), &root, &mut items, &mut members, 0);
        assert_eq!(members["jaysrock"].len(), 1);
        std::fs::remove_file(playlist).unwrap();
        std::fs::remove_file(root.join("Back in Black.mp3")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    fn item(kind: ItemKind, name: &str, artist: &str) -> LibraryItem {
        LibraryItem { kind, name: name.into(), uri: format!("spotify:x:{}", compact(name)), artist: artist.into(), source: "test" }
    }

    #[test]
    fn misheard_playlist_names_find_the_playlist() {
        let lib = vec![
            item(ItemKind::Playlist, "Jays Rock", "JayKaey"),
            item(ItemKind::Playlist, "Jay’s Rap", "JayKaey"),
            item(ItemKind::Playlist, "Jays Auto", "JayKaey"),
            item(ItemKind::Playlist, "Jays Focus", "JayKaey"),
            item(ItemKind::Playlist, "Classic Rock Mix", "Spotify"),
            item(ItemKind::Track, "Back In Black", "AC/DC"),
            item(ItemKind::Track, "Back in Black", "Back In Black"),
            item(ItemKind::Track, "Black Betty", "Ram Jam"),
        ];
        for spoken in ["JaysRock", "Jays Rock", "Jay's Rock", "jaysrock", "jayrock", "chase rock", "jays rock playlist"] {
            let (hit, _) = best_match(&lib, spoken, &[ItemKind::Playlist], None).unwrap_or_else(|| panic!("{spoken}"));
            assert_eq!(hit.name, "Jays Rock", "{spoken}");
        }
        assert_eq!(best_match(&lib, "jays rap", &[ItemKind::Playlist], None).unwrap().0.name, "Jay’s Rap");
        assert!(best_match(&lib, "dinner jazz", &[ItemKind::Playlist], None).is_none());
        let (song, _) = best_match(&lib, "back in black", &[ItemKind::Track], Some("ACDC")).unwrap();
        assert_eq!(song.artist, "AC/DC", "the artist breaks the tie");
        assert_eq!(phonetic("chase rock"), phonetic("jaysrock"));
    }

    #[test]
    fn jarvis_picks_one_of_sirs_own_playlists() {
        use chrono::TimeZone;
        let lib = vec![
            item(ItemKind::Playlist, "Classic Rock Mix", "Spotify"),
            item(ItemKind::Playlist, "Jays Rock", "JayKaey"),
            item(ItemKind::Playlist, "Jays Focus", "JayKaey"),
            item(ItemKind::Track, "Back in Black", "AC/DC"),
        ];
        let evening = chrono::Local.with_ymd_and_hms(2026, 10, 23, 20, 0, 0).unwrap();
        assert_eq!(choose_playlist(&lib, None, evening).as_deref(), Some("Jays Rock"));
        let study = chrono::Local.with_ymd_and_hms(2026, 10, 22, 14, 0, 0).unwrap();
        assert_eq!(choose_playlist(&lib, None, study).as_deref(), Some("Jays Focus"));
        assert_eq!(choose_playlist(&lib, Some("Jays Focus"), evening).as_deref(), Some("Jays Focus"), "the last one played wins");
        assert!(choose_playlist(&[], None, evening).is_none());
    }

    #[test]
    fn the_track_index_finds_tagged_songs_and_skips_system_folders() {
        assert!(skip_dir("Windows") && skip_dir(".git") && skip_dir("SteamLibrary") && skip_dir("$Recycle.Bin"));
        assert!(!skip_dir("Music") && !skip_dir("Downloads") && !skip_dir("AC-DC"));
        let root = std::env::temp_dir().join(format!("jarvis-index-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Rock")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        let song = vec![0u8; (MIN_SONG_BYTES + 10) as usize];
        std::fs::write(root.join("Rock/AC_DC - Highway to Hell.mp3"), &song).unwrap();
        std::fs::write(root.join("node_modules/Highway to Hell.mp3"), &song).unwrap();
        std::fs::write(root.join("Rock/click.wav"), [0u8; 100]).unwrap();
        let found = scan_audio_files(std::slice::from_ref(&root));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].title.as_str(), found[0].artist.as_str()), ("Highway to Hell", "AC DC"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn spotify_links_become_uris() {
        assert_eq!(spotify_uri("https://open.spotify.com/playlist/ExamplePlaylistId00000?si=abc").as_deref(), Some("spotify:playlist:ExamplePlaylistId00000"));
        assert_eq!(spotify_uri("https://open.spotify.com/intl-de/track/08mG3Y1vljYA6bvDt4Wqkj").as_deref(), Some("spotify:track:08mG3Y1vljYA6bvDt4Wqkj"));
        assert_eq!(spotify_uri("spotify:album:1").as_deref(), Some("spotify:album:1"));
        assert_eq!(spotify_uri("my gym list"), None);
    }

    #[test]
    #[ignore = "reads the real Sonora library"]
    fn reads_sonoras_library() {
        let lib = MusicEngine::in_memory().library();
        assert!(lib.iter().any(|i| i.kind == ItemKind::Playlist));
        println!("{} items", lib.len());
    }

    /// Nothing a test runs pauses, skips or starts what Sir is listening to.
    #[tokio::test]
    async fn tests_never_touch_sirs_players() {
        assert!(crate::platform::hands_off());
        assert_eq!(stop_all_active_media().await, MediaStop::default());
        for op in [SessionOp::Pause, SessionOp::Play, SessionOp::Toggle, SessionOp::Next, SessionOp::Previous] {
            assert!(session_control(op, "sonora").await.unwrap_err().contains(HANDS_OFF), "{op:?}");
        }
        assert_eq!(open_uri("spotify:search:example%20rock").await, Err(HANDS_OFF.to_string()));
        assert_eq!(launch_sonora(Vec::new()).await, Err(HANDS_OFF.to_string()));
    }
}
