//! The File Janitor: "clean up my Downloads." Classifies the loose files of a folder (images,
//! videos, documents, installers, archives, code, Minecraft, duplicates …), shows a preview card,
//! moves them into tidy sub-folders when Sir agrees, and writes a manifest so "undo the cleanup"
//! puts every file back.

use crate::features::{bool_arg, show_window, spoken, str_arg, user_folder};
use crate::tools::ToolContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Plan {
    pub folder: PathBuf,
    pub moves: Vec<Move>,
}

#[derive(Clone)]
pub struct Janitor {
    manifests: PathBuf,
    pending: Arc<Mutex<Option<Plan>>>,
}

/// The sub-folder a file belongs in.
pub fn category(path: &Path, age_days: u64) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "svg" | "tif" | "tiff" | "avif" | "ico" => "Images",
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "m4v" => "Videos",
        "mp3" | "wav" | "flac" | "m4a" | "ogg" | "aac" | "opus" => "Audio",
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "epub" => "Documents",
        "xls" | "xlsx" | "ods" | "csv" => "Spreadsheets",
        "ppt" | "pptx" | "odp" | "key" => "Presentations",
        "exe" | "msi" | "msix" | "appx" | "msixbundle" if age_days > 30 => "Installers\\Old",
        "exe" | "msi" | "msix" | "appx" | "msixbundle" => "Installers",
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2" => "Archives",
        "jar" | "mrpack" | "mcpack" | "mcworld" | "litematic" | "schem" | "schematic" => "Minecraft",
        "rs" | "py" | "js" | "ts" | "java" | "c" | "cpp" | "h" | "cs" | "go" | "html" | "css" | "json" | "toml" | "yaml" | "yml" | "ipynb" | "sh" | "ps1" | "bat" => "Code",
        "stl" | "obj" | "fbx" | "blend" | "3mf" | "step" | "stp" | "gcode" => "3D",
        "ttf" | "otf" | "woff" | "woff2" => "Fonts",
        "torrent" => "Torrents",
        "iso" | "img" => "Disk images",
        _ if name.starts_with("screenshot") || name.starts_with("bildschirmfoto") => "Images",
        _ => "Other",
    }
}

/// Skipped: hidden files, unfinished downloads and anything touched in the last two minutes.
fn skip(path: &Path, modified: SystemTime) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    name.starts_with('.') || name.eq_ignore_ascii_case("desktop.ini") || matches!(ext.as_str(), "crdownload" | "part" | "partial" | "tmp" | "download" | "lnk" | "url") || modified.elapsed().map(|d| d < Duration::from_secs(120)).unwrap_or(true)
}

/// (size, hash of the first 64 KiB) — enough to spot the copies browsers make ("x (1).pdf").
fn fingerprint(path: &Path, size: u64) -> Option<(u64, u64)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 65536.min(size as usize)];
    f.read_exact(&mut buf).ok()?;
    let mut h: u64 = 0xcbf29ce484222325;
    for b in &buf {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    Some((size, h))
}

/// A free name in `dir` ("report.pdf", "report (2).pdf" …).
fn free_name(dir: &Path, file: &str, taken: &mut std::collections::HashSet<PathBuf>) -> PathBuf {
    let p = Path::new(file);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(file);
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    let mut candidate = dir.join(file);
    let mut n = 2;
    while candidate.exists() || taken.contains(&candidate) {
        candidate = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

pub fn plan(folder: &Path) -> Result<Plan, String> {
    let rd = std::fs::read_dir(folder).map_err(|e| format!("{} cannot be read: {e}", folder.display()))?;
    let mut files: Vec<(PathBuf, u64, SystemTime)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .filter(|(p, _, t)| !skip(p, *t))
        .collect();
    // Oldest first: the original stays, later copies count as duplicates.
    files.sort_by_key(|(_, _, t)| *t);
    let mut seen: HashMap<(u64, u64), PathBuf> = HashMap::new();
    let mut taken = std::collections::HashSet::new();
    let mut moves = Vec::new();
    for (path, size, modified) in files {
        let age = modified.elapsed().map(|d| d.as_secs() / 86_400).unwrap_or(0);
        let dup = (size > 0).then(|| fingerprint(&path, size)).flatten().is_some_and(|key| seen.insert(key, path.clone()).is_some());
        let target = if dup { "Duplicates" } else { category(&path, age) };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let to = free_name(&folder.join(target), &name, &mut taken);
        moves.push(Move { from: path, to });
    }
    Ok(Plan { folder: folder.to_path_buf(), moves })
}

/// Counts per target sub-folder.
fn summary(plan: &Plan) -> BTreeMap<String, usize> {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for m in &plan.moves {
        let dir = m.to.parent().and_then(|p| p.strip_prefix(&plan.folder).ok()).map(|p| p.display().to_string()).unwrap_or_default();
        *by.entry(dir).or_default() += 1;
    }
    by
}

fn resolve_folder(raw: &str) -> Option<PathBuf> {
    let r = raw.trim().to_lowercase();
    let known = [("download", "Downloads"), ("desktop", "Desktop"), ("schreibtisch", "Desktop"), ("document", "Documents"), ("picture", "Pictures"), ("photo", "Pictures"), ("video", "Videos"), ("music", "Music")];
    known.iter().find(|(k, _)| r.contains(k)).map(|(_, f)| user_folder(f)).or_else(|| crate::features::resolve_path(raw).filter(|p| p.is_dir()))
}

impl Janitor {
    pub fn new(manifests: PathBuf) -> Self {
        Self { manifests, pending: Arc::new(Mutex::new(None)) }
    }

    fn apply(&self, plan: &Plan) -> Result<(usize, Vec<String>), String> {
        let mut done = Vec::new();
        let mut failed = Vec::new();
        for m in &plan.moves {
            if let Some(dir) = m.to.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::rename(&m.from, &m.to) {
                Ok(()) => done.push(m.clone()),
                Err(e) => failed.push(format!("{}: {e}", m.from.file_name().and_then(|n| n.to_str()).unwrap_or("?"))),
            }
        }
        std::fs::create_dir_all(&self.manifests).map_err(|e| e.to_string())?;
        let manifest = Plan { folder: plan.folder.clone(), moves: done.clone() };
        let file = self.manifests.join(format!("cleanup_{}.json", crate::features::stamp()));
        std::fs::write(&file, serde_json::to_string_pretty(&manifest).unwrap_or_default()).map_err(|e| e.to_string())?;
        info!("[JANITOR] Moved {} files in {} (manifest {})", done.len(), plan.folder.display(), file.display());
        Ok((done.len(), failed))
    }

    fn card(&self, tools: &ToolContext, plan: &Plan, title: &str) -> String {
        let by = summary(plan);
        let mut md = format!("## {title}\n`{}`\n\n| Folder | Files |\n|---|---|\n", plan.folder.display());
        for (dir, n) in &by {
            md.push_str(&format!("| {dir} | {n} |\n"));
        }
        md.push_str("\n### Examples\n");
        for m in plan.moves.iter().take(14) {
            md.push_str(&format!("- {} → {}\n", m.from.file_name().and_then(|n| n.to_str()).unwrap_or("?"), m.to.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("?")));
        }
        show_window(tools, "file_janitor", "File Janitor", &md, 560.0, 600.0);
        md
    }

    /// `smart_folder_cleanup`: preview (default), apply the previewed plan, or plan + apply at once.
    pub async fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or("").to_lowercase();
        if action == "apply" || action == "confirm" {
            let plan = self.pending.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or("There's no cleanup preview to apply, Sir; ask me to clean up a folder first.")?;
            let (moved, failed) = self.apply(&plan)?;
            tools.focus.clear_offer();
            return Ok(spoken(
                format!("Moved {moved} files in {}.{}", plan.folder.display(), if failed.is_empty() { String::new() } else { format!(" Skipped: {}", failed.join("; ")) }),
                format!("Done, Sir. {moved} files are filed away{}. Say \"undo the cleanup\" if you want them back.", if failed.is_empty() { String::new() } else { format!(", {} were in use", failed.len()) }),
            ));
        }
        let folder = resolve_folder(str_arg(args, &["target_folder", "folder", "path"]).unwrap_or("downloads")).ok_or("I can't find that folder, Sir.")?;
        let p = tokio::task::spawn_blocking({
            let folder = folder.clone();
            move || plan(&folder)
        })
        .await
        .map_err(|e| e.to_string())??;
        if p.moves.is_empty() {
            return Ok(spoken(format!("{} is already tidy (no loose files).", folder.display()), "That folder is already tidy, Sir."));
        }
        let by = summary(&p);
        let top = by.iter().max_by_key(|(_, n)| **n).map(|(d, n)| format!("{n} into {d}")).unwrap_or_default();
        if !bool_arg(args, "dry_run_first", true) {
            let (moved, _) = self.apply(&p)?;
            self.card(tools, &p, "File Janitor — done");
            return Ok(spoken(format!("Moved {moved} files into {} folders in {}.", by.len(), folder.display()), format!("Done, Sir: {moved} files sorted into {} folders. Say \"undo the cleanup\" to reverse it.", by.len())));
        }
        let md = self.card(tools, &p, "File Janitor — preview");
        let n = p.moves.len();
        let folders = by.len();
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
        // "Yes" applies it.
        tools.focus.note_offer(crate::focus::PendingOffer { tool: "smart_folder_cleanup".into(), args: json!({ "action": "apply" }), terminate: None });
        Ok(spoken(md, format!("I'd move {n} files into {folders} folders — {top}. Shall I go ahead?")))
    }

    /// `undo_last_cleanup`.
    pub fn undo(&self) -> Result<String, String> {
        let mut manifests: Vec<PathBuf> = std::fs::read_dir(&self.manifests).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect()).unwrap_or_default();
        manifests.sort();
        let file = manifests.pop().ok_or("There's no cleanup to undo, Sir.")?;
        let plan: Plan = serde_json::from_str(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut back = 0;
        let mut missing = 0;
        for m in plan.moves.iter().rev() {
            if m.to.exists() && !m.from.exists() && std::fs::rename(&m.to, &m.from).is_ok() {
                back += 1;
            } else {
                missing += 1;
            }
        }
        // Folders the cleanup created and left empty.
        let mut dirs: Vec<PathBuf> = plan.moves.iter().filter_map(|m| m.to.parent().map(Path::to_path_buf)).collect();
        dirs.sort();
        dirs.dedup();
        for d in dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
            if let Some(parent) = d.parent().filter(|p| *p != plan.folder) {
                let _ = std::fs::remove_dir(parent);
            }
        }
        let _ = std::fs::rename(&file, file.with_extension("undone"));
        Ok(spoken(
            format!("Undid the cleanup of {}: {back} files back{}.", plan.folder.display(), if missing > 0 { format!(", {missing} had moved or been deleted since") } else { String::new() }),
            format!("Everything's back where it was, Sir{}.", if missing > 0 { format!(", except {missing} files that changed since") } else { String::new() }),
        ))
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "smart_folder_cleanup",
        "description": "File Janitor: tidy a folder (downloads, desktop, documents or a path) — classifies loose files into Images, Videos, Documents, Installers, Archives, Code, Minecraft, Duplicates … Shows a preview card first (dry_run_first, default true); action apply carries out the previewed plan. Undo with undo_last_cleanup.",
        "parameters": {"type": "object", "properties": {
            "target_folder": {"type": "string"},
            "dry_run_first": {"type": "boolean"},
            "action": {"type": "string", "enum": ["preview", "apply"]}
        }, "required": ["target_folder"]}
    }})
}

pub fn undo_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "undo_last_cleanup",
        "description": "File Janitor: move every file of the last folder cleanup back where it was.",
        "parameters": {"type": "object", "properties": {}}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_applies_and_undoes() {
        assert_eq!(category(Path::new("a.PNG"), 0), "Images");
        assert_eq!(category(Path::new("setup.exe"), 90), "Installers\\Old");
        assert_eq!(category(Path::new("pack.mrpack"), 0), "Minecraft");
        let dir = std::env::temp_dir().join(format!("jarvis_janitor_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in [("a.pdf", "same"), ("a (1).pdf", "same"), ("b.png", "img"), ("c.zip", "zip")] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        // Old enough not to be skipped as "just downloaded".
        let old = SystemTime::now() - Duration::from_secs(3600);
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let f = std::fs::File::options().write(true).open(e.path()).unwrap();
            f.set_modified(old).unwrap();
        }
        let p = plan(&dir).unwrap();
        let by = summary(&p);
        assert_eq!(by.get("Duplicates"), Some(&1), "{by:?}");
        assert_eq!(by.get("Images"), Some(&1));
        let j = Janitor::new(dir.join("_manifests"));
        let (moved, failed) = j.apply(&p).unwrap();
        assert_eq!((moved, failed.len()), (4, 0));
        assert!(dir.join("Archives").join("c.zip").exists());
        j.undo().unwrap();
        assert!(dir.join("c.zip").exists() && !dir.join("Archives").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
