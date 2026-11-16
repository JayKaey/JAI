//! JAI's own lifecycle beyond quitting: sleep mode (ignores everything but "wake up") and
//! restarting into the same or a freshly compiled binary once the shutdown sequence has run.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::{info, warn};

static SLEEPING: AtomicBool = AtomicBool::new(false);
static RESTART: Mutex<Option<Restart>> = Mutex::new(None);
static HOOK: OnceLock<Box<dyn Fn(Signal) + Send + Sync>> = OnceLock::new();

/// Background work (a self-rebuild) reaching the app after its tool call has returned.
#[derive(Debug, Clone)]
pub enum Signal {
    /// Shown in the HUD.
    Notice(String),
    /// Shown and, unless Sir chose silence, spoken.
    Alert(String),
    /// Quit (after the alert above has been spoken); a pending restart then runs.
    Shutdown,
}

/// Installed once by the app.
pub fn set_hook(hook: impl Fn(Signal) + Send + Sync + 'static) {
    let _ = HOOK.set(Box::new(hook));
}

pub fn signal(s: Signal) {
    match HOOK.get() {
        Some(hook) => hook(s),
        None => warn!("[LIFE] No app hook for {s:?}"),
    }
}

/// What to start once this process has shut down.
#[derive(Debug, Clone)]
pub enum Restart {
    /// The running executable again.
    Same,
    /// A new build in `target/release`; on Windows it is packaged as `JAI.exe` first when
    /// that is what Sir started.
    Rebuilt { project: PathBuf },
}

pub fn is_sleeping() -> bool {
    SLEEPING.load(Ordering::Acquire)
}

pub fn set_sleeping(sleeping: bool) -> bool {
    let was = SLEEPING.swap(sleeping, Ordering::AcqRel);
    if was != sleeping {
        info!("[LIFE] {}", if sleeping { "Sleep mode on" } else { "Awake" });
    }
    was != sleeping
}

pub fn request_restart(plan: Restart) {
    info!("[LIFE] Restart requested: {plan:?}");
    *RESTART.lock().unwrap_or_else(|e| e.into_inner()) = Some(plan);
}

/// "Wake up", "wach auf", "I'm back" — the only things JAI reacts to while asleep.
pub fn is_wake_up_phrase(text: &str) -> bool {
    let t = text.to_lowercase();
    ["wake up", "wakey", "wach auf", "wach wieder auf", "aufwachen", "i'm back", "im back", "i am back", "bin wieder da", "good morning", "guten morgen", "come back online", "stand up"]
        .iter()
        .any(|p| t.contains(p))
}

/// Starts the follow-up process (called by `main` right before it exits).
pub fn spawn_pending_restart() {
    let Some(plan) = RESTART.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if let Err(e) = spawn(&plan) {
        warn!("[LIFE] Could not restart ({plan:?}): {e}");
    }
}

// ------------------------------------------------------- the last version that ran well ----
// A packaged JAI.exe counts its starts in `jai_starts.txt` beside it, for its own build only
// (length and write time). A start that runs `HEALTHY_AFTER` clears the count and keeps a copy
// of its exe as JAI.prev.exe; quitting clears it too. With two starts of this build that never
// got going, the next hands over to that copy, and keeps doing so until JAI.exe is replaced.

const STARTS_FILE: &str = "jai_starts.txt";
const PREV_EXE: &str = "JAI.prev.exe";
pub const HEALTHY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// Only the packaged JAI.exe counts (a dev build or the copy itself does not).
fn packaged_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().filter(|exe| exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case("JAI.exe")))
}

/// A build's stamp: its length and write time.
fn build_stamp(exe: &std::path::Path) -> String {
    std::fs::metadata(exe)
        .map(|m| format!("{}-{}", m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())))
        .unwrap_or_default()
}

/// (failed starts before this one, the count's file text for this start, hand over).
pub fn tally(file_text: &str, stamp: &str, copy_exists: bool) -> (u32, Option<String>, bool) {
    let mut parts = file_text.split_whitespace();
    let before = match (parts.next().and_then(|n| n.parse::<u32>().ok()), parts.next()) {
        (Some(n), Some(s)) if s == stamp => n,
        _ => 0,
    };
    let hand_over = before >= 2 && copy_exists;
    (before, (!hand_over).then(|| format!("{} {stamp}", before + 1)), hand_over)
}

/// Counts this start; the copy to start instead when this build failed twice.
pub fn count_start() -> Option<PathBuf> {
    let exe = packaged_exe()?;
    let dir = exe.parent()?;
    let (file, prev) = (dir.join(STARTS_FILE), dir.join(PREV_EXE));
    let (before, write, hand_over) = tally(&std::fs::read_to_string(&file).unwrap_or_default(), &build_stamp(&exe), prev.is_file());
    if let Some(text) = write {
        let _ = std::fs::write(&file, text);
    }
    if hand_over {
        warn!("[LIFE] This build did not get going {before} times: handing over to {}", prev.display());
    }
    hand_over.then_some(prev)
}

pub fn launch_fallback(prev: &std::path::Path) -> Result<(), String> {
    launch(prev, &["--fallback".to_string()])
}

pub fn clear_start_count() {
    if let Some(dir) = packaged_exe().as_deref().and_then(std::path::Path::parent) {
        let _ = std::fs::remove_file(dir.join(STARTS_FILE));
    }
}

/// This start went well: the count goes, and this build becomes the one to fall back to.
pub fn start_went_well() {
    clear_start_count();
    let Some(exe) = packaged_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let (prev, part) = (dir.join(PREV_EXE), dir.join(format!("{PREV_EXE}.part")));
    if build_stamp(&prev) == build_stamp(&exe) {
        return;
    }
    match std::fs::copy(&exe, &part).and_then(|_| std::fs::rename(&part, &prev)) {
        Ok(()) => info!("[LIFE] This build ran well: kept as {}", prev.display()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            warn!("[LIFE] Could not keep this build as {}: {e}", prev.display());
        }
    }
}

fn current_args() -> Vec<String> {
    std::env::args().skip(1).filter(|a| a != "--restarted").collect()
}

fn spawn(plan: &Restart) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut args = current_args();
    args.push("--restarted".into());
    match plan {
        Restart::Same => launch(&exe, &args),
        Restart::Rebuilt { project } => {
            let built = project.join("target").join("release").join(if cfg!(windows) { "jai.exe" } else { "jai" });
            if !built.exists() {
                return Err(format!("{} does not exist", built.display()));
            }
            let packaged = project.join("JAI.exe");
            let started_packaged = cfg!(windows) && exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case("JAI.exe"));
            if started_packaged {
                // JAI.exe is locked while it runs: a hidden helper waits for this process to
                // end, copies the new build over it (tools/package.ps1) and starts it.
                package_after_exit(project, &packaged, &args)
            } else {
                launch(&built, &args)
            }
        }
    }
}

fn launch(exe: &std::path::Path, args: &[String]) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    if let Ok(dir) = std::env::current_dir() {
        cmd.current_dir(dir);
    }
    crate::shell::hide_console_std(&mut cmd);
    cmd.spawn().map_err(|e| format!("{}: {e}", exe.display()))?;
    info!("[LIFE] Started {} {}", exe.display(), args.join(" "));
    Ok(())
}

#[cfg(windows)]
fn package_after_exit(project: &std::path::Path, packaged: &std::path::Path, args: &[String]) -> Result<(), String> {
    let pid = std::process::id();
    let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let arg_list = if args.is_empty() {
        String::new()
    } else {
        format!(" -ArgumentList {}", args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(","))
    };
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 200 }}; \
         & powershell -NoProfile -ExecutionPolicy Bypass -File {package}; \
         Start-Process -FilePath {exe} -WorkingDirectory {dir}{arg_list}",
        package = quote(&project.join("tools").join("package.ps1").display().to_string()),
        exe = quote(&packaged.display().to_string()),
        dir = quote(&project.display().to_string()),
    );
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]);
    crate::shell::hide_console_std(&mut cmd);
    cmd.spawn().map_err(|e| e.to_string())?;
    info!("[LIFE] Packaging the new build into {} after exit", packaged.display());
    Ok(())
}

#[cfg(not(windows))]
fn package_after_exit(project: &std::path::Path, _packaged: &std::path::Path, args: &[String]) -> Result<(), String> {
    launch(&project.join("target").join("release").join("jai"), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_up_phrases() {
        assert!(is_wake_up_phrase("JAI, wake up."));
        assert!(is_wake_up_phrase("Wach auf, JAI"));
        assert!(!is_wake_up_phrase("What's the weather?"));
    }

    #[test]
    fn two_starts_that_never_got_going_hand_over_to_the_last_good_build() {
        let stamp = "104857600-1789000000";
        assert_eq!(tally("", stamp, true), (0, Some(format!("1 {stamp}")), false), "the first start");
        assert_eq!(tally(&format!("1 {stamp}"), stamp, true), (1, Some(format!("2 {stamp}")), false), "one failure is not enough");
        assert_eq!(tally(&format!("2 {stamp}"), stamp, true), (2, None, true), "the third start hands over and the count stays");
        assert_eq!(tally(&format!("2 {stamp}"), stamp, false), (2, Some(format!("3 {stamp}")), false), "no copy: nothing to hand over to");
        assert_eq!(tally("5 104857600-1700000000", stamp, true), (0, Some(format!("1 {stamp}")), false), "a new build starts its own count");
        assert_eq!(tally("garbage", stamp, true).0, 0);

        // The copy keeps the build's stamp, so a well-run build is copied once, not on every start.
        let dir = std::env::temp_dir().join(format!("jai_prev_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let (exe, copy) = (dir.join("JAI.exe"), dir.join(PREV_EXE));
        std::fs::write(&exe, vec![1u8; 4096]).expect("exe");
        std::fs::copy(&exe, &copy).expect("copy");
        assert_eq!(build_stamp(&copy), build_stamp(&exe));
        assert!(!build_stamp(&exe).is_empty() && build_stamp(&dir.join("missing.exe")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
