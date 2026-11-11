//! Self-improvement: JAI reads his own Rust source, applies changes behind a safety net and
//! rebuilds himself.
//!
//! * `inspect_own_source`: list `src/*.rs`, read a file (with line numbers) or grep the tree.
//! * `patch_own_source`: exact find/replace, a line range or a whole (new) file. The original is
//!   copied to `./backups/<timestamp>/` first, `cargo check` must pass, and a failing change is
//!   rolled back automatically, so a bad patch can never leave the tree broken.
//! * `restore_source_backup`: puts the newest (or a named) backup back.
//! * `recompile_and_restart`: `cargo build --release` in the background; on success JAI
//!   announces it, shuts down and the new binary starts (`lifecycle::Restart::Rebuilt`).
//!
//! The cargo environment mirrors `build.bat` (its PATH and LIBCLANG_PATH lines are read).

use crate::lifecycle::{self, Restart, Signal};
use crate::tools::str_arg;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const READ_LIMIT_LINES: usize = 400;
const READ_LIMIT_CHARS: usize = 14_000;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(45 * 60);

static BUILDING: AtomicBool = AtomicBool::new(false);

/// The folder with `Cargo.toml` and `src/` (JAI runs from his project folder).
pub fn project_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut dir = Some(cwd.as_path());
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("src").join("main.rs").is_file() {
            return Ok(d.to_path_buf());
        }
        dir = d.parent();
    }
    Err("My source code is not on this machine (no Cargo.toml and src/ next to JARVIS.exe), so I cannot change or rebuild myself.".into())
}

/// `src/brain.rs`, `brain.rs`, `brain` → the file inside the project; nothing outside `src/`
/// (plus `Cargo.toml`) is ever touched.
fn resolve_source(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim().replace('\\', "/");
    if raw.eq_ignore_ascii_case("cargo.toml") {
        return Ok(root.join("Cargo.toml"));
    }
    let rel = raw.trim_start_matches("./").trim_start_matches("src/");
    let rel = if rel.ends_with(".rs") { rel.to_string() } else { format!("{rel}.rs") };
    let path = Path::new(&rel);
    if path.is_absolute() || path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("'{raw}' is outside my source tree."));
    }
    Ok(root.join("src").join(path))
}

fn list_sources(root: &Path) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let lines = std::fs::read_to_string(&p).map(|t| t.lines().count()).unwrap_or(0);
                let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string().replace('\\', "/");
                out.push((rel, lines));
            }
        }
    }
    out.sort();
    out
}

/// `inspect_own_source`.
pub fn inspect(args: &Value) -> Result<String, String> {
    let root = project_root()?;
    if let Some(query) = str_arg(args, &["query", "search", "grep"]) {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for (rel, _) in list_sources(&root) {
            let Ok(text) = std::fs::read_to_string(root.join(&rel)) else { continue };
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    hits.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                    if hits.len() >= 60 {
                        break;
                    }
                }
            }
        }
        return Ok(if hits.is_empty() { format!("No line in src/ contains '{query}'.") } else { hits.join("\n") });
    }
    let Some(file) = str_arg(args, &["file", "path", "module"]) else {
        let files = list_sources(&root);
        let total: usize = files.iter().map(|(_, n)| n).sum();
        let listing = files.iter().map(|(f, n)| format!("{f} ({n} lines)")).collect::<Vec<_>>().join("\n");
        return Ok(format!("My source tree ({} files, {total} lines of Rust) at {}:\n{listing}", files.len(), root.display()));
    };
    let path = resolve_source(&root, file)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let total = text.lines().count();
    let start = args.get("start_line").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let max = args.get("max_lines").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(READ_LIMIT_LINES).clamp(1, READ_LIMIT_LINES);
    let mut out = format!("{} (lines {start}-{} of {total}):\n", path.strip_prefix(&root).unwrap_or(&path).display(), (start + max - 1).min(total));
    for (i, line) in text.lines().enumerate().skip(start - 1).take(max) {
        out.push_str(&format!("{:>5} | {line}\n", i + 1));
        if out.len() > READ_LIMIT_CHARS {
            out.push_str("… (truncated; read on with start_line)\n");
            break;
        }
    }
    Ok(out)
}

fn backup(root: &Path, files: &[PathBuf]) -> Result<PathBuf, String> {
    let dir = root.join("backups").join(chrono::Local::now().format("%Y%m%d_%H%M%S").to_string());
    for f in files.iter().filter(|f| f.exists()) {
        let rel = f.strip_prefix(root).unwrap_or(f);
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(f, &dest).map_err(|e| format!("backing up {}: {e}", f.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn restore(root: &Path, backup_dir: &Path) -> Result<Vec<String>, String> {
    let mut restored = Vec::new();
    let mut stack = vec![backup_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(backup_dir).map_err(|e| e.to_string())?;
            std::fs::copy(&p, root.join(rel)).map_err(|e| e.to_string())?;
            restored.push(rel.display().to_string().replace('\\', "/"));
        }
    }
    Ok(restored)
}

/// `patch_own_source`: back up, edit, `cargo check`, roll back on failure.
pub async fn patch(args: &Value) -> Result<String, String> {
    let root = project_root()?;
    let file = str_arg(args, &["file", "path"]).ok_or("patch_own_source needs 'file'.")?;
    let path = resolve_source(&root, file)?;
    let original = std::fs::read_to_string(&path).ok();
    let reason = str_arg(args, &["reason", "description"]).unwrap_or("self-improvement");

    let updated = if let Some(find) = args.get("find").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let text = original.as_deref().ok_or_else(|| format!("{file} does not exist."))?;
        let replace = args.get("replace").and_then(Value::as_str).unwrap_or("");
        match text.matches(find).count() {
            0 => return Err(format!("The text to replace was not found in {file}. Read the file first (inspect_own_source) and copy it exactly.")),
            1 => text.replacen(find, replace, 1),
            n => return Err(format!("The text to replace occurs {n} times in {file}; include more surrounding lines so it is unique.")),
        }
    } else if let (Some(start), Some(end)) = (args.get("start_line").and_then(Value::as_u64), args.get("end_line").and_then(Value::as_u64)) {
        let text = original.as_deref().ok_or_else(|| format!("{file} does not exist."))?;
        let lines: Vec<&str> = text.lines().collect();
        let (start, end) = (start.max(1) as usize, end as usize);
        if start > end || end > lines.len() {
            return Err(format!("Lines {start}-{end} are outside {file} (it has {} lines).", lines.len()));
        }
        let new_text = args.get("new_text").and_then(Value::as_str).unwrap_or("");
        let mut out: Vec<&str> = lines[..start - 1].to_vec();
        out.extend(new_text.lines());
        out.extend(&lines[end..]);
        out.join("\n") + "\n"
    } else if let Some(content) = args.get("content").and_then(Value::as_str) {
        content.to_string()
    } else {
        return Err("patch_own_source needs either find/replace, start_line/end_line/new_text, or content.".into());
    };
    if Some(updated.as_str()) == original.as_deref() {
        return Err("That change would not alter the file.".into());
    }

    let backup_dir = backup(&root, std::slice::from_ref(&path))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &updated).map_err(|e| format!("writing {}: {e}", path.display()))?;
    info!("[SELF] Patched {} ({reason}); backup in {}", path.display(), backup_dir.display());

    let check = cargo(&root, &["check", "--message-format", "short"], CHECK_TIMEOUT).await;
    match check {
        Ok(out) => Ok(format!(
            "Patched {file} ({reason}). `cargo check` passed{}. Backup: {}. Call recompile_and_restart to run the new code.",
            warnings_note(&out),
            backup_dir.strip_prefix(&root).unwrap_or(&backup_dir).display()
        )),
        Err(errors) => {
            // Roll back: the tree must always compile.
            match &original {
                Some(text) => std::fs::write(&path, text).map_err(|e| e.to_string())?,
                None => std::fs::remove_file(&path).map_err(|e| e.to_string())?,
            }
            warn!("[SELF] cargo check failed; {file} rolled back");
            Err(format!("`cargo check` failed, so I rolled the change back. Compiler output:\n{errors}"))
        }
    }
}

fn warnings_note(out: &str) -> String {
    let n = out.lines().filter(|l| l.contains("warning:")).count();
    if n == 0 { String::new() } else { format!(" with {n} warning(s)") }
}

/// `restore_source_backup`.
pub fn restore_backup(args: &Value) -> Result<String, String> {
    let root = project_root()?;
    let backups = root.join("backups");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&backups)
        .map_err(|_| "There are no source backups yet.".to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let chosen = match str_arg(args, &["backup", "id", "name"]) {
        Some(name) => dirs.iter().find(|d| d.file_name().is_some_and(|n| n.to_string_lossy() == name)).cloned(),
        None => dirs.last().cloned(),
    }
    .ok_or("No matching source backup.")?;
    let files = restore(&root, &chosen)?;
    Ok(format!("Restored {} from backup {}.", files.join(", "), chosen.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))
}

/// `recompile_and_restart`: builds in the background, then restarts into the new binary.
pub fn recompile_and_restart(args: &Value) -> Result<String, String> {
    let root = project_root()?;
    let restart = crate::tools::bool_arg(args, "restart").unwrap_or(true);
    if BUILDING.swap(true, Ordering::AcqRel) {
        return Err("A rebuild is already running.".into());
    }
    let shown = root.display().to_string();
    tokio::spawn(async move {
        let started = Instant::now();
        lifecycle::signal(Signal::Notice("Rebuilding J.A.I. (cargo build --release)…".into()));
        let renamed = free_running_binary(&root);
        let result = cargo(&root, &["build", "--release", "--message-format", "short"], BUILD_TIMEOUT).await;
        BUILDING.store(false, Ordering::Release);
        match result {
            Ok(_) => {
                let secs = started.elapsed().as_secs();
                info!("[SELF] Release build finished in {secs} s");
                if restart {
                    lifecycle::request_restart(Restart::Rebuilt { project: root.clone() });
                    lifecycle::signal(Signal::Alert("The new build is ready, Sir. Restarting now.".into()));
                    lifecycle::signal(Signal::Shutdown);
                } else {
                    lifecycle::signal(Signal::Alert(format!("The new build compiled successfully in {secs} seconds, Sir.")));
                }
            }
            Err(errors) => {
                if let Some((current, moved)) = renamed {
                    let _ = std::fs::rename(moved, current);
                }
                warn!("[SELF] Release build failed:\n{errors}");
                lifecycle::signal(Signal::Alert("I'm afraid the rebuild failed, Sir. The details are in the log.".into()));
                lifecycle::signal(Signal::Notice(format!("Build failed: {}", errors.chars().take(600).collect::<String>())));
            }
        }
    });
    Ok(format!(
        "Started `cargo build --release` in {shown} in the background. {}",
        if restart { "When it succeeds I will announce it and restart into the new version." } else { "I will report when it is done." }
    ))
}

/// Windows cannot overwrite a running executable but can rename it: when JAI runs from
/// `target/release`, his binary moves aside so the linker can write the new one.
fn free_running_binary(root: &Path) -> Option<(PathBuf, PathBuf)> {
    if !cfg!(windows) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let release = root.join("target").join("release");
    if exe.parent() != Some(release.as_path()) {
        return None;
    }
    let moved = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&moved);
    std::fs::rename(&exe, &moved).ok()?;
    Some((exe, moved))
}

static BAT_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?im)^\s*set\s+"PATH=([^"]*?);?%PATH%""#).expect("valid regex"));
static BAT_LIBCLANG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?im)set\s+"LIBCLANG_PATH=([^"]+)""#).expect("valid regex"));

fn expand_percent(s: &str) -> String {
    let mut out = s.to_string();
    for (k, v) in std::env::vars() {
        out = out.replace(&format!("%{k}%"), &v);
    }
    out
}

fn cargo_binary() -> String {
    if crate::apps::which("cargo").is_some() {
        return "cargo".into();
    }
    let home = crate::apps::home_dir().unwrap_or_default();
    let candidate = home.join(".cargo").join("bin").join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
    if candidate.is_file() {
        candidate.display().to_string()
    } else {
        "cargo".into()
    }
}

/// Runs cargo with build.bat's toolchain environment; Ok(output) on success, Err(errors) otherwise.
async fn cargo(root: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut envs: Vec<(String, String)> = Vec::new();
    let mut path_prefix: Vec<String> = Vec::new();
    if let Some(home) = crate::apps::home_dir() {
        path_prefix.push(home.join(".cargo").join("bin").display().to_string());
    }
    if let Ok(bat) = std::fs::read_to_string(root.join("build.bat")) {
        if let Some(c) = BAT_PATH.captures(&bat) {
            path_prefix.extend(expand_percent(&c[1]).split(';').filter(|p| !p.trim().is_empty()).map(String::from));
        }
        if std::env::var_os("LIBCLANG_PATH").is_none() {
            if let Some(c) = BAT_LIBCLANG.captures(&bat) {
                envs.push(("LIBCLANG_PATH".into(), expand_percent(&c[1])));
            }
        }
    }
    let sep = if cfg!(windows) { ";" } else { ":" };
    let path = std::env::var("PATH").unwrap_or_default();
    envs.push(("PATH".into(), format!("{}{sep}{path}", path_prefix.join(sep))));
    info!("[SELF] cargo {}", args.join(" "));
    let output = match crate::shell::run_helper(Path::new(&cargo_binary()), args, Some(root), &envs, timeout).await {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(format!("cargo {} timed out after {} minutes", args.join(" "), timeout.as_secs() / 60)),
        Err(e) => return Err(format!("could not run cargo: {e}")),
    };
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    if output.success() {
        Ok(text)
    } else {
        let errors: Vec<&str> = text.lines().filter(|l| l.contains("error") || l.trim_start().starts_with("-->")).take(40).collect();
        Err(if errors.is_empty() { text.chars().rev().take(3000).collect::<String>().chars().rev().collect() } else { errors.join("\n") })
    }
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "inspect_own_source",
            "description": "Read your own Rust source code: no arguments lists src/*.rs; 'file' reads one (with line numbers, use start_line to page); 'query' searches all files.",
            "parameters": {"type": "object", "properties": {
                "file": {"type": "string", "description": "e.g. src/gui.rs"},
                "query": {"type": "string"},
                "start_line": {"type": "integer"}, "max_lines": {"type": "integer"}
            }, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "patch_own_source",
            "description": "Improve your own source code when Sir asks you to improve or extend yourself. Give 'find' (exact existing text, unique) + 'replace', or start_line/end_line/new_text, or 'content' for a new file. A backup goes to ./backups, `cargo check` must pass, otherwise the change is rolled back.",
            "parameters": {"type": "object", "properties": {
                "file": {"type": "string"}, "find": {"type": "string"}, "replace": {"type": "string"},
                "start_line": {"type": "integer"}, "end_line": {"type": "integer"}, "new_text": {"type": "string"},
                "content": {"type": "string"}, "reason": {"type": "string"}
            }, "required": ["file", "reason"]}
        }}),
        json!({"type": "function", "function": {
            "name": "restore_source_backup",
            "description": "Undo your last source change by restoring the newest backup (or a named one).",
            "parameters": {"type": "object", "properties": {"backup": {"type": "string"}}, "required": []}
        }}),
        json!({"type": "function", "function": {
            "name": "recompile_and_restart",
            "description": "Rebuild yourself (cargo build --release, in the background) and restart into the new version when it succeeds.",
            "parameters": {"type": "object", "properties": {"restart": {"type": "boolean", "description": "default true"}}, "required": []}
        }}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_paths_stay_inside_src() {
        let root = Path::new("/project");
        assert_eq!(resolve_source(root, "src/brain.rs").unwrap(), root.join("src").join("brain.rs"));
        assert_eq!(resolve_source(root, "brain").unwrap(), root.join("src").join("brain.rs"));
        assert_eq!(resolve_source(root, "Cargo.toml").unwrap(), root.join("Cargo.toml"));
        assert!(resolve_source(root, "../secrets.rs").is_err());
        assert!(resolve_source(root, "src/../../x.rs").is_err());
    }

    #[test]
    fn build_bat_environment_is_parsed() {
        let bat = r#"set "PATH=%USERPROFILE%\.cargo\bin;C:\CMake\bin;%PATH%"
if not defined LIBCLANG_PATH set "LIBCLANG_PATH=D:\clang\native""#;
        assert_eq!(&BAT_PATH.captures(bat).unwrap()[1], r"%USERPROFILE%\.cargo\bin;C:\CMake\bin");
        assert_eq!(&BAT_LIBCLANG.captures(bat).unwrap()[1], r"D:\clang\native");
    }
}
