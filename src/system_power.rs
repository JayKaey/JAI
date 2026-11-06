//! Shutting down, restarting, sleeping and locking the PC on Sir's request.
//!
//! Shutdown and restart never run on the first word: the brain asks "Sir, please confirm …",
//! shows a crimson confirmation card and waits up to 30 s for a clear "yes" (or the card's
//! button). Only then Jarvis says goodbye and, 1.2 s later (the goodbye is heard), runs
//! `shutdown /s /t 0` or `shutdown /r /t 0` hidden. Sleep and lock are harmless and run at once.
//! "Shut down" on its own still quits Jarvis himself (`intents::SHUTDOWN`); only the PC named as
//! the object ("my PC", "the computer", "den Rechner") reaches this module.

use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;

/// How long a shutdown or restart question stays open.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(30);
/// Between the goodbye and the shutdown command, so the goodbye is heard.
pub const FAREWELL_DELAY: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Restart,
    Sleep,
    Lock,
}

impl PowerAction {
    /// Shutdown and restart end Sir's session: they are confirmed first.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::Shutdown | Self::Restart)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Restart => "restart",
            Self::Sleep => "sleep",
            Self::Lock => "lock",
        }
    }

    /// The double-check question.
    pub fn question(self) -> &'static str {
        match self {
            Self::Restart => "Sir, please confirm: would you like me to restart your PC right now?",
            _ => "Sir, please confirm: would you like me to shut down your PC right now?",
        }
    }

    /// The confirmation card's main button.
    pub fn card_label(self) -> &'static str {
        match self {
            Self::Restart => "Confirm PC Restart",
            _ => "Confirm PC Shutdown",
        }
    }

    /// What Jarvis says as he carries it out.
    pub fn farewell(self) -> &'static str {
        match self {
            Self::Shutdown => "Shutting down all systems now. Goodbye, Sir.",
            Self::Restart => "Restarting your PC now. I'll be back with it, Sir.",
            Self::Sleep => "Putting your PC to sleep, Sir.",
            Self::Lock => "Locking your PC, Sir.",
        }
    }

    /// What Jarvis says when Sir says no or the question times out.
    pub fn aborted(self) -> &'static str {
        match self {
            Self::Restart => "Restart aborted, Sir.",
            _ => "Shutdown aborted, Sir.",
        }
    }
}

/// Lower-case words, punctuation gone, umlauts kept.
fn words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The PC itself as the object of the sentence.
const PC: &str = r"(?:my |the |this |your |our |den |meinen |meinem |dem |mein |deinen )?(?:pc|computer|laptop|rechner|machine|desktop|workstation|gaming pc|gaming rig|rig)";

static SHUTDOWN_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\b(?:shut\s*down|shutdown|turn off|switch off|power off|power down)\s+{PC}\b|\b{PC}\s+(?:shut\s*down|shutdown|herunterfahren|runterfahren|ausschalten|abschalten)\b|\b(?:fahr|fahre)\s+{PC}\s+(?:herunter|runter)\b|\b(?:schalt|schalte|mach|mache)\s+{PC}\s+aus\b"
    ))
    .expect("valid shutdown regex")
});
static RESTART_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\b(?:restart|reboot)\s+{PC}\b|\b{PC}\s+(?:neu starten|neustarten|neu booten|rebooten)\b|\b(?:starte|start|boote)\s+{PC}\s+neu\b|\bneustart (?:des|vom) (?:pcs?|computers?|rechners?)\b"
    ))
    .expect("valid restart regex")
});
static SLEEP_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\b(?:put|send|set)\s+{PC}\s+(?:to|into|in)\s+(?:sleep|standby|sleep mode)\b|\b(?:sleep|suspend|hibernate)\s+{PC}\b|\b{PC}\s+(?:in den )?(?:ruhezustand|standby|energiesparmodus|schlafmodus)\b|\b(?:schick|schicke)\s+{PC}\s+schlafen\b"
    ))
    .expect("valid sleep regex")
});
static LOCK_PC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\block\s+(?:{PC}|(?:my |the )?screen)\b|\b(?:{PC}|(?:den )?bildschirm)\s+sperren\b|\b(?:sperr|sperre)\s+(?:{PC}|(?:den )?bildschirm)\b"
    ))
    .expect("valid lock regex")
});
static NEGATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:don't|dont|do not|never|nicht|nie|kein|keine|without)\b").expect("valid negation regex"));
/// "How do I shut down my PC?" asks for an explanation, not for the deed.
static EXPLANATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:how|why|what|when|wie|warum|wieso|was|wann)\b").expect("valid explanation regex"));

/// The power action Sir asks for, if any ("shut down my PC", "fahre meinen PC herunter",
/// "restart the computer", "put my PC to sleep", "lock my screen").
pub fn power_request(text: &str) -> Option<PowerAction> {
    let s = words(text);
    if s.is_empty() || NEGATED.is_match(&s) || EXPLANATION.is_match(&s) {
        return None;
    }
    if RESTART_PC.is_match(&s) {
        return Some(PowerAction::Restart);
    }
    if SHUTDOWN_PC.is_match(&s) {
        return Some(PowerAction::Shutdown);
    }
    if SLEEP_PC.is_match(&s) {
        return Some(PowerAction::Sleep);
    }
    if LOCK_PC.is_match(&s) {
        return Some(PowerAction::Lock);
    }
    None
}

/// "Yes", "ja", "confirm", "do it", "shut it down", "yes please", "mach es": the go-ahead.
pub fn is_confirmation(text: &str) -> bool {
    let s = words(text);
    if s.is_empty() || is_cancellation(text) {
        return false;
    }
    const WHOLE: &[&str] = &[
        "yes", "yeah", "yep", "yup", "ja", "jawohl", "jo", "confirm", "confirmed", "i confirm", "do it", "go ahead", "go", "yes please", "yes do it",
        "please do", "shut it down", "restart it", "reboot it", "mach es", "mach das", "mach", "machs", "mach's", "tu es", "bestätigen", "bestätige",
        "bestaetigen", "ok", "okay", "sure", "affirmative", "absolutely", "of course", "klar", "genau", "los", "ja bitte", "ja mach", "ja mach es",
        "proceed", "yes sir", "do that", "make it so", "engage", "execute", "yes shut it down", "yes restart it", "ja fahr ihn runter",
    ];
    let trimmed = s.trim_start_matches("jarvis ").trim_end_matches(" jarvis").trim();
    WHOLE.contains(&trimmed)
        || trimmed.starts_with("confirm")
        || trimmed.starts_with("yes ")
        || trimmed.starts_with("ja ")
        || trimmed.starts_with("ok ")
        || trimmed.starts_with("okay ")
}

/// "No", "cancel", "stop", "abbrechen", "wait": the question is withdrawn.
pub fn is_cancellation(text: &str) -> bool {
    let s = words(text);
    static CANCEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:no|nope|nah|nein|cancel|cancelled|stop|stopp|abort|abbrechen|abbruch|halt|wait|warte|never mind|nevermind|forget it|vergiss es|don't|dont|do not|nicht|lass es|lieber nicht)\b")
            .expect("valid cancel regex")
    });
    CANCEL.is_match(&s)
}

/// Carries the action out (after the confirmation, where one is needed).
pub async fn execute(action: PowerAction) -> Result<String, String> {
    imp::execute(action).await
}

#[cfg(windows)]
mod imp {
    use super::PowerAction;
    use std::path::PathBuf;
    use std::time::Duration;

    fn shutdown_exe() -> PathBuf {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join("System32").join("shutdown.exe")
    }

    /// `shutdown.exe` hidden (CREATE_NO_WINDOW, no busy cursor) with `args`.
    async fn shutdown(args: &[&str]) -> Result<(), String> {
        let out = crate::shell::run_helper(&shutdown_exe(), args, None, &[], Duration::from_secs(10)).await.map_err(|e| format!("shutdown.exe could not start: {e}"))?;
        if out.success() {
            Ok(())
        } else {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if why.is_empty() { format!("shutdown.exe ended with code {:?}", out.code) } else { why })
        }
    }

    fn lock_pc() -> Result<(), String> {
        #[link(name = "user32")]
        extern "system" {
            fn LockWorkStation() -> i32;
        }
        // SAFETY: documented parameterless API acting on this interactive session.
        if unsafe { LockWorkStation() } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    fn sleep_pc() -> Result<(), String> {
        #[link(name = "PowrProf")]
        extern "system" {
            fn SetSuspendState(hibernate: u8, force: u8, disable_wake_events: u8) -> u8;
        }
        // SAFETY: documented BOOLEAN arguments: suspend (not hibernate), no forced close.
        if unsafe { SetSuspendState(0, 0, 0) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    pub async fn execute(action: PowerAction) -> Result<String, String> {
        match action {
            PowerAction::Shutdown => shutdown(&["/s", "/t", "0"]).await.map(|_| "The PC is shutting down.".to_string()),
            PowerAction::Restart => shutdown(&["/r", "/t", "0"]).await.map(|_| "The PC is restarting.".to_string()),
            PowerAction::Sleep => tokio::task::spawn_blocking(sleep_pc).await.map_err(|e| e.to_string())?.map(|_| "The PC went to sleep.".to_string()),
            PowerAction::Lock => tokio::task::spawn_blocking(lock_pc).await.map_err(|e| e.to_string())?.map(|_| "The PC is locked.".to_string()),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PowerAction;
    use std::path::Path;
    use std::time::Duration;

    async fn run(program: &str, args: &[&str]) -> Result<(), String> {
        let out = crate::shell::run_helper(Path::new(program), args, None, &[], Duration::from_secs(10)).await.map_err(|e| format!("{program}: {e}"))?;
        if out.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub async fn execute(action: PowerAction) -> Result<String, String> {
        match action {
            PowerAction::Shutdown => run("systemctl", &["poweroff"]).await.map(|_| "The PC is shutting down.".to_string()),
            PowerAction::Restart => run("systemctl", &["reboot"]).await.map(|_| "The PC is restarting.".to_string()),
            PowerAction::Sleep => run("systemctl", &["suspend"]).await.map(|_| "The PC went to sleep.".to_string()),
            PowerAction::Lock => run("loginctl", &["lock-session"]).await.map(|_| "The PC is locked.".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_in_english_and_german() {
        for t in ["Jarvis, shut down my PC", "Turn off the computer", "Fahre meinen PC herunter", "shutdown my pc please", "schalte den Rechner aus", "PC herunterfahren"] {
            assert_eq!(power_request(t), Some(PowerAction::Shutdown), "{t}");
        }
        for t in ["Restart my PC", "reboot the computer", "Starte den PC neu", "Rechner neu starten"] {
            assert_eq!(power_request(t), Some(PowerAction::Restart), "{t}");
        }
        assert_eq!(power_request("put my pc to sleep"), Some(PowerAction::Sleep));
        assert_eq!(power_request("PC in den Ruhezustand"), Some(PowerAction::Sleep));
        assert_eq!(power_request("lock my screen"), Some(PowerAction::Lock));
        assert_eq!(power_request("sperre den PC"), Some(PowerAction::Lock));
    }

    #[test]
    fn not_requests() {
        for t in ["shut down", "close yourself", "don't shut down my PC", "how do I shut down my PC?", "turn down the volume", "shut down Discord", "restart Spotify"] {
            assert_eq!(power_request(t), None, "{t}");
        }
    }

    #[test]
    fn confirmations_and_cancellations() {
        for t in ["Yes", "Ja", "Confirm", "Do it", "Shut it down", "Yes please", "Mach es", "confirm pc shutdown", "Yes, do it."] {
            assert!(is_confirmation(t), "{t}");
        }
        for t in ["No", "Cancel", "Stop", "Abbrechen", "no, don't", "wait", "cancel pc shutdown"] {
            assert!(is_cancellation(t), "{t}");
            assert!(!is_confirmation(t), "{t}");
        }
        assert!(!is_confirmation("what's the weather"));
    }

    #[test]
    fn only_shutdown_and_restart_are_double_checked() {
        assert!(PowerAction::Shutdown.needs_confirmation() && PowerAction::Restart.needs_confirmation());
        assert!(!PowerAction::Sleep.needs_confirmation() && !PowerAction::Lock.needs_confirmation());
    }
}
