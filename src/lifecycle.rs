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
            let built = project.join("target").join("release").join(if cfg!(windows) { "jarvis.exe" } else { "jai" });
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
    launch(&project.join("target").join("release").join("jarvis"), args)
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
}
